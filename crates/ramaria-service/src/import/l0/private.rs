//! crates/ramaria-service/src/import/l0/private.rs - Ramaria 私聊导入（双画像）
//!
//! 设计特点:
//! - 导出者 / 对方双画像准备：UID 优先级 > QQ 号 > 平台 UID > 递增序号
//! - 导入侧过滤跳过的画像不创建，对应消息按侧过滤丢弃
//! - 画像名从库内回读上报，回读失败回退展示名

use ramaria_core::error::RamariaResult;
use ramaria_core::privacy::mask_id;
use ramaria_importer::qq::{PersonaSide, build_persona_uid, ensure_qq_persona};
use ramaria_importer::writer::{ImportWriter, PersonaDispatch};
use sqlx::SqlitePool;

use super::tools::resolve_persona_name;
use super::types::{ImportL0Outcome, ImportRequest};
use crate::import::analyze::report_time_range;

// =========================================================
// 私聊导入（双画像）
// =========================================================

/// 私聊导入：准备导出者 / 对方双画像并写入会话与消息。
///
/// 参数:
/// - `pool`: 数据库连接池；
/// - `sessions`: 解析后的会话列表；
/// - `report`: 解析报告（画像标识与名称来源）；
/// - `req`: 导入请求（导入侧过滤与画像覆盖）。
///
/// 返回:
/// - `ImportL0Outcome`：写入统计、双画像标识与 session UUID 列表。
pub(super) async fn import_dual(
    pool: &SqlitePool,
    sessions: &[ramaria_importer::ImportedSession],
    report: &ramaria_importer::ImportReport,
    req: &ImportRequest,
) -> RamariaResult<ImportL0Outcome> {
    // ---- 1. 双画像准备 ----
    // 查询已有 QQ persona 最大 seq（用于 UID 递增序号兜底）
    let all_personas = ramaria_storage::repo::personas::list_all(pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "查询已有 persona 列表失败");
            e
        })?;
    let max_qq_seq: u32 = all_personas
        .iter()
        .filter(|p| p.source == "qq")
        .map(|p| p.seq as u32)
        .max()
        .unwrap_or(0);

    // 1a. 导出者（我方）：UID 前缀 user-（kind=user）；导入侧过滤跳过时不创建
    // 请求覆盖名仅作创建入参；复用既有 persona 时以库内名为准（结果构建阶段回读）
    let self_requested_name = req
        .persona_name
        .clone()
        .unwrap_or_else(|| report.self_name.clone());
    let self_default_uid = build_persona_uid(
        PersonaSide::Me,
        req.self_persona_uid.as_deref(),
        report.self_uin.as_deref(),
        &report.self_id,
        max_qq_seq + 1,
    );
    let self_persona_uid = if req.side.needs_persona(PersonaSide::Me) {
        let resolved = ensure_qq_persona(
            pool,
            &self_default_uid,
            &self_requested_name,
            Some(&report.self_id),
        )
        .await
        .map_err(|e| {
            tracing::error!(
                error = %e,
                persona_uid = %mask_id(&self_default_uid),
                persona_name = %mask_id(&self_requested_name),
                "创建/查找导出者 persona 失败"
            );
            e
        })?;
        tracing::info!(
            persona_uid = %mask_id(&resolved),
            persona_name = %mask_id(&self_requested_name),
            "导出者 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过导出者 persona");
        None
    };

    // 1b. 对方：UID 前缀 char-；导入侧过滤跳过时不创建
    let other_name = req.other_persona_name.clone().unwrap_or_else(|| {
        if report.other_name.is_empty() {
            report.chat_name.clone()
        } else {
            report.other_name.clone()
        }
    });
    let other_ref_id = if report.other_uid.is_empty() {
        None
    } else {
        Some(report.other_uid.as_str())
    };
    let other_default_uid = build_persona_uid(
        PersonaSide::Other,
        req.other_persona_uid.as_deref(),
        report.other_uin.as_deref(),
        &report.other_uid,
        max_qq_seq + 2,
    );
    tracing::debug!(
        other_uid = %mask_id(&other_default_uid),
        other_name = %mask_id(&other_name),
        other_ref_id = ?other_ref_id.map(mask_id),
        "准备创建对方 persona"
    );
    let other_persona_uid = if req.side.needs_persona(PersonaSide::Other) {
        let resolved = ensure_qq_persona(pool, &other_default_uid, &other_name, other_ref_id)
            .await
            .map_err(|e| {
                tracing::error!(
                    error = %e,
                    other_uid = %mask_id(&other_default_uid),
                    other_name = %mask_id(&other_name),
                    "创建/查找对方 persona 失败"
                );
                e
            })?;
        tracing::info!(
            persona_uid = %mask_id(&resolved),
            persona_name = %mask_id(&other_name),
            "对方 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过对方 persona");
        None
    };

    // ---- 2. L0 写入（按导入侧过滤消息；跳过侧画像为 None） ----
    tracing::debug!(
        sessions_count = sessions.len(),
        self_persona = ?self_persona_uid.as_deref().map(mask_id),
        other_persona = ?other_persona_uid.as_deref().map(mask_id),
        side = ?req.side,
        "准备执行 L0 写入"
    );

    let outcome = ImportWriter::write_l0(
        pool,
        sessions,
        PersonaDispatch::Dual {
            self_uid: &report.self_id,
            self_persona_uid: self_persona_uid.as_deref(),
            other_persona_uid: other_persona_uid.as_deref(),
        },
        req.side,
        req.file_path.parent(),
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "导入写入失败");
        e
    })?;

    tracing::info!(
        sessions_written = outcome.sessions_written,
        messages_written = outcome.messages_written,
        messages_dropped = outcome.messages_dropped,
        "L0 写入完成"
    );
    if outcome.messages_dropped > 0 {
        tracing::warn!(
            messages_dropped = outcome.messages_dropped,
            "导入存在因画像缺失被丢弃的消息（不记录消息内容）"
        );
    }

    // ---- 3. 构建结果 ----
    let time_range = report_time_range(report);
    let report_summary = report.summary();
    let skipped_count = report.total_skipped();
    // 文件解析名：预览对照口径，与画像实际注册名（`persona_name`）区分
    let self_name = report.self_name.clone();
    let chat_name = report.chat_name.clone();

    // 画像名从库内回读（复用既有 persona 时可能与请求覆盖名 / 文件解析名不同）；
    // 回读失败只记 warn 并回退展示名，不阻塞导入
    let persona_name =
        resolve_persona_name(pool, self_persona_uid.as_deref(), &self_requested_name).await;
    let other_persona_name =
        resolve_persona_name(pool, other_persona_uid.as_deref(), &other_name).await;

    Ok(ImportL0Outcome {
        report_summary,
        sessions_written: outcome.sessions_written,
        messages_written: outcome.messages_written,
        messages_dropped: outcome.messages_dropped,
        persona_uid: self_persona_uid,
        persona_name,
        other_persona_uid,
        other_persona_name,
        self_name,
        chat_name,
        time_range,
        skipped_count,
        session_ids: outcome.session_ids,
        mode: req.mode,
        chat_type: report.chat_type.clone(),
        members: report.members.clone(),
    })
}
