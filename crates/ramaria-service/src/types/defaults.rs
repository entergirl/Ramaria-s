//! crates/ramaria-service/src/types/defaults.rs - Ramaria 服务层默认值与边界常量
//!
//! 设计特点:
//! - 默认值与硬边界集中声明，入口层（MCP schema）与用例层共用同一口径，避免双处定义漂移
//! - 常量口径对齐工具契约（memory_recall / chat_ingest / chat_send / chat_history）
//! - 缺省语义：调用方未指定 persona / max_items / max_chars / limit 时使用
//! - 通道标识用于会话来源标注（MCP 入口）

// =========================================================
// 默认值与边界常量
// =========================================================

/// 默认目标人格 uid（调用方未指定 persona 时使用）。
pub const DEFAULT_PERSONA_UID: &str = "rama-0001";

/// 召回条目默认上限（`max_items` 缺省值）。
pub const DEFAULT_MAX_ITEMS: u32 = 5;

/// 召回条目上限的硬边界（`max_items` 超过时按此截断）。
pub const MAX_ITEMS_LIMIT: u32 = 20;

/// 召回上下文文本默认预算（`max_chars` 缺省值，单位：字符）。
pub const DEFAULT_MAX_CHARS: u32 = 1200;

/// 会话历史分页默认条数（`chat_history.limit` 缺省值）。
pub const DEFAULT_HISTORY_LIMIT: u32 = 20;

/// MCP 入口的会话通道标识（外部 MCP 客户端产生的会话）。
pub const CHANNEL_MCP: &str = "mcp";
