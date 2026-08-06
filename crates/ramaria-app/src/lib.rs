//! rust/crates/ramaria-app/src/lib.rs - Ramaria 应用编排层入口
//!
//! 设计特点:
//! - CLI 和 Desktop 共用的应用编排层，不直接处理 UI 展示
//! - `App` 结构体持有一切运行时依赖，提供核心对话用例 `send_message`
//! - 管理应用状态机: NeedsSetup → Indexing → Ready
//! - 管理 session 生命周期: 手动关闭、空闲自动关闭、L0→L3 管线触发
//! - 流式事件模型 `StreamEvent` 统一增量文本、完成信号和错误
//! - 错误提示映射 `ErrorHint` 将内部错误翻译为用户友好文本
//! - 隐私确认按 provider+base_url 粒度管理
//! - ModelManager 管理嵌入模型的下载、校验和目录管理
//!
//! 依赖方向:
//! - ramaria-core: 类型、trait、错误模型
//! - ramaria-storage: 持久化（通过 StorageBackend trait）
//! - ramaria-memory: 检索、RAG、衰减、摘要、推断
//! - ramaria-llm: LLM provider + keychain + embedding provider

pub mod app;
pub mod app_chat;
pub mod app_privacy;
pub mod app_retriever;
pub mod app_setup;
pub mod app_state;
pub mod config_sync;
pub mod diagnostics;
pub mod error_hint;
pub mod model_manager;
pub mod pipeline;
pub mod privacy;
pub mod session_lifecycle;
pub mod setup;
pub mod stages;
pub mod stream_event;
pub mod update;

// 重新导出核心类型
pub use app::{App, SendMessageStream};
pub use config_sync::{ConfigSyncService, MismatchEntry, SyncOutcome, SyncWriteResult};
pub use diagnostics::{DiagnosticsReport, export_diagnostics};
pub use error_hint::{ErrorHint, error_detail, error_title, is_retryable};
pub use model_manager::{
    DownloadProgress, MODEL_PRESETS, ModelManager, ModelPreset, default_models_root,
};
pub use pipeline::{
    LlmRawStream, PipelineContext, PipelineData, PipelineError, PipelineStage, SendMessagePipeline,
};
pub use privacy::{PrivacyStatus, check_privacy, confirm_privacy, require_privacy};
pub use session_lifecycle::SessionLifecycle;
pub use setup::{SetupStatus, check_setup_status, determine_state, run_setup};
pub use stages::{
    StageBuildPrompt, StageBuildRequest, StageCallLlm, StageCheckPrivacy, StageCheckState,
    StageLoadHistory, StagePersistMessage, StageResolveSession, StageRetrieveMemory,
    StageTokenBudget,
};
pub use stream_event::StreamEvent;
pub use update::{UpdateStatus, check_update};

/// 返回当前时间的 `YYYY-MM-DD HH:MM` 字符串（本地时区）。
///
/// 用途: 消息时间戳、System Prompt 当前时间等共享格式化。
pub(crate) fn now_timestamp_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}
