//! crates/ramaria-service/src/vision/batch.rs - Ramaria 图片理解批量处理模块
//!
//! 设计特点:
//! - 批量导入与"继续处理"共用单一逐行处理实现（`understand_attachment_rows`）
//! - 门禁未过不写库（行保持待处理，由能力恢复后的后续批次重新扫描）
//! - 逐行独立处置：定位 / 体积 / 读取 / 调用失败按行降级，单行失败不中断整批
//! - 可选逐行回调供宿主推送进度：批量入口（`understand_attachments`）不传，
//!   导入后处理与"继续处理"编排按需传入

use std::path::Path;
use std::sync::Arc;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider, StorageBackend};
use ramaria_core::types::{AttachmentStatus, MessageAttachment};
use uuid::Uuid;

use crate::engine::Engine;

use super::locate::AttachmentLocator;
use super::prepare;
use super::{
    CapabilityGate, VISION_PROMPT, VISION_TEMPLATE_VERSION, VisionRunStat, capability_gate,
    model_label, sanitize_description,
};

// =========================================================
// 常量
// =========================================================

/// 单张图片体积上限（字节）；超过则不读取直接跳过。
pub(crate) const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

// =========================================================
// 单行处理结果
// =========================================================

/// 单行附件的处理结果（统计计数口径）。
enum RowOutcome {
    /// 调用成功并回填（含重试成功）。
    Done,
    /// 命中已有描述直接复用回填。
    Reused,
    /// 调用失败（重试后仍失败 / 清洗后为空）。
    Failed,
    /// 定位失败 / 超大文件等跳过。
    Skipped,
}

// =========================================================
// 批量入口
// =========================================================

/// 理解一批会话中的待处理图片附件。
///
/// 流程:
/// 1. 扫描 pending 附件并平铺为单批行列表（见 [`scan_pending_attachments`]）；
/// 2. 经 [`understand_attachment_rows`] 统一处理（门禁 + 逐行）。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `session_ids`: 待理解会话列表；
/// - `export_root`: 导出目录（附件相对引用的定位根）。
///
/// 返回:
/// - 本轮执行统计。
pub(crate) async fn understand_attachments(
    engine: &Engine,
    session_ids: &[Uuid],
    export_root: &Path,
) -> RamariaResult<VisionRunStat> {
    let rows = scan_pending_attachments(engine, session_ids).await?;
    let stat = VisionRunStat {
        scanned: rows.len(),
        ..Default::default()
    };
    if rows.is_empty() {
        return Ok(stat);
    }

    // 定位器（单导出根）+ 统一逐行处理
    let mut locator = AttachmentLocator::new(vec![export_root.to_path_buf()]);
    understand_attachment_rows(engine, &rows, &mut locator, None).await
}

/// 扫描一批会话中的待处理图片附件（平铺为单批行列表）。
///
/// 说明:
/// - 逐会话扫描（各自独立限制 `[vision].batch_limit`，0 = 不限），
///   会话内按附件 id 升序（查询层保证）；无待处理行时返回空列表且不记日志；
/// - 供宿主在批量处理前获知总量，以组织起始进度事件。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `session_ids`: 待扫描会话列表。
///
/// 返回:
/// - 平铺后的待处理附件行（调用方负责门禁与逐行处理）。
pub(crate) async fn scan_pending_attachments(
    engine: &Engine,
    session_ids: &[Uuid],
) -> RamariaResult<Vec<MessageAttachment>> {
    let storage = engine.storage_ref().clone();
    let batch_limit = engine.config().vision.batch_limit;
    let mut rows: Vec<MessageAttachment> = Vec::new();
    let mut sessions_with_rows = 0usize;
    for session_id in session_ids {
        let session_rows = storage
            .list_pending_attachments_by_session(*session_id, batch_limit)
            .await?;
        if !session_rows.is_empty() {
            sessions_with_rows += 1;
            rows.extend(session_rows);
        }
    }
    if !rows.is_empty() {
        tracing::info!(
            sessions = sessions_with_rows,
            pending = rows.len(),
            "图片理解批次扫描完成"
        );
    }
    Ok(rows)
}

