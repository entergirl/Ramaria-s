//! crates/ramaria-service/src/chat/proactive/tests.rs - Ramaria 主动生成用例单元测试
//!
//! 设计特点:
//! - 由 chat::proactive 以 `#[cfg(test)] mod tests;` 收纳：覆盖门禁静默跳过 / assistant-only 落库 /
//!   提示词注入 / 失败不留半条
//! - 使用 mock LLM 与真实 SQLite 临时库，断言以落库状态与记录到的 LLM 请求为准
//! - 门禁用例断言"零写入 + 未触发 LLM"；成功用例断言"仅新增 1 条主动 assistant 行"
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实 API key / 网络调用 / 用户数据。

use super::*;
use crate::recall::RecallPolicy;
use crate::test_support::{
    MockLlm, engine_with_db, engine_with_failing_llm, engine_with_l1_reply, engine_with_shared_llm,
    seed_closed_session_with_messages, seed_persona, seed_session_with_messages,
};
use crate::types::DEFAULT_PERSONA_UID;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{
    AppState, BackendConfig, MessageRole, MessageSource, PrivacyConsent, now_ms,
};
use std::sync::Arc;
use uuid::Uuid;

/// 固定回复（供"生成成功"路径断言）。
const REPLY: &str = "刚路过一家花店，想起你说想学插花。";

/// 锚点文本（供提示词注入断言）。
const ANCHOR: &str = "上周提过想去看展";

/// 构造主动生成指令（固定来源 `event`、带选题键与锚点 / 角度 / 语气）。
fn directive(persona: &str, session_id: Option<Uuid>) -> ProactiveDirective {
    ProactiveDirective {
        persona: persona.to_string(),
        session_id,
        source: "event".to_string(),
        topic_key: Some("evt-1".to_string()),
        anchor: Some(ANCHOR.to_string()),
        angle: Some("轻问一句近况".to_string()),
        tone: Some("随意".to_string()),
        valence: 0.0,
    }
}

// =========================================================
// 成功路径（assistant-only 落库）
// =========================================================

