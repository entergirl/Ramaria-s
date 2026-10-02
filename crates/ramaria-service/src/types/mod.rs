//! crates/ramaria-service/src/types/mod.rs - Ramaria 服务层用例数据结构
//!
//! 设计特点:
//! - 与传输无关的纯数据：不出现 stdio / Tauri / HTTP 概念（工具契约结构体均可 serde 序列化）
//! - 字段口径对齐工具契约（memory_recall / chat_send / chat_ingest / persona_* / chat_history）；
//!   交互入口的流式生成请求（`ChatStreamRequest`）承载配置覆盖与预置上文，不做 serde 序列化
//! - 默认值与边界以常量集中声明，入口层（MCP schema）与用例层共用同一口径，避免双处定义漂移
//! - 时间字段对外统一 ISO-8601 UTC 字符串；毫秒时间戳由用例层在映射时转换
//! - 枚举序列化统一小写，与 MCP 客户端 JSON 约定一致
//! - 结构体仅承载数据，不含行为；业务语义由用例层（engine / recall / ingest 等）实现

mod chat;
mod defaults;
mod facts;
mod ingest;
mod keyword;
mod memory_browse;
mod persona;
mod recall;
mod session;
mod setup;

pub use chat::{
    ChatRole, ChatSendOutcome, ChatSendRequest, ChatStreamRequest, ChatTurn, SealOutcome,
};
pub use defaults::{
    CHANNEL_MCP, DEFAULT_HISTORY_LIMIT, DEFAULT_MAX_CHARS, DEFAULT_MAX_ITEMS, DEFAULT_PERSONA_UID,
    MAX_ITEMS_LIMIT,
};
pub use facts::{
    FactBrowsePage, FactBrowseRequest, FactDetailView, FactEntryView, GroupedFactsView,
};
pub use ingest::{IngestOutcome, IngestRequest};
pub use keyword::{
    AliasAction, AliasResolveOutcome, AliasResolveRequest, KeywordEntryView, KeywordPoolView,
    KeywordSeedItem, KeywordSeedOutcome, KeywordSuggestionOutcome, PendingAliasView,
};
pub use memory_browse::{
    EvidenceEventView, EvidenceL1SourceView, L1BrowsePage, L1BrowseRequest, L1MemoryView,
    L2BrowsePage, L2BrowseRequest, L2EventView, L3TraitView, PersonalityProfileView,
    ProfileStatusView, TraitDetailView, TraitEvidenceRequest, TraitEvidenceView,
};
pub use persona::{
    BehaviorRuleView, DataMaturityView, FactView, PersonaCardRequest, PersonaCardView,
    PersonaFileAction, PersonaFileOutcome, PersonaFullView, PersonaSection, PersonaSummaryView,
    PersonaUpdateRequest, StyleView, TraitView,
};
pub use recall::{RecallItem, RecallLayer, RecallMode, RecallRequest, RecallResult, RecallStats};
pub use session::{
    ChannelOverviewView, HistoryMessageView, HistoryRequest, HistoryResult, SessionBrowsePage,
    SessionBrowseRequest, SessionDetailView, SessionMessageView, SessionMessagesRequest,
    SessionMessagesView, SessionSummaryView,
};
pub use setup::{
    DegradedReason, EmbeddingModelView, EmbeddingValidation, SetupRequest, SetupStatus,
};

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
