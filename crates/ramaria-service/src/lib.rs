//! crates/ramaria-service/src/lib.rs - Ramaria 记忆与对话服务层入口
//!
//! 设计特点:
//! - 与传输无关的能力层：召回、写入、封存、会话解析、空闲检查、人格读取六类用例
//! - 依赖方向单向：入口（CLI / 桌面 / MCP / 未来社交通道）→ service → 内核，反向禁止
//! - 禁止依赖 `ramaria-app` / `ramaria-cli` / `ramaria-desktop` / `tauri`（分层纪律）
//! - 请求与响应为纯数据结构（`types` 模块），不出现 stdio / Tauri / HTTP 概念
//! - 通道（channel / external_ref）是数据属性而非算法输入
//! - 召回同源：记忆层检索复用 `ramaria_memory::recall`（在线管线同一份实现）
//! - 降级纪律：LLM / embedding 不可用时不阻塞装配与记忆读取
//!
//! 模块划分:
//! - `engine`：依赖装配（storage / config / LLM / embedding / 检索槽）与用例入口；
//! - `index`：检索索引懒加载、代次刷新与 L1 增量镜像（召回前置）；
//! - `model`：模型管理用例（后端配置热更新、嵌入模型校验 / 加载 / 读取、降级原因）；
//! - `setup`：首次配置用例（缺项诊断、状态机推进、后端健康探测）；
//! - `recall` / `chat` / `ingest` / `seal` / `idle` / `session` / `persona`：用例实现；
//!   空闲检查同时提供宿主循环（`IdleLoop`），供长驻进程免外部驱动自动封存超时会话；
//! - `l2`：L2 事件提取触发（无 app 宿主的运行时用）；
//! - `lifecycle`：会话生命周期容器（活跃指针 / 手动关闭 / 空闲检查线程 / L2-L3 调度 / 关停）
//!   与 L1 摘要重生成 / 补扫，后台定时链路与手动触发共用；
//! - `fact_extract`：知识事实自动抽取编排（`[knowledge].auto_fact_detect` 增强层）；
//! - `hooks`：默认封存钩子装配（行为 / 风格 / L2，供 MCP 等入口注册）；
//! - 入口层（如 `ramaria-mcp`）只做协议包装，不承载业务逻辑。

pub mod chat;
pub mod engine;
pub mod fact_extract;
pub mod hooks;
pub mod idle;
pub mod index;
pub mod ingest;
pub mod l2;
pub mod lifecycle;
pub mod model;
pub mod persona;
pub mod recall;
pub mod seal;
pub mod session;
pub mod setup;
pub mod types;

#[cfg(test)]
pub(crate) mod test_support;

pub use engine::{Engine, EngineOptions};
pub use hooks::default_seal_hooks;
pub use idle::{IdleLoop, IdleLoopOptions, MIN_IDLE_CHECK_INTERVAL_SECONDS};
pub use lifecycle::{Lifecycle, LifecycleOptions};
pub use model::validate_embedding_model;
pub use recall::RecallPolicy;
pub use seal::{SealHook, SealHooks};
pub use types::{
    BehaviorRuleView, CHANNEL_MCP, ChatRole, ChatSendOutcome, ChatSendRequest, ChatTurn,
    DEFAULT_HISTORY_LIMIT, DEFAULT_MAX_CHARS, DEFAULT_MAX_ITEMS, DEFAULT_PERSONA_UID,
    DataMaturityView, DegradedReason, EmbeddingModelView, EmbeddingValidation, FactView,
    HistoryMessageView, HistoryRequest, HistoryResult, IngestOutcome, IngestRequest,
    MAX_ITEMS_LIMIT, PersonaCardRequest, PersonaCardView, PersonaSection, PersonaSummaryView,
    RecallItem, RecallLayer, RecallMode, RecallRequest, RecallResult, RecallStats, SealOutcome,
    SessionSummaryView, SetupRequest, SetupStatus, StyleView, TraitView,
};
