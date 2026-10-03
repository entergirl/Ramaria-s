//! crates/ramaria-service/src/lib.rs - Ramaria 记忆与对话服务层入口
//!
//! 设计特点:
//! - 与传输无关的能力层：召回、写入、封存、会话解析、空闲检查、人格读取六类用例
//! - 依赖方向单向：入口（CLI / 桌面 / MCP / 未来社交通道）→ service → 内核，反向禁止
//! - 禁止依赖 `ramaria-cli` / `ramaria-desktop` / `tauri` 等入口层（分层纪律）
//! - 请求与响应为纯数据结构（`types` 模块），不出现 stdio / Tauri / HTTP 概念
//! - 通道（channel / external_ref）是数据属性而非算法输入
//! - 召回同源：记忆层检索复用 `ramaria_memory::recall`（在线管线同一份实现）
//! - 降级纪律：LLM / embedding 不可用时不阻塞装配与记忆读取
//!
//! 模块划分:
//! - `engine`：依赖装配（storage / config / LLM / embedding / 检索槽）与用例入口；
//! - `config`：配置双写用例（config.toml ↔ settings / backend_config 表，一致性校验与模板生成）；
//! - `index`：检索索引懒加载、代次刷新与 L1 增量镜像（召回前置）；
//! - `model`：模型管理用例（后端配置热更新、嵌入模型校验 / 加载 / 读取、降级原因）；
//! - `setup`：首次配置用例（缺项诊断、状态机推进、后端健康探测）；
//! - `recall` / `chat` / `ingest` / `seal` / `idle` / `session` / `persona`：用例实现；
//!   空闲检查同时提供宿主循环（`IdleLoop`），供长驻进程免外部驱动自动封存超时会话；
//! - `settings`：设置键值与元信息读取用例（settings 表读写 / 后端配置 / schema 版本 / 密钥掩码）；
//! - `browse`：记忆与会话浏览用例（L1 / L2 / L3 / 事实 / 证据链 / 会话列表与消息）；
//! - `behavior`：行为规则用例（管理 / 学习 / 证据链 / 增量更新，与封存钩子同源）；
//! - `style`：表达风格统计用例（增量更新 / 规则读取 / 统计视图）；
//! - `keyword`：关键词词典用例（列表 / 幂等注入 / 待确认别名 / 别名裁决状态机）；
//! - `export`：会话导出数据装配用例（会话集合 + 消息 + 人格 L1 摘要段）；
//! - `utt`：utt 话语块重建用例（配置读取 / 强制重切 / 索引刷新）；
//! - `l2`：L2 事件提取触发（无 app 宿主的运行时用）；
//! - `lifecycle`：会话生命周期容器（活跃指针 / 手动关闭 / 空闲检查线程 / L2-L3 调度 /
//!   主动对话调度 / 关停）与 L1 摘要重生成 / 补扫，后台定时链路与手动触发共用；
//! - `fact_extract`：知识事实自动抽取编排（`[knowledge].auto_fact_detect` 增强层）；
//! - `hooks`：封存钩子默认装配（轻量链 / 完整链两套，供入口按响应语义注册）；
//! - `stream_event`：流式事件领域模型（Delta / Done / Error）与事件流句柄；
//! - `privacy` / `bridge` / `feedback`：隐私确认、新会话桥接、弱反馈检测（生成编排的伴随能力）；
//! - `eta`：导入进度分层 EMA 预估（纯计算，无 I/O）；`update`：GitHub Release 版本检查；
//! - `error_hint`：错误到用户提示映射（标题 / 明细 / 可重试标记 / 入口统一文案）；
//! - `diagnostics`：诊断信息导出用例（日志 / 配置脱敏 + 原子替换）；
//! - `import`：QQ 聊天记录导入用例（解析 / L0 写入 / L1 批量生成与 ETA / 深度触发，
//!   `importer` feature 下编译）；
//! - 入口层（如 `ramaria-mcp`）只做协议包装，不承载业务逻辑。

pub mod behavior;
pub mod bridge;
pub mod browse;
pub mod chat;
pub mod config;
pub mod diagnostics;
pub mod engine;
pub mod error_hint;
pub mod eta;
pub mod export;
pub mod fact_extract;
pub mod feedback;
pub mod hooks;
pub mod idle;
pub mod index;
pub mod ingest;
pub mod keyword;
pub mod l2;
pub mod lifecycle;
pub mod model;
pub mod persona;
pub mod privacy;
pub mod proactive;
pub mod recall;
pub mod seal;
pub mod session;
pub mod settings;
pub mod setup;
pub mod stream_event;
pub mod style;
pub mod types;
pub mod update;
pub mod utt;

