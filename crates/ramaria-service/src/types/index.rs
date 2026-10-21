//! crates/ramaria-service/src/types/index.rs - Ramaria 索引状态与重建用例数据结构
//!
//! 设计特点:
//! - 状态视图与进度事件共用同一状态枚举与计数口径（loaded / total / percent）
//! - 百分比只表达向量文档的加载进度：无向量文档（total=0）就绪时为 100%
//! - 可选字段（模型标识 / 失败原因）缺省不输出，前端按缺省处理

use ramaria_core::types::IndexStatus;
use serde::{Deserialize, Serialize};

// =========================================================
// 索引状态
// =========================================================

/// 索引状态视图（设置页与诊断展示）。
///
/// 字段约定:
/// - `status`: 索引状态机当前值。
/// - `loaded` / `total`: 已加载向量文档数与向量文档总数（total=0 表示本次构建无向量路径）。
/// - `failed`: 本次构建中向量补算失败的条数（0 = 无失败）。
/// - `percent`: 进度百分比（0~100；total=0 时 Ready 为 100，其余为 0）。
/// - `model`: 当前向量模型标识（无向量路径时为 None）。
/// - `last_error`: 最近一次构建失败的脱敏原因（成功后清空）。
/// - `updated_at`: 状态最后更新时刻（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexStatusView {
    pub status: IndexStatus,
    pub loaded: usize,
    pub total: usize,
    pub failed: usize,
    pub percent: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub updated_at: i64,
}

/// 索引构建进度事件负载。
///
/// 字段约定:
/// - `status`: 事件发生时的索引状态。
/// - `loaded` / `total` / `failed` / `percent`: 与状态视图同一口径。
/// - `elapsed_ms`: 本次构建已耗时（毫秒）。
/// - `model`: 当前向量模型标识（无向量路径时为 None，不输出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexProgress {
    pub status: IndexStatus,
    pub loaded: usize,
    pub total: usize,
    pub failed: usize,
    pub percent: u8,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// 索引重建触发回执。
///
/// 字段约定:
/// - `started`: 是否成功启动构建（false = 已有构建在途，本次忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRebuildReceipt {
    pub started: bool,
}
