//! crates/ramaria-service/src/vision.rs - Ramaria 图片理解模块
//!
//! 设计特点:
//! - 理解任务全链路：扫描待处理附件 → 能力门禁 → md5 去重 → 定位读取 → 多模态调用
//!   → 描述清洗 → 回填；逐行独立处置，单行业务失败不中断整批
//! - 能力门禁顺序固定：声明（配置显式开关）→ 隐私确认 → 探测；探测结论按
//!   model_id + base_url 进程内缓存，同键不重复发送探测请求
//! - 降级纪律：声明关 / 探测失败批量标 skipped；隐私未确认保持 pending 待下次；
//!   定位失败与超大文件跳过；调用失败重试一次后标 failed
//! - md5 去重：同图已有完成描述直接批量回填，不重复调用模型
//! - base64 编码自实现（不新增依赖）；日志不落图片内容与描述原文

// 理解入口经 import 门面（`importer` feature）调用；未启用该 feature 时本模块
// 仅作为引擎探测缓存的类型宿主，不产生未使用代码告警
#![cfg_attr(not(feature = "importer"), allow(dead_code))]

use std::path::Path;
use std::sync::Arc;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider, StorageBackend};
use ramaria_core::types::{AttachmentStatus, MessageAttachment, is_local_relative_ref};
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// 常量
// =========================================================

/// 单张图片体积上限（字节）；超过则不读取直接跳过。
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 描述截断上限（按字符数）。
const MAX_DESCRIPTION_CHARS: usize = 200;

/// 理解任务的内置提示词（固定单轮，请求文本不经用户输入）。
const VISION_PROMPT: &str = "请用一两句话简要描述这张图片的内容，直接给出描述，不要额外说明。";

/// 探测请求的提示词。
const PROBE_PROMPT: &str = "请描述这张图片。";

/// 探测用 1x1 PNG 图片（data URI 的 base64 载荷）。
const PROBE_IMAGE_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// 理解请求的模板版本。
const VISION_TEMPLATE_VERSION: &str = "vision-v1";

/// 探测请求的模板版本。
const PROBE_TEMPLATE_VERSION: &str = "vision-probe-v1";

// =========================================================
// 公开类型
// =========================================================

/// 一轮图片理解的执行统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisionRunStat {
    /// 扫描到的待处理附件数。
    pub scanned: usize,
    /// 实际调用理解并成功回填的描述数。
    pub done: usize,
    /// 命中已有描述（md5 复用）直接回填的描述数。
    pub reused: usize,
    /// 调用失败（重试后仍失败 / 空描述）的描述数。
    pub failed: usize,
    /// 跳过数（定位失败 / 文件过大 / 门禁批量标记）。
    pub skipped: usize,
}

/// 图片理解能力探测缓存条目（按 model_id + base_url 键控）。
pub(crate) struct VisionProbeState {
    /// 缓存键：`{model_id}/{base_url}`。
    pub(crate) model_key: String,
    /// 探测结论（true = 可用）。
    pub(crate) capable: bool,
}

// =========================================================
// 理解任务
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

/// 图片理解能力门禁（内部判定状态）。
enum CapabilityGate {
    /// 声明开启且探测可用。
    Ready,
    /// 配置显式声明当前模型不支持图片理解。
    DeclaredOff,
    /// 线上 provider 隐私确认未完成。
    PrivacyPending,
    /// 能力探测失败（模型不可用 / 不支持多模态）。
    ProbeFailed,
}