#[cfg(feature = "importer")]
pub mod import;

#[cfg(test)]
pub(crate) mod test_support;

pub use behavior::{BehaviorLearnOutcome, RuleEvidenceItem};
pub use config::{ConfigWriter, MismatchEntry, SyncOutcome, SyncWriteResult};
pub use diagnostics::{DiagnosticsReport, DiagnosticsRequest};
pub use engine::{Engine, EngineOptions};
pub use error_hint::{ErrorHint, entry_error_message, error_detail, error_title, is_retryable};
pub use eta::{EtaEstimator, PhaseEma, PhaseKind, linear_remaining};
pub use export::{
    EXPORT_FORMAT_VERSION, ExportData, ExportDataRequest, ExportSessionData, render_sessions_json,
    render_sessions_markdown,
};
pub use hooks::{default_seal_hooks, full_seal_hooks};
pub use idle::{IdleLoop, IdleLoopOptions, MIN_IDLE_CHECK_INTERVAL_SECONDS};
#[cfg(feature = "importer")]
pub use import::{
    AnalysisReport, AnalyzeRequest, ImportDoneSummary, ImportL0Outcome, ImportL1Outcome,
    ImportL1Plan, ImportL1Progress, ImportMode, ImportProgressSink, ImportRequest,
};
pub use index::IndexBuildFailure;
pub use lifecycle::{Lifecycle, LifecycleOptions};
pub use model::{
    BackendConfigWriteOptions, BackendConfigWriteOutcome, RemoveModelOutcome, download_model,
    is_model_ready, list_models, model_size, models_root, remove_model, validate_embedding_model,
};
pub use persona::{PersonaLoadMode, PersonaRegenerateOutcome};
pub use privacy::PrivacyStatus;
pub use proactive::{ProactiveMessage, ProactiveSink};
pub use recall::RecallPolicy;
pub use seal::{SealHook, SealHooks};
pub use settings::mask_api_key;
pub use stream_event::{ChatEventStream, ChatStreamHandle, StreamEvent};
pub use style::StyleStatsView;
pub use types::{
    AliasAction, AliasResolveOutcome, AliasResolveRequest, BehaviorRuleView, CHANNEL_MCP,
    ChannelOverviewView, ChatRole, ChatSendOutcome, ChatSendRequest, ChatStreamRequest, ChatTurn,
    DEFAULT_HISTORY_LIMIT, DEFAULT_MAX_CHARS, DEFAULT_MAX_ITEMS, DEFAULT_PERSONA_UID,
    DataMaturityView, DegradedReason, EmbeddingModelView, EmbeddingValidation, EvidenceEventView,
    EvidenceL1SourceView, FactBrowsePage, FactBrowseRequest, FactDetailView, FactEntryView,
    FactView, GroupedFactsView, HistoryMessageView, HistoryRequest, HistoryResult, IngestOutcome,
    IngestRequest, KeywordEntryView, KeywordPoolView, KeywordSeedItem, KeywordSeedOutcome,
    KeywordSuggestionOutcome, L1BrowsePage, L1BrowseRequest, L1MemoryView, L2BrowsePage,
    L2BrowseRequest, L2EventView, L3TraitView, MAX_ITEMS_LIMIT, PendingAliasView,
    PersonaCardRequest, PersonaCardView, PersonaFileAction, PersonaFileOutcome, PersonaFullView,
    PersonaSection, PersonaSummaryView, PersonaUpdateRequest, PersonalityProfileView,
    ProfileStatusView, RecallItem, RecallLayer, RecallMode, RecallRequest, RecallResult,
    RecallStats, SealOutcome, SessionBrowsePage, SessionBrowseRequest, SessionDetailView,
    SessionMessageView, SessionMessagesRequest, SessionMessagesView, SessionSummaryView,
    SetupRequest, SetupStatus, StyleView, TraitDetailView, TraitEvidenceRequest, TraitEvidenceView,
    TraitView,
};
pub use update::{UpdateStatus, check_update};
pub use utt::UttRebuildOutcome;