/// 处理一批附件行（门禁 + 逐行理解；导入后处理与"继续处理"共用单一实现）。
///
/// 流程:
/// 1. 空批直接返回默认统计；
/// 2. 能力门禁（声明 → 隐私 → 探测）；未过时按分支记日志并返回，
///    **不落库**（行保持 pending，待能力可达后重新扫描）；
/// 3. 逐行处理并计数；每完成一行调用一次 `on_row`（附已完成行数）。
///
/// 参数:
/// - `engine`: 服务层引擎；
/// - `rows`: 待处理附件行（调用方完成扫描 / 重置）；
/// - `locator`: 附件定位器（多导出根 + md5 反查）；
/// - `on_row`: 每完成一行回调（附已完成行数，从 1 起；供进度推送，批量入口传 None）。
///
/// 返回:
/// - 本轮执行统计（门禁未过时 skipped = rows.len()，不落库）。
pub(crate) async fn understand_attachment_rows(
    engine: &Engine,
    rows: &[MessageAttachment],
    locator: &mut AttachmentLocator,
    on_row: Option<&(dyn Fn(usize) + Send + Sync)>,
) -> RamariaResult<VisionRunStat> {
    let mut stat = VisionRunStat {
        scanned: rows.len(),
        ..Default::default()
    };
    if rows.is_empty() {
        return Ok(stat);
    }

    let storage = engine.storage_ref().clone();

    // ---- 能力门禁（顺序固定）：未过时行保持 pending，不做状态迁移 ----
    match capability_gate(engine).await? {
        CapabilityGate::DeclaredOff => {
            stat.skipped = rows.len();
            tracing::info!(count = rows.len(), "图片理解声明关闭，附件保持待处理");
            return Ok(stat);
        }
        CapabilityGate::PrivacyPending => {
            tracing::info!(count = rows.len(), "隐私确认未完成，图片保持待处理");
            return Ok(stat);
        }
        CapabilityGate::ProbeFailed => {
            stat.skipped = rows.len();
            tracing::warn!(count = rows.len(), "图片理解能力探测失败，附件保持待处理");
            return Ok(stat);
        }
        CapabilityGate::Ready => {}
    }

    // ---- 逐行处理（单行业务失败不中断整批） ----
    let llm = engine.llm();
    let label = model_label(&llm);
    let mut done = 0usize;
    for row in rows {
        match process_row(&llm, &storage, row, locator, &label).await? {
            RowOutcome::Done => stat.done += 1,
            RowOutcome::Reused => stat.reused += 1,
            RowOutcome::Failed => stat.failed += 1,
            RowOutcome::Skipped => stat.skipped += 1,
        }
        done += 1;
        if let Some(on_row) = on_row {
            on_row(done);
        }
    }

    tracing::info!(
        scanned = stat.scanned,
        done = stat.done,
        reused = stat.reused,
        failed = stat.failed,
        skipped = stat.skipped,
        "图片理解批次完成"
    );
    Ok(stat)
}

// =========================================================
// 单行处理
// =========================================================

