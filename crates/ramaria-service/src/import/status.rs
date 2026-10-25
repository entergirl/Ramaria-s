//! crates/ramaria-service/src/import/status.rs - Ramaria 导入续跑状态查询
//!
//! 设计特点:
//! - 只读汇总：进程内后处理标志 + 持久化待办规模（附件 / 缺失摘要会话）
//! - 跨重启可判定：待办规模来自数据库既有状态，不新增表、不写标记
//! - 口径与"继续处理"编排一致：计数所用过滤条件与 `resume` 的扫描条件同源
//! - 计数走 `COUNT(*)`，不加载附件行或会话 id 列表

use ramaria_core::error::RamariaResult;

use crate::engine::Engine;

use super::is_post_active;

// =========================================================
// 结果类型
// =========================================================

/// 导入续跑状态汇总。
///
/// 字段约定:
/// - `post_active`: 进程内导入后处理（图片理解 / 摘要 / 级联）是否在途；
/// - `pending_attachments`: 全库待处理附件数（含中断残留）；
/// - `skipped_attachments`: 全库被跳过的附件数（更换导出根后可回补）；
/// - `missing_l1_sessions`: 已关闭且缺摘要的本地会话数（可补跑）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportStatus {
    pub post_active: bool,
    pub pending_attachments: u64,
    pub skipped_attachments: u64,
    pub missing_l1_sessions: u64,
}

impl ImportStatus {
    /// 是否存在可继续处理的事项（附件补处理或摘要补跑）。
    pub fn resumable(&self) -> bool {
        self.pending_attachments > 0 || self.skipped_attachments > 0 || self.missing_l1_sessions > 0
    }
}

// =========================================================
// 用例入口
// =========================================================

/// 查询导入续跑状态（只读）。
///
/// 参数:
/// - `engine`: 服务层引擎（需已附着 SQLite 连接池）。
///
/// 返回:
/// - [`ImportStatus`]：后处理标志与三类待办计数。
///
/// 说明:
/// - 任一计数失败即整体返回错误；状态查询是显式用户动作，不做静默降级。
pub(crate) async fn query_status(engine: &Engine) -> RamariaResult<ImportStatus> {
    let storage = engine.storage_ref().clone();
    let pending_attachments = storage.count_pending_attachments().await?;
    let skipped_attachments = storage.count_skipped_attachments().await?;
    let missing_l1_sessions = storage.count_closed_sessions_missing_l1().await?;

    Ok(ImportStatus {
        post_active: is_post_active(),
        pending_attachments,
        skipped_attachments,
        missing_l1_sessions,
    })
}
