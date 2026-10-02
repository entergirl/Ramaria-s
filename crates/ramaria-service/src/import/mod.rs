//! crates/ramaria-service/src/import/mod.rs - QQ 聊天记录导入用例（解析 / L0 写入 / L1 批量生成 / 深度触发）
//!
//! 设计特点:
//! - 管线分段：解析预览（`analyze`）→ L0 写入（`write_l0`）→ L1 批量生成（`generate_l1`）
//!   → 深度处理触发（`trigger_deep`），宿主按导入模式组合调用，不写第二份导入实现
//! - 双画像：导出者与对方分别准备 `source="qq"` 的 persona（UID 生成策略与文件解析口径
//!   由 `ramaria-importer` 承担），L1 摘要按 persona 各生成一份；结果中的画像名以库内实际注册名为准
//! - 进度与 ETA：逐 session 经 `ImportProgressSink` 回调（分层 EMA 预估见 `crate::eta`），
//!   宿主负责把回调转发为自身事件通道
//! - 静默降级：单次 L1 生成失败只记 warn 并计入失败计数，不中断批量（完成提示由宿主汇总）
//! - 隐私：日志不落消息正文与昵称；个人标识一律经 `mask_id` 脱敏，文件路径只留文件名
//! - 宿主差异（快速 / 深度、节流间隔、进度通道）由请求参数与回调表达
//!
//! 模块划分:
//! - `detect`：格式探测与解析前校验（扩展名校验 / 日志路径标签）；
//! - `analyze`：解析预览（`AnalyzeRequest` → `AnalysisReport`，不写入数据库）；
//! - `l0`：L0 写入（双画像准备、会话与消息落库、画像名回读口径）；
//! - `l1`：L1 批量生成与 ETA 进度（含完成摘要构造）；
//! - `deep`：深度触发（L2 事件提取 → L3 性格画像级联）。

mod analyze;
mod deep;
mod detect;
mod l0;
mod l1;

use std::path::Path;

use ramaria_core::error::RamariaResult;
use uuid::Uuid;

use crate::engine::Engine;

// 请求与结果类型 re-export：`crate::import::X` / `ramaria_service::import::X` 为既有调用路径
pub use analyze::{AnalysisReport, AnalyzeRequest};
pub use l0::{ImportL0Outcome, ImportMode, ImportRequest};
pub use l1::{
    ImportDoneSummary, ImportL1Outcome, ImportL1Plan, ImportL1Progress, ImportProgressSink,
    done_summary,
};

// 用例入口 re-export：`crate::import::X` 为引擎门面与既有调用路径
pub(crate) use analyze::analyze;
pub(crate) use deep::trigger_deep;
pub(crate) use detect::detect_format;
pub(crate) use l0::write_l0;
pub(crate) use l1::generate_l1;

// =========================================================
// 引擎门面
// =========================================================

impl Engine {
    /// 探测文件是否为 QQ 聊天记录支持的格式（桌面 / CLI 共用）。
    ///
    /// 说明:
    /// - 文件存在性 / 路径与扩展名白名单校验由入口负责（各自现状口径）；
    /// - 探测失败时返回结构化错误，入口按需补错误前缀。
    pub async fn detect_qq_format(&self, path: &Path) -> RamariaResult<bool> {
        detect_format(self, path).await
    }

    /// 解析 QQ 聊天记录文件并返回诊断报告（不写入数据库）。
    pub async fn analyze_qq_import(&self, req: AnalyzeRequest) -> RamariaResult<AnalysisReport> {
        analyze(self, req).await
    }

    /// 执行 QQ 聊天记录 L0 导入（双画像准备 + 会话 / 消息写入，不含 L1 生成）。
    ///
    /// 说明:
    /// - 需要引擎已附着 SQLite 连接池（`open_with` 装配自动携带；
    ///   `from_parts` 注入构造需先 [`Engine::attach_sqlite_pool`]）。
    pub async fn import_qq_l0(&self, req: ImportRequest) -> RamariaResult<ImportL0Outcome> {
        write_l0(self, req).await
    }

    /// 批量生成导入会话的 L1 摘要（可选级联与进度回调）。
    ///
    /// 参数:
    /// - `session_ids`: 待生成 L1 的会话（取 L0 导入结果）；
    /// - `plan`: 目标列表与节流 / 级联选项（见 [`ImportL1Plan`]）；
    /// - `progress`: 可选进度回调（逐 session 回调，含 ETA 估算）。
    pub async fn generate_import_l1(
        &self,
        session_ids: &[Uuid],
        plan: ImportL1Plan,
        progress: Option<&dyn ImportProgressSink>,
    ) -> RamariaResult<ImportL1Outcome> {
        generate_l1(self, session_ids, plan, progress).await
    }

    /// 触发导入后的深度处理（L2 事件提取 → L3 性格画像级联）。
    ///
    /// 用法:
    /// - 调用方在深度模式且至少一条 L1 生成成功时调用；
    /// - `l1_total` 传 L1 批量生成结果的总量（`None` 时进度事件不带 L1 总量）；
    /// - 阶段进度经 `progress` 回调（L2 / L3 各一条），内部失败只记日志。
    pub async fn trigger_import_deep(
        &self,
        l1_total: Option<usize>,
        progress: Option<&dyn ImportProgressSink>,
    ) -> RamariaResult<()> {
        trigger_deep(self, l1_total, progress).await
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