/// 处理单行附件（含状态回填等全部副作用；业务失败按标记口径返回）。
async fn process_row(
    llm: &Arc<dyn LlmProvider>,
    storage: &Arc<dyn StorageBackend>,
    row: &MessageAttachment,
    locator: &mut AttachmentLocator,
    model_label: &str,
) -> RamariaResult<RowOutcome> {
    // ---- 1. md5 去重：已有完成描述 → 直接批量回填 ----
    let row_md5 = row.md5.as_deref().filter(|md5| !md5.is_empty());
    if let Some(md5) = row_md5 {
        if let Some((description, description_model)) =
            storage.find_attachment_description_by_md5(md5).await?
        {
            storage
                .fill_attachment_done_by_md5(md5, &description, &description_model)
                .await?;
            return Ok(RowOutcome::Reused);
        }
    }

    // ---- 2. 定位（source_ref 直连优先，md5 反查兜底） ----
    let located = match locator.locate(&row.source_ref, row.md5.as_deref()) {
        Some(located) => located,
        None => {
            tracing::warn!(attachment_id = row.id, "附件定位失败，跳过");
            storage
                .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                .await?;
            return Ok(RowOutcome::Skipped);
        }
    };
    if located.via_reverse_lookup {
        tracing::info!(attachment_id = row.id, "附件经 md5 反查定位成功");
    }
    let file_path = located.path;

    // ---- 3. 体积检查与读取（超大与 IO 失败均为环境跳过，非模型失败） ----
    let metadata = match std::fs::metadata(&file_path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            tracing::warn!(attachment_id = row.id, "附件路径不是文件，跳过");
            storage
                .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                .await?;
            return Ok(RowOutcome::Skipped);
        }
        Err(error) => {
            tracing::warn!(attachment_id = row.id, error = %error, "附件定位失败，跳过");
            storage
                .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                .await?;
            return Ok(RowOutcome::Skipped);
        }
    };
    if metadata.len() > MAX_IMAGE_BYTES {
        tracing::warn!(
            attachment_id = row.id,
            size = metadata.len(),
            "附件超过体积上限，跳过"
        );
        storage
            .mark_attachment_status(row.id, AttachmentStatus::Skipped)
            .await?;
        return Ok(RowOutcome::Skipped);
    }
    let bytes = match std::fs::read(&file_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(attachment_id = row.id, error = %error, "附件读取失败，跳过");
            storage
                .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                .await?;
            return Ok(RowOutcome::Skipped);
        }
    };

    // ---- 4. 输入准备与多模态调用（失败重试一次，共两次尝试） ----
    let prepared = match prepare::prepare_image(
        llm.config().provider.is_online(),
        &bytes,
        prepare::image_mime_for(&located.normalized_ref),
    ) {
        Ok(prepared) => prepared,
        Err(reject) => {
            tracing::warn!(
                attachment_id = row.id,
                reason = reject.as_str(),
                "图片不符合线上理解后端输入约束，跳过"
            );
            storage
                .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                .await?;
            return Ok(RowOutcome::Skipped);
        }
    };
    let request = ChatRequest {
        system_prompt: String::new(),
        memory_context: None,
        history: Vec::new(),
        user_message: VISION_PROMPT.to_string(),
        temperature: 0.2,
        // 输出预算：文字类图片的识别文本较长（描述上限 2000 字符兜底）
        max_tokens: 4096,
        request_id: Uuid::new_v4(),
        template_version: VISION_TEMPLATE_VERSION.to_string(),
        enable_thinking: None,
    };
    let images = vec![prepared.to_data_uri()];
    let reply = match llm.chat_vision(&request, &images).await {
        Ok(reply) => Ok(reply),
        Err(first_error) => {
            tracing::warn!(
                attachment_id = row.id,
                error = %first_error,
                "图片理解调用失败，重试一次"
            );
            llm.chat_vision(&request, &images).await
        }
    };
    let raw = match reply {
        Ok(raw) => raw,
        Err(error) => {
            tracing::warn!(attachment_id = row.id, error = %error, "图片理解调用仍然失败");
            storage
                .mark_attachment_status(row.id, AttachmentStatus::Failed)
                .await?;
            return Ok(RowOutcome::Failed);
        }
    };

    // ---- 5. 清洗与回填（空描述不落库） ----
    let description = sanitize_description(&raw);
    if description.is_empty() {
        tracing::warn!(attachment_id = row.id, "图片理解返回空描述，标记失败");
        storage
            .mark_attachment_status(row.id, AttachmentStatus::Failed)
            .await?;
        return Ok(RowOutcome::Failed);
    }
    match row_md5 {
        Some(md5) => {
            storage
                .fill_attachment_done_by_md5(md5, &description, model_label)
                .await?;
        }
        None => {
            storage
                .mark_attachment_done(row.id, &description, model_label)
                .await?;
        }
    }
    Ok(RowOutcome::Done)
}