/// 理解一批会话中的待处理图片附件。
///
/// 流程:
/// 1. 逐会话扫描 pending 附件（各自独立限制 `[vision].batch_limit`，0 = 不限）；
/// 2. 能力门禁（声明 → 隐私 → 探测）；门禁未通过按分支批量处置后返回；
/// 3. 逐行处理：md5 去重 → 定位读取 → 多模态调用（失败重试一次）→ 清洗 → 回填。
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
    let mut stat = VisionRunStat::default();
    if session_ids.is_empty() {
        return Ok(stat);
    }

    // ---- 1. 扫描（按 session 序；会话内按附件 id 升序，由查询层保证） ----
    let storage = engine.storage_ref().clone();
    let batch_limit = engine.config().vision.batch_limit;
    let mut scanned: Vec<(Uuid, Vec<MessageAttachment>)> = Vec::new();
    for session_id in session_ids {
        let rows = storage
            .list_pending_attachments_by_session(*session_id, batch_limit)
            .await?;
        if !rows.is_empty() {
            scanned.push((*session_id, rows));
        }
    }
    let total: usize = scanned.iter().map(|(_, rows)| rows.len()).sum();
    stat.scanned = total;
    if total == 0 {
        return Ok(stat);
    }

    // ---- 2. 能力门禁（顺序固定） ----
    match capability_gate(engine).await? {
        CapabilityGate::DeclaredOff => {
            for (session_id, rows) in &scanned {
                for row in rows {
                    storage
                        .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                        .await?;
                }
                tracing::info!(
                    session = %session_id,
                    count = rows.len(),
                    "图片理解声明关闭，附件跳过"
                );
            }
            stat.skipped = total;
            return Ok(stat);
        }
        CapabilityGate::PrivacyPending => {
            tracing::info!(
                sessions = scanned.len(),
                pending = total,
                "隐私确认未完成，图片保持待处理"
            );
            return Ok(stat);
        }
        CapabilityGate::ProbeFailed => {
            for (_, rows) in &scanned {
                for row in rows {
                    storage
                        .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                        .await?;
                }
            }
            stat.skipped = total;
            tracing::warn!(count = total, "图片理解能力探测失败，附件跳过");
            return Ok(stat);
        }
        CapabilityGate::Ready => {}
    }

    // ---- 3. 逐行处理（单行业务失败不中断整批） ----
    let llm = engine.llm();
    let label = model_label(&llm);
    for (_, rows) in &scanned {
        for row in rows {
            match process_row(&llm, &storage, row, export_root, &label).await? {
                RowOutcome::Done => stat.done += 1,
                RowOutcome::Reused => stat.reused += 1,
                RowOutcome::Failed => stat.failed += 1,
                RowOutcome::Skipped => stat.skipped += 1,
            }
        }
    }

    tracing::info!(
        sessions = scanned.len(),
        scanned = stat.scanned,
        done = stat.done,
        reused = stat.reused,
        failed = stat.failed,
        skipped = stat.skipped,
        "图片理解批次完成"
    );
    Ok(stat)
}

/// 处理单行附件（含状态回填等全部副作用；业务失败按标记口径返回）。
async fn process_row(
    llm: &Arc<dyn LlmProvider>,
    storage: &Arc<dyn StorageBackend>,
    row: &MessageAttachment,
    export_root: &Path,
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

    // ---- 2. 定位（仅受信相对引用；导出根拼接后校验文件） ----
    let source_ref = row.source_ref.as_str();
    if !is_local_relative_ref(source_ref) {
        tracing::warn!(attachment_id = row.id, "附件引用不可定位，跳过");
        storage
            .mark_attachment_status(row.id, AttachmentStatus::Skipped)
            .await?;
        return Ok(RowOutcome::Skipped);
    }
    let path = export_root.join(source_ref);
    let metadata = match std::fs::metadata(&path) {
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

    // ---- 3. 体积检查与读取（超大与 IO 失败均为环境跳过，非模型失败） ----
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
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(attachment_id = row.id, error = %error, "附件读取失败，跳过");
            storage
                .mark_attachment_status(row.id, AttachmentStatus::Skipped)
                .await?;
            return Ok(RowOutcome::Skipped);
        }
    };

    // ---- 4. 多模态调用（失败重试一次，共两次尝试） ----
    let request = ChatRequest {
        system_prompt: String::new(),
        memory_context: None,
        history: Vec::new(),
        user_message: VISION_PROMPT.to_string(),
        temperature: 0.2,
        max_tokens: 256,
        request_id: Uuid::new_v4(),
        template_version: VISION_TEMPLATE_VERSION.to_string(),
    };
    let data_uri = format!(
        "data:{};base64,{}",
        image_mime_for(source_ref),
        base64_encode(&bytes)
    );
    let images = vec![data_uri];
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

// =========================================================
// 能力门禁与探测
// =========================================================

