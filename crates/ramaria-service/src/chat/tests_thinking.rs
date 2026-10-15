//! crates/ramaria-service/src/chat/tests_thinking.rs - Ramaria 思考适配装配单元测试
//!
//! 设计特点:
//! - 覆盖 `[backend]` 思考适配三键在请求装配中的效果：思考开关 / 角色沉浸注入 / 对话温度
//! - 角色沉浸注入断言以最终 `ChatRequest` 为准（system 末尾 / 首条 user 消息末尾 / 幂等）
//! - 条件注入：仅 DeepSeek 生效；本地 provider 即使开关打开也不注入
//! - 使用真实 SQLite 临时库与记录请求的 mock LLM；通道式入口与交互式装配同源
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实 API key / 网络调用 / 用户数据。

use super::context::{ROLE_IMMERSION_DIRECTIVE, ROLE_IMMERSION_MARKER};
use crate::test_support::{
    MockLlm, engine_with_shared_llm, seed_persona, seed_session_with_messages,
};
use crate::types::{CHANNEL_MCP, ChatSendRequest, DEFAULT_PERSONA_UID};
use ramaria_core::config::{RamariaConfig, RoleImmersion};
use ramaria_core::traits::{ChatMessage, ChatRequest, StoreCrud};
use ramaria_core::types::{
    BackendConfig, LlmProvider, Message, MessageRole, MessageSource, now_ms,
};
use std::sync::Arc;
use uuid::Uuid;

// =========================================================
// 辅助
// =========================================================

/// 构造通道式生成请求（显式会话 / 新会话两种形态）。
fn request(message: &str, session_id: Option<Uuid>) -> ChatSendRequest {
    ChatSendRequest {
        message: message.to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        session_id,
        conversation_id: None,
        channel: CHANNEL_MCP.to_string(),
    }
}

/// DeepSeek 口径生效配置：仅覆盖 `[backend]` 思考适配键（其余保持默认）。
fn deepseek_config(chat_thinking: bool, role_immersion: RoleImmersion) -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.backend.provider = LlmProvider::DeepSeek;
    config.backend.chat_thinking = chat_thinking;
    config.backend.role_immersion = role_immersion;
    config
}

/// 运行一次生成并返回最后一次 LLM 请求与临时目录（调用方负责清理）。
async fn run_and_capture(
    tag: &str,
    llm: &Arc<MockLlm>,
    config: RamariaConfig,
    session_id: Option<Uuid>,
) -> (ChatRequest, std::path::PathBuf) {
    let (engine, storage, dir) = engine_with_shared_llm(tag, Arc::clone(llm), config, None).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    engine
        .chat_send(request("你好", session_id))
        .await
        .expect("生成调用应成功");
    let recorded = llm.requests();
    let last = recorded.last().expect("应记录一次 LLM 请求").clone();
    (last, dir)
}

/// 统计请求全量文本中的角色沉浸标记次数（system prompt + 历史 + 当前用户消息）。
fn count_immersion_markers(request: &ChatRequest) -> usize {
    let mut total = request.system_prompt.matches(ROLE_IMMERSION_MARKER).count();
    total += request.user_message.matches(ROLE_IMMERSION_MARKER).count();
    for message in &request.history {
        total += message.content.matches(ROLE_IMMERSION_MARKER).count();
    }
    total
}

/// 取消息数组内首条 user 消息。
fn first_user_message(request: &ChatRequest) -> Option<&ChatMessage> {
    request
        .history
        .iter()
        .find(|message| message.role == MessageRole::User)
}

// =========================================================
// 默认路径与 provider 门禁
// =========================================================

