//! crates/ramaria-core/src/types/state.rs - Ramaria 应用运行状态数据类型模块
//!
//! 设计特点:
//! - 定义应用运行状态枚举
//! - 提供状态字符串表示与就绪判断辅助
//! - 供桌面端与 CLI 共享运行态语义
//! - 支持 serde

use serde::{Deserialize, Serialize};

// =========================================================
// 应用状态机
// =========================================================

/// 应用全局状态。
///
/// 职责:
/// - 统一 CLI 和 Desktop 对应用生命周期的理解。
/// - 驱动首次配置、模型下载、索引重建、正常对话和错误恢复界面。
///
/// 状态流:
/// - `NeedsSetup` -> `DownloadingModel` -> `Indexing` -> `Ready`
/// - 可恢复故障进入 `Degraded`
/// - 不可恢复故障进入 `FatalError`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AppState {
    /// 首次配置未完成，需进入配置向导
    NeedsSetup,
    /// embedding 模型下载中
    DownloadingModel,
    /// 索引初始化或重建中
    Indexing,
    /// 可正常对话
    Ready,
    /// 可恢复故障（LLM 暂不可用 或 嵌入模型未配置/不可用）
    ///
    /// 语义扩大：不再仅限 LLM 故障。当嵌入模型缺失时也进入此状态，
    /// 对话功能可用（BM25 + 图谱通道仍工作），但向量检索通道不可用。
    /// 前端应在对话页顶部显示具体原因的警告条。
    Degraded,
    /// 不可恢复错误（数据库损坏、keychain 失败等）
    FatalError,
}

impl AppState {
    /// 返回应用状态的稳定字符串标识。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NeedsSetup => "needs_setup",
            Self::DownloadingModel => "downloading_model",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::FatalError => "fatal_error",
        }
    }
}

impl std::fmt::Display for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------
