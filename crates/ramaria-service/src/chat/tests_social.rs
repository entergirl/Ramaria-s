//! crates/ramaria-service/src/chat/tests_social.rs - Ramaria 社交会话聊天语义单元测试
//!
//! 设计特点:
//! - 覆盖社交会话历史窗口的发言人前缀（`{sender_name}：`）与预置上文零变化
//! - 覆盖社交会话只读门禁（交互式一律拒绝；通道式仅放行入站回复链路）
//! - 使用真实 SQLite 临时库，直接调用步骤实现验证前置编排语义
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实 API key / 网络调用 / 用户数据。

use super::history::step_load_history;
use super::steps::{ChatInput, ChatMode, step_resolve_session};
use crate::test_support::{MockLlm, engine_with_db, engine_with_shared_llm, seed_persona};
use crate::types::{ChannelReplyRequest, DEFAULT_PERSONA_UID};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{Message, MessageRole, MessageSource, now_ms};
use ramaria_memory::chat::{CHANNEL_GROUP_DECLARATION, SegmentStyle};
use std::sync::Arc;

/// 构造通道式聊天输入（显式会话；其余字段按通道语义给值）。
fn channel_input(session_id: uuid::Uuid, message_in_history: bool) -> ChatInput {
    ChatInput {
        message: "你好".to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        session_id: Some(session_id),
        mode: ChatMode::Channel,
        channel: String::new(),
        conversation_id: None,
        seed_history: Vec::new(),
        message_in_history,
        segment_style: SegmentStyle::Pipe,
        proactive: None,
    }
}

/// 构造交互式聊天输入（显式会话）。
fn interactive_input(session_id: uuid::Uuid) -> ChatInput {
    ChatInput {
        message: "你好".to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        session_id: Some(session_id),
        mode: ChatMode::Interactive,
        channel: String::new(),
        conversation_id: None,
        seed_history: Vec::new(),
        message_in_history: false,
        segment_style: SegmentStyle::Pipe,
        proactive: None,
    }
}

/// 写入一条指定发言人 / 正文 / 时间的历史消息。
async fn save_message(
    storage: &std::sync::Arc<ramaria_storage::SqliteStorage>,
    session_id: uuid::Uuid,
    role: MessageRole,
    content: &str,
    sender_name: Option<&str>,
    created_at: i64,
) {
    let mut message = Message::new(session_id, role, content.to_string(), MessageSource::Local)
        .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
    message.sender_name = sender_name.map(str::to_string);
    message.created_at = created_at;
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
}

// =========================================================
// 历史窗口发言人前缀
// =========================================================

/// 社交会话：他人消息加 `{sender_name}：` 前缀；assistant / 无发言人消息零变化。
#[tokio::test]
async fn social_history_prefixes_sender_names() {
    let (engine, storage, dir) = engine_with_db("chat-social-prefix").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("group:20002"))
        .await
        .expect("建会话应成功");
    let base_ts = now_ms() - 10_000;

    save_message(
        &storage,
        session.id,
        MessageRole::User,
        "你好呀",
        Some("小明"),
        base_ts + 1,
    )
    .await;
    save_message(
        &storage,
        session.id,
        MessageRole::Assistant,
        "在呢",
        Some("rama"),
        base_ts + 2,
    )
    .await;
    save_message(
        &storage,
        session.id,
        MessageRole::User,
        "有人吗",
        Some("   "),
        base_ts + 3,
    )
    .await;
    save_message(
        &storage,
        session.id,
        MessageRole::User,
        "在吗",
        None,
        base_ts + 4,
    )
    .await;

    let config = engine.config();
    let history = step_load_history(storage.as_ref(), config.as_ref(), session.id, &[], true).await;

    assert_eq!(history.len(), 4);
    assert_eq!(history[0].content, "小明：你好呀", "他人消息应带发言人前缀");
    assert_eq!(history[1].content, "在呢", "assistant 消息不应加前缀");
    assert_eq!(history[2].content, "有人吗", "空白发言人不应加前缀");
    assert_eq!(history[3].content, "在吗", "发言人缺失不应加前缀");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 本地会话：即使带发言人字段也不加前缀（社交标注仅对社交通道启用）。
