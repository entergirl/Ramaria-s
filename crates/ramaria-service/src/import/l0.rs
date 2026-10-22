//! crates/ramaria-service/src/import/l0.rs - Ramaria QQ 聊天记录 L0 写入模块
//!
//! 设计特点:
//! - 写入路径按对话类型分流：私聊双画像准备（导出者 / 对方），群聊按发送者聚合成员并逐成员准备画像
//! - 双画像准备：UID 优先级 > QQ 号 > 平台 UID > 递增序号；导入侧过滤跳过时不创建
//! - 群聊成员画像：成员聚合按发送者归并（跳过空 UID），UID 前缀 user- / char-，会话归属导出者
//! - `ImportWriter::write_l0` 写入会话与消息（指纹去重，失败补偿删除会话）
//! - 画像名从库内回读上报（复用既有 persona 时以库内实际注册名为准），回读失败回退展示名
//! - 未附着 SQLite 连接池时显式报错（`Engine::attach_sqlite_pool`），不 panic
//! - 隐私：个人标识一律经 `mask_id` 脱敏，文件路径只留文件名

mod group;
mod private;
mod tools;
mod types;

#[cfg(test)]
pub(crate) use tools::resolve_persona_name;
pub use types::{ImportL0Outcome, ImportMode, ImportRequest};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_importer::ImportSource;
use ramaria_importer::qq::QqImporter;

use crate::engine::Engine;

use super::detect::{detect_format, ensure_json_extension, path_log_label};
use group::import_group;
use private::import_dual;

// =========================================================
// 用例入口
// =========================================================

/// 执行 QQ 聊天记录 L0 导入（画像准备 + 会话 / 消息写入）。
///
/// 流程:
/// 1. 连接池与扩展名校验；
/// 2. 格式检测与文件解析（空会话显式报错）；
/// 3. 按对话类型分流：私聊准备双画像（导出者 / 对方），
///    群聊按发送者聚合成员并逐成员准备 persona（按导入侧过滤跳过时不创建）；
/// 4. `ImportWriter::write_l0` 写入会话与消息（指纹去重，失败补偿删除会话）；
///    画像名从库内回读上报（回读失败回退请求覆盖名 / 文件解析名）；
/// 5. 登记导出根目录（best-effort：失败不阻塞导入）。
///
/// 参数:
/// - `engine`: 服务层引擎（须已附着 SQLite 连接池）；
/// - `req`: 导入请求（文件 / 模式 / 切割间隔 / 导入侧 / 画像覆盖）。
///
/// 返回:
/// - `ImportL0Outcome`：写入统计、双画像标识与 session UUID 列表。
pub(crate) async fn write_l0(
    engine: &Engine,
    req: ImportRequest,
) -> RamariaResult<ImportL0Outcome> {
    let Some(pool) = engine.sqlite_pool() else {
        return Err(RamariaError::unsupported(
            "导入用例需要 SQLite 连接池（Engine::attach_sqlite_pool）",
        ));
    };
    let path = req.file_path.as_path();
    ensure_json_extension(path)?;

    tracing::info!(
        file = %path_log_label(path),
        mode = %req.mode.as_str(),
        gap_minutes = req.gap_minutes,
        side = ?req.side,
        "开始 QQ 聊天记录导入"
    );

    // ---- 1. 格式检测与文件解析 ----
    let is_qq = detect_format(engine, path).await?;

    if !is_qq {
        return Err(RamariaError::validation(format!(
            "文件 '{}' 不是 QQ 聊天记录格式。请确认文件来自 qq-chat-exporter v6.x 导出的 JSON。",
            path.display()
        )));
    }

    let importer = QqImporter::new();
    let (sessions, report) = importer.parse(path, req.gap_minutes).inspect_err(|_| {
        tracing::warn!(file = %path_log_label(path), "QQ 聊天记录文件解析失败");
    })?;

    if sessions.is_empty() {
        return Err(RamariaError::validation(format!(
            "文件中没有可导入的消息。解析报告: 原始 {} 条，成功 0 条，跳过 {} 条。",
            report.total_raw,
            report.total_skipped()
        )));
    }

    tracing::info!(
        session_count = sessions.len(),
        total_success = report.total_success(),
        total_degraded = report.total_degraded(),
        total_skipped = report.total_skipped(),
        "文件解析完成"
    );

    // ---- 2. 画像准备与写入（按对话类型分流） ----
    // 群聊：按发送者聚合成员并逐成员准备 persona；私聊：导出者 / 对方双画像
    let outcome = if report.chat_type == "group" {
        import_group(&pool, &sessions, &report, &req).await?
    } else {
        import_dual(&pool, &sessions, &report, &req).await?
    };

    // ---- 3. 登记导出根目录（best-effort：失败不阻塞导入） ----
    if let Some(root) = req.file_path.parent().and_then(|parent| parent.to_str()) {
        if let Err(error) =
            crate::attachment_read::register_export_root(engine.storage_ref().as_ref(), root).await
        {
            tracing::warn!(error = %error, "导出根登记失败（不阻塞导入）");
        }
    }

    Ok(outcome)
}
