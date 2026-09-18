//! crates/ramaria-service/src/lib.rs - Ramaria 记忆与对话服务层入口
//!
//! 设计特点:
//! - 与传输无关的能力层：召回、写入、封存、会话解析、空闲检查、人格读取六类用例
//! - 依赖方向单向：入口（CLI / 桌面 / MCP / 未来社交通道）→ service → 内核，反向禁止
//! - 禁止依赖 `ramaria-app` / `ramaria-cli` / `ramaria-desktop` / `tauri`（分层纪律）
//! - 请求与响应为纯数据结构（`types` 模块），不出现 stdio / Tauri / HTTP 概念
//! - 通道（channel / external_ref）是数据属性而非算法输入
//! - 降级纪律：LLM / embedding 不可用时不阻塞装配与记忆读取
//!
//! 分层说明:
//! - `engine` 负责依赖装配（storage / config / LLM / embedding / 检索占位）与用例挂载点；
//! - 用例实现按里程碑接入，入口层（如 `ramaria-mcp`）只做协议包装，不承载业务逻辑。

pub mod engine;
pub mod types;

pub use engine::{Engine, EngineOptions};
pub use types::{
    BehaviorRuleView, ChatRole, ChatTurn, DEFAULT_HISTORY_LIMIT, DEFAULT_MAX_CHARS,
    DEFAULT_MAX_ITEMS, DEFAULT_PERSONA_UID, DataMaturityView, FactView, HistoryMessageView,
    HistoryRequest, HistoryResult, IngestOutcome, IngestRequest, MAX_ITEMS_LIMIT,
    PersonaCardRequest, PersonaCardView, PersonaSection, PersonaSummaryView, RecallItem,
    RecallLayer, RecallMode, RecallRequest, RecallResult, RecallStats, SealOutcome,
    SessionSummaryView, TraitView,
};