#[tokio::test]
async fn local_history_keeps_plain_text() {
    let (engine, storage, dir) = engine_with_db("chat-local-plain").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("建会话应成功");
    save_message(
        &storage,
        session.id,
        MessageRole::User,
        "你好呀",
        Some("小明"),
        now_ms(),
    )
    .await;

    let config = engine.config();
    let history =
        step_load_history(storage.as_ref(), config.as_ref(), session.id, &[], false).await;

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "你好呀", "本地会话正文应零变化");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 社交会话只读门禁
// =========================================================

/// 交互式：显式指定社交平台会话一律拒绝（文案提示仅可查看）。
#[tokio::test]
async fn interactive_rejects_social_session() {
    let (engine, storage, dir) = engine_with_db("chat-social-interactive").await;
    let session = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:10001"))
        .await
        .expect("建会话应成功");

    let config = engine.config();
    let input = interactive_input(session.id);
    let err = step_resolve_session(
        &engine,
        storage.as_ref(),
        config.as_ref(),
        &input,
        DEFAULT_PERSONA_UID,
    )
    .await
    .expect_err("社交会话应拒绝客户端本地生成");
    assert_eq!(err.category(), "validation");
    assert!(
        err.to_string().contains("社交平台"),
        "拒绝文案应说明社交平台只读: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 通道式：显式指定社交平台会话仅放行入站回复链路（`message_in_history=true`）。
#[tokio::test]
async fn channel_gate_only_allows_inbound_reply_path() {
    let (engine, storage, dir) = engine_with_db("chat-social-channel").await;
    let session = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:10001"))
        .await
        .expect("建会话应成功");
    let config = engine.config();

    // MCP chat_send 形态（message_in_history=false）：拒绝
    let blocked = channel_input(session.id, false);
    let err = step_resolve_session(
        &engine,
        storage.as_ref(),
        config.as_ref(),
        &blocked,
        DEFAULT_PERSONA_UID,
    )
    .await
    .expect_err("绕过入站回复路径应拒绝");
    assert_eq!(err.category(), "validation");
    assert!(err.to_string().contains("社交平台"));

    // 社交通道回复形态（message_in_history=true）：放行
    let allowed = channel_input(session.id, true);
    assert!(
        step_resolve_session(
            &engine,
            storage.as_ref(),
            config.as_ref(),
            &allowed,
            DEFAULT_PERSONA_UID,
        )
        .await
        .is_ok(),
        "入站回复链路应放行"
    );

    // 本地会话不受门禁约束
    let local = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("建会话应成功");
    let local_input = channel_input(local.id, false);
    assert!(
        step_resolve_session(
            &engine,
            storage.as_ref(),
            config.as_ref(),
            &local_input,
            DEFAULT_PERSONA_UID,
        )
        .await
        .is_ok(),
        "本地会话不应受社交门禁约束"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 群聊说明段（通道回复端到端）
// =========================================================

/// 群聊会话：通道回复提示词追加群聊说明段；私聊会话不追加。
#[tokio::test]
async fn channel_reply_prompt_declares_group_conversation() {
    let llm = Arc::new(MockLlm::with_reply("在呢"));
    let (engine, storage, dir) = engine_with_shared_llm(
        "chat-social-group-prompt",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let group = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("group:20002"))
        .await
        .expect("建群聊会话应成功");
    engine
        .channel_reply(ChannelReplyRequest {
            session_id: group.id,
            persona_uid: DEFAULT_PERSONA_UID.to_string(),
            message: "消息".to_string(),
        })
        .await
        .expect("群聊社交通道回复应成功");

    let requests = llm.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .system_prompt
            .contains(CHANNEL_GROUP_DECLARATION),
        "群聊会话提示词应含群聊说明段"
    );

    let private = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:10001"))
        .await
        .expect("建私聊会话应成功");
    engine
        .channel_reply(ChannelReplyRequest {
            session_id: private.id,
            persona_uid: DEFAULT_PERSONA_UID.to_string(),
            message: "消息".to_string(),
        })
        .await
        .expect("私聊社交通道回复应成功");

    let requests = llm.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[1]
            .system_prompt
            .contains(CHANNEL_GROUP_DECLARATION),
        "私聊会话不应含群聊说明段"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
