//! crates/ramaria-desktop/src/commands/import_status.rs - 导入续跑状态查询 Tauri Command
//!
//! 设计特点:
//! - 只读查询：进程内后处理标志 + 三类持久化待办计数（附件 / 跳过 / 缺失摘要）
//! - `resumable` 由服务层口径计算，前端不重算"是否可继续处理"
//! - 计数不含路径与正文，日志只记规模
//! - 不做静默降级：数据库错误原样转用户文案，由前端展示错误块

use crate::DesktopState;
use serde::Serialize;
use tauri::State;

// =========================================================
// 返回结构
// =========================================================

/// 导入续跑状态视图。
///
/// 字段约定:
/// - `post_active`: 导入后处理（图片理解 / 摘要 / 级联）是否在途；
/// - `pending_attachments` / `skipped_attachments`: 待处理与被跳过的附件数；
/// - `missing_l1_sessions`: 已关闭但缺摘要的会话数；
/// - `resumable`: 上述任一待办大于 0。
#[derive(Debug, Clone, Serialize)]
pub struct ImportStatusView {
    pub post_active: bool,
    pub pending_attachments: u64,
    pub skipped_attachments: u64,
    pub missing_l1_sessions: u64,
    pub resumable: bool,
}

// =========================================================
// get_import_status — 查询可继续处理的导入事项
// =========================================================

/// 查询导入续跑状态。
///
/// 返回:
/// - `ImportStatusView`：后处理标志与待办规模。
///
/// 说明:
/// - 进入导入页时调用，用于展示「继续处理」入口与规模提示；
/// - 跨重启有效：待办规模来自数据库既有状态。
#[tauri::command]
pub async fn get_import_status(state: State<'_, DesktopState>) -> Result<ImportStatusView, String> {
    let status = state
        .engine
        .import_status()
        .await
        .map_err(|e| crate::commands::service_error_message(&e, "查询导入状态失败"))?;

    tracing::info!(
        post_active = status.post_active,
        pending = status.pending_attachments,
        skipped = status.skipped_attachments,
        missing_l1 = status.missing_l1_sessions,
        "导入续跑状态已查询"
    );

    Ok(ImportStatusView {
        post_active: status.post_active,
        pending_attachments: status.pending_attachments,
        skipped_attachments: status.skipped_attachments,
        missing_l1_sessions: status.missing_l1_sessions,
        resumable: status.resumable(),
    })
}