/// 指定会话：成功生成并落库单条主动 assistant 消息（无新增用户行）。
#[tokio::test]
async fn success_persists_single_proactive_assistant_message() {
    let (engine, storage, dir) = engine_with_l1_reply("proactive-ok", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let session_id = seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, now_ms()).await;

    let outcome = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect("主动生成应成功")
        .expect("应产出主动消息");
    assert_eq!(outcome.session_id, session_id);
    assert_eq!(outcome.content, REPLY);
    assert_eq!(outcome.persona, DEFAULT_PERSONA_UID);
    assert_eq!(outcome.source, "event");
    assert_eq!(outcome.topic_key.as_deref(), Some("evt-1"), "选题键应透传");

    let messages = storage
        .list_messages(session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 3, "应为既有 2 条 + 1 条主动 assistant");
    let last = messages.last().expect("应有落库消息");
    assert_eq!(last.role, MessageRole::Assistant);
    assert!(last.is_proactive, "主动消息应带 is_proactive 标记");
    assert_eq!(last.source, MessageSource::Online);
    assert_eq!(last.persona_uid.as_deref(), Some(DEFAULT_PERSONA_UID));
    assert_eq!(last.content, REPLY);
    let user_count = messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .count();
    assert_eq!(user_count, 1, "主动生成不应新增用户行");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 新建路径（无 session_id）：新建会话并落库单条主动 assistant 消息。
#[tokio::test]
async fn new_session_path_creates_session_with_single_message() {
    let (engine, storage, dir) = engine_with_l1_reply("proactive-new", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let outcome = run(&engine, directive(DEFAULT_PERSONA_UID, None))
        .await
        .expect("主动生成应成功")
        .expect("应产出主动消息");

    let session = storage
        .get_session(outcome.session_id)
        .await
        .expect("读取会话成功")
        .expect("会话应存在");
    assert_eq!(session.persona_uid.as_deref(), Some(DEFAULT_PERSONA_UID));

    let messages = storage
        .list_messages(outcome.session_id)
        .await
        .expect("读取消息成功");
    assert_eq!(messages.len(), 1, "新建会话仅落库 1 条主动消息");
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(messages[0].is_proactive);

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 门禁静默跳过（不落库不投递）
// =========================================================

/// 隐私门禁：线上 provider 未确认时静默跳过；确认后放行进入生成。
#[tokio::test]
async fn privacy_gate_skips_silently_for_online_provider() {
    let llm = Arc::new(MockLlm::online());
    let (engine, storage, dir) = engine_with_shared_llm(
        "proactive-privacy",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let session_id = seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, now_ms()).await;

    let result = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect("跳过不应报错");
    assert!(result.is_none(), "未确认隐私应静默跳过");
    assert_eq!(llm.chat_calls(), 0, "未确认隐私不应调用 LLM");
    assert_eq!(
        storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功")
            .len(),
        2,
        "跳过不应产生任何写入"
    );

    // 写入确认后：门禁放行进入生成；online mock 的回复为空 → 空回复防御跳过。
    // 以"已进入生成（LLM 被调用）+ 无落库变化"证明隐私门禁放行，不混淆空回复语义。
    let backend = BackendConfig::deepseek_default();
    storage
        .save_privacy_consent(&PrivacyConsent::new(
            backend.provider,
            backend.base_url.clone(),
            true,
        ))
        .await
        .expect("写入隐私确认应成功");
    let result = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect("确认后不应报错");
    assert!(result.is_none(), "空回复不落库");
    assert!(llm.chat_calls() > 0, "确认后应进入生成（调用 LLM）");
    assert_eq!(
        storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功")
            .len(),
        2,
        "空回复不应落库"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 状态门禁：非 Ready 静默跳过且不触发 LLM。
#[tokio::test]
async fn state_gate_requires_ready() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "proactive-state",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session_id = seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, now_ms()).await;

    let result = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect("跳过不应报错");
    assert!(result.is_none(), "未就绪应静默跳过");
    assert_eq!(llm.chat_calls(), 0, "未就绪不应调用 LLM");
    assert_eq!(
        storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功")
            .len(),
        2,
        "跳过不应产生任何写入"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 人格白名单：不可见人格静默跳过。
#[tokio::test]
async fn persona_whitelist_skips() {
    let (engine, storage, dir) = engine_with_l1_reply("proactive-whitelist", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    engine.set_recall_policy(
        RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
    );

    let result = run(&engine, directive(DEFAULT_PERSONA_UID, None))
        .await
        .expect("跳过不应报错");
    assert!(result.is_none(), "白名单外人格应静默跳过");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 目标会话已关闭：静默跳过（消息数不变）。
#[tokio::test]
async fn closed_session_skips() {
    let (engine, storage, dir) = engine_with_l1_reply("proactive-closed", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let session_id =
        seed_closed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, now_ms()).await;

    let result = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect("跳过不应报错");
    assert!(result.is_none(), "已关闭会话应静默跳过");
    assert_eq!(
        storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功")
            .len(),
        2,
        "跳过不应产生任何写入"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 目标会话归属他人格：静默跳过（消息数不变）。
#[tokio::test]
async fn session_persona_mismatch_skips() {
    let (engine, storage, dir) = engine_with_l1_reply("proactive-mismatch", REPLY).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_persona(&storage, "char-0001").await;
    engine.set_state(AppState::Ready);
    let session_id = seed_session_with_messages(&storage, "char-0001", 2, now_ms()).await;

    let result = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect("跳过不应报错");
    assert!(result.is_none(), "归属不符应静默跳过");
    assert_eq!(
        storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功")
            .len(),
        2,
        "跳过不应产生任何写入"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 失败与空回复（不留半条）
// =========================================================

/// LLM 失败：错误上抛且消息数不变（失败不留半条）。
#[tokio::test]
async fn llm_failure_leaves_nothing() {
    let (engine, storage, dir) = engine_with_failing_llm("proactive-fail").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let session_id = seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, now_ms()).await;

    let err = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect_err("LLM 失败应上抛");
    assert_eq!(err.category(), "llm");
    assert_eq!(
        storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功")
            .len(),
        2,
        "LLM 失败不应写入任何消息"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空回复：LLM 成功但无内容时不落库。
#[tokio::test]
async fn empty_reply_skips_without_persisting() {
    let (engine, storage, dir) = engine_with_db("proactive-empty").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let session_id = seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, now_ms()).await;

    let result = run(&engine, directive(DEFAULT_PERSONA_UID, Some(session_id)))
        .await
        .expect("空回复不应报错");
    assert!(result.is_none(), "空回复应静默跳过");
    assert_eq!(
        storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功")
            .len(),
        2,
        "空回复不应落库"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 提示词注入
// =========================================================

/// 提示词注入：系统 Prompt 含主动开口段与锚点 / 角度；请求用户消息位为空。
#[tokio::test]
async fn anchor_and_angle_injected_into_prompt() {
    let llm = Arc::new(MockLlm::with_reply(REPLY));
    let (engine, storage, dir) = engine_with_shared_llm(
        "proactive-prompt",
        Arc::clone(&llm),
        RamariaConfig::default(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let outcome = run(&engine, directive(DEFAULT_PERSONA_UID, None))
        .await
        .expect("主动生成应成功")
        .expect("应产出主动消息");
    assert_eq!(outcome.content, REPLY);

    let recorded = llm.requests();
    let last = recorded.last().expect("应记录一次 LLM 请求");
    assert!(
        last.system_prompt.contains("# 主动开口"),
        "系统 Prompt 应含主动开口段"
    );
    assert!(
        last.system_prompt.contains(ANCHOR),
        "系统 Prompt 应含锚点文本"
    );
    assert!(
        last.system_prompt.contains("开口角度："),
        "系统 Prompt 应含开口角度行"
    );
    assert!(
        last.user_message.is_empty(),
        "assistant-only：请求用户消息位应为空"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
