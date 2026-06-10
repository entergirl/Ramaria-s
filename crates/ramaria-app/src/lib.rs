//! rust/crates/ramaria-app/src/lib.rs - Ramaria 应用编排层入口
//!
//! 设计特点:
//! - CLI 和 Desktop 共用的应用编排层，不直接处理 UI 展示
//! - `App` 结构体持有一切运行时依赖，提供核心对话用例 `send_message`
//! - 管理应用状态机: NeedsSetup → Indexing → Ready
//! - 流式事件模型 `StreamEvent` 统一增量文本、完成信号和错误
//! - 错误提示映射 `ErrorHint` 将内部错误翻译为用户友好文本
//! - 隐私确认按 provider+base_url 粒度管理
//!
//! 依赖方向:
//! - ramaria-core: 类型、trait、错误模型
//! - ramaria-storage: 持久化（通过 StorageBackend trait）
//! - ramaria-memory: 检索、RAG、衰减、摘要
//! - ramaria-llm: LLM provider + keychain

pub mod app;
pub mod error_hint;
pub mod privacy;
pub mod setup;
pub mod stream_event;

// 重新导出核心类型
pub use app::{App, SendMessageStream};
pub use error_hint::{ErrorHint, error_detail, error_title, is_retryable};
pub use privacy::{PrivacyStatus, check_privacy, confirm_privacy, require_privacy};
pub use setup::{SetupStatus, check_setup_status, determine_state, run_setup};
pub use stream_event::StreamEvent;

// 保留旧占位函数（向后兼容，后续可移除）
pub use ramaria_core;

/// 模块存活检查 (Phase 1 占位，可后续移除)
pub fn hello_app() -> &'static str {
    "ramaria-app is ready"
}