/// 判定图片理解能力门禁（顺序固定：声明 → 隐私 → 探测）。
async fn capability_gate(engine: &Engine) -> RamariaResult<CapabilityGate> {
    if !engine.config().vision.model_supports_vision {
        return Ok(CapabilityGate::DeclaredOff);
    }
    // 隐私状态为 non_exhaustive：仅"已确认 / 无需确认"放行，其余（含未知状态）保守保持待处理
    if !engine.check_privacy().await?.is_confirmed() {
        return Ok(CapabilityGate::PrivacyPending);
    }
    if probe_once(engine).await {
        Ok(CapabilityGate::Ready)
    } else {
        Ok(CapabilityGate::ProbeFailed)
    }
}

/// 确认当前模型图片理解可用（结果按 model_id + base_url 进程内缓存）。
async fn probe_once(engine: &Engine) -> bool {
    let llm = engine.llm();
    let model_key = format!(
        "{}/{}",
        llm.config().capability.model_id,
        llm.config().capability.base_url
    );

    // 缓存查询（短临界区：锁内只读，不跨 await 持锁）
    {
        let guard = engine.vision_probe_slot().lock().await;
        if let Some(state) = guard.as_ref().filter(|state| state.model_key == model_key) {
            return state.capable;
        }
    }

    let request = ChatRequest {
        system_prompt: String::new(),
        memory_context: None,
        history: Vec::new(),
        user_message: PROBE_PROMPT.to_string(),
        temperature: 0.2,
        max_tokens: 8,
        request_id: Uuid::new_v4(),
        template_version: PROBE_TEMPLATE_VERSION.to_string(),
    };
    let images = vec![format!("data:image/png;base64,{PROBE_IMAGE_BASE64}")];
    let capable = match llm.chat_vision(&request, &images).await {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(error = %error, "图片理解能力探测失败");
            false
        }
    };

    // 结果写回缓存（短临界区）
    {
        let mut guard = engine.vision_probe_slot().lock().await;
        *guard = Some(VisionProbeState { model_key, capable });
    }
    capable
}

// =========================================================
// 内部纯函数
// =========================================================

/// 产生描述的模型标识：优先模型 ID，缺失时用 provider 稳定标识。
fn model_label(llm: &Arc<dyn LlmProvider>) -> String {
    let config = llm.config();
    if config.capability.model_id.is_empty() {
        config.provider.as_str().to_string()
    } else {
        config.capability.model_id.clone()
    }
}

/// 按扩展名返回图片 MIME（未知扩展名回退 image/jpeg）。
fn image_mime_for(source_ref: &str) -> &'static str {
    let extension = Path::new(source_ref)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => "image/jpeg",
    }
}

/// 清洗模型返回的描述文本。
///
/// 步骤:
/// - trim 首尾空白，循环剥离成对包裹的引号（`"` / `'` / `“”` / `「」`）；
/// - 换行与控制字符替换为空格；
/// - 折叠连续空白为单个空格；
/// - 按字符截断到 200 字符。
fn sanitize_description(raw: &str) -> String {
    let unquoted = strip_wrapping_quotes(raw);
    let mut spaced = String::with_capacity(unquoted.len());
    for ch in unquoted.chars() {
        if ch.is_control() {
            spaced.push(' ');
        } else {
            spaced.push(ch);
        }
    }
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_DESCRIPTION_CHARS).collect()
}

/// 循环剥离成对包裹的引号（剥离后重新 trim；不处理嵌套不配对形态）。
fn strip_wrapping_quotes(text: &str) -> &str {
    let mut current = text.trim();
    loop {
        if current.chars().count() < 2 {
            return current;
        }
        let first = current.chars().next();
        let last = current.chars().next_back();
        let matched = matches!(
            (first, last),
            (Some('"'), Some('"'))
                | (Some('\''), Some('\''))
                | (Some('“'), Some('”'))
                | (Some('「'), Some('」'))
        );
        if !matched {
            return current;
        }
        let start = first.map(char::len_utf8).unwrap_or(0);
        let end = current.len() - last.map(char::len_utf8).unwrap_or(0);
        current = current[start..end].trim();
    }
}

/// 标准 base64 编码（RFC 4648 字母表 + '=' 补齐）。仅编码，无解码。
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