/// 默认配置（LmStudio）→ 请求不含沉浸指令，`enable_thinking` 非 `Some(true)`。
#[tokio::test]
async fn default_local_provider_keeps_request_baseline() {
    let llm = Arc::new(MockLlm::local());
    let (last, dir) = run_and_capture(
        "chat-thinking-default",
        &llm,
        RamariaConfig::default(),
        None,
    )
    .await;

    assert_ne!(last.enable_thinking, Some(true), "默认配置不开启思考模式");
    assert_eq!(
        count_immersion_markers(&last),
        0,
        "默认配置不含角色沉浸指令"
    );
    assert!(
        (last.temperature - BackendConfig::lm_studio_default().temperature).abs() < f64::EPSILON,
        "未配置对话温度时应沿用后端 temperature"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 本地 provider（LmStudio）即使打开思考适配开关也不注入、不开启思考（条件注入门禁）。
#[tokio::test]
async fn local_provider_ignores_thinking_adaptation_switches() {
    let llm = Arc::new(MockLlm::local());
    let mut config = RamariaConfig::default();
    config.backend.chat_thinking = true;
    config.backend.role_immersion = RoleImmersion::User;

    let (last, dir) = run_and_capture("chat-thinking-local-gate", &llm, config, None).await;

    assert_ne!(
        last.enable_thinking,
        Some(true),
        "非 DeepSeek 不开启思考模式"
    );
    assert_eq!(
        count_immersion_markers(&last),
        0,
        "非 DeepSeek 不注入角色沉浸指令"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 角色沉浸注入（DeepSeek）
// =========================================================

/// DeepSeek + `role_immersion = user` + 有历史 user 消息 → 指令拼到首条 user 消息末尾。
#[tokio::test]
async fn deepseek_user_immersion_appends_to_first_history_user() {
    let llm = Arc::new(MockLlm::online());
    let config = deepseek_config(false, RoleImmersion::User);
    let (engine, storage, dir) =
        engine_with_shared_llm("chat-thinking-user", Arc::clone(&llm), config, None).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session_id =
        seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 4, now_ms() - 4_000).await;

    engine
        .chat_send(request("你好", Some(session_id)))
        .await
        .expect("生成调用应成功");
    let recorded = llm.requests();
    let last = recorded.last().expect("应记录一次 LLM 请求");

    let first_user = first_user_message(last).expect("历史应含 user 消息");
    assert!(
        first_user.content.ends_with(ROLE_IMMERSION_DIRECTIVE),
        "指令应拼在首条 user 消息末尾: {}",
        first_user.content
    );
    assert_eq!(count_immersion_markers(last), 1, "指令应仅出现一次");
    assert!(
        !last.system_prompt.contains(ROLE_IMMERSION_MARKER),
        "user 版不应注入 system prompt"
    );
    assert!(
        !last.user_message.contains(ROLE_IMMERSION_MARKER),
        "有历史 user 消息时不应退回当前用户消息"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// DeepSeek + `role_immersion = system` → 指令拼到 system prompt 末尾。
#[tokio::test]
async fn deepseek_system_immersion_appends_to_system_prompt() {
    let llm = Arc::new(MockLlm::online());
    let config = deepseek_config(false, RoleImmersion::System);
    let (last, dir) = run_and_capture("chat-thinking-system", &llm, config, None).await;

    assert!(
        last.system_prompt.ends_with(ROLE_IMMERSION_DIRECTIVE),
        "指令应拼在 system prompt 末尾"
    );
    assert_eq!(count_immersion_markers(&last), 1, "指令应仅出现一次");
    assert!(
        !last.user_message.contains(ROLE_IMMERSION_MARKER),
        "system 版不应注入用户消息"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 幂等：目标（首条 user）已含标记 → 不重复追加。
#[tokio::test]
async fn deepseek_user_immersion_is_idempotent() {
    let llm = Arc::new(MockLlm::online());
    let config = deepseek_config(false, RoleImmersion::User);
    let (engine, storage, dir) =
        engine_with_shared_llm("chat-thinking-idempotent", Arc::clone(&llm), config, None).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let session = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("创建会话应成功");
    let seeded = format!("原有内容{ROLE_IMMERSION_MARKER}原有内容");
    let mut message = Message::new(
        session.id,
        MessageRole::User,
        seeded.clone(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
    message.created_at = now_ms() - 1_000;
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");

    engine
        .chat_send(request("你好", Some(session.id)))
        .await
        .expect("生成调用应成功");
    let recorded = llm.requests();
    let last = recorded.last().expect("应记录一次 LLM 请求");

    let first_user = first_user_message(last).expect("历史应含 user 消息");
    assert_eq!(first_user.content, seeded, "已含标记时不应重复追加指令");
    assert_eq!(count_immersion_markers(last), 1, "标记应保持一次");

    let _ = std::fs::remove_dir_all(&dir);
}

/// DeepSeek + `role_immersion = user` + 无历史 user 消息 → 指令拼到当前用户消息末尾。
#[tokio::test]
async fn deepseek_user_immersion_falls_back_to_current_message() {
    let llm = Arc::new(MockLlm::online());
    let config = deepseek_config(false, RoleImmersion::User);
    let (last, dir) = run_and_capture("chat-thinking-fallback", &llm, config, None).await;

    assert!(last.history.is_empty(), "新会话应无历史消息");
    assert!(
        last.user_message.ends_with(ROLE_IMMERSION_DIRECTIVE),
        "无历史 user 消息时指令应拼到当前用户消息末尾"
    );
    assert_eq!(count_immersion_markers(&last), 1, "指令应仅出现一次");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 思考开关与对话温度（DeepSeek）
// =========================================================

/// DeepSeek + `chat_thinking = true` → 请求开启思考模式。
#[tokio::test]
async fn deepseek_chat_thinking_enables_request_flag() {
    let llm = Arc::new(MockLlm::online());
    let config = deepseek_config(true, RoleImmersion::Off);
    let (last, dir) = run_and_capture("chat-thinking-enabled", &llm, config, None).await;

    assert_eq!(
        last.enable_thinking,
        Some(true),
        "开启 chat_thinking 后请求应标记思考模式"
    );
    assert_eq!(
        count_immersion_markers(&last),
        0,
        "role_immersion = off 不注入指令"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// DeepSeek + `chat_temperature` 配置 → 覆盖对话温度（未配置时沿用后端值）。
#[tokio::test]
async fn deepseek_chat_temperature_overrides_backend_temperature() {
    let llm = Arc::new(MockLlm::online());
    let mut config = deepseek_config(false, RoleImmersion::Off);
    config.backend.chat_temperature = Some(1.0);
    let (last, dir) = run_and_capture("chat-thinking-temperature", &llm, config, None).await;

    assert!(
        (last.temperature - 1.0).abs() < f64::EPSILON,
        "配置对话温度应覆盖后端 temperature"
    );
    assert_ne!(
        last.enable_thinking,
        Some(true),
        "未开启 chat_thinking 时思考保持关闭"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
