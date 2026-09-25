//! crates/ramaria-service/tests/parity/chat.rs - 对照路径：生成（chat_send）
//!
//! 设计特点:
//! - fixture 固定：persona + 空会话空间 + 固定脚本回复；外部对话标识只作"是否提供"布尔落盘，
//!   便于同一实现重复执行时的快照可比
//! - 快照只含稳定字段：回复与字符数 / 会话通道与消息数 / 消息序列（角色 + 内容）/
//!   Prompt 结构指标（各段长度、是否含记忆上下文、模板版本）；**不落 Prompt 全文**（隐私红线）
//! - 覆盖三条链路：基线一致（golden 冻结）、跨隔离环境等价（`assert_parity`）、
//!   会话续写（同一外部标识落在同一会话）、LLM 不可用时"不落半条"（库内不产生孤立用户消息）
//! - 输出入口：`snapshot_of` 是"某一实现在该 fixture 上的规范化输出"的唯一入口，
//!   同形状快照可直接送入 `assert_parity` 比对

use std::sync::Arc;

use ramaria_core::traits::{LlmProvider, StoreCrud};
use ramaria_service::types::ChatSendRequest;
use ramaria_service::{CHANNEL_MCP, DEFAULT_PERSONA_UID};
use serde_json::json;

use crate::support::{
    GoldenStore, ParityEnv, ParityError, ParityResult, ScriptedLlm, Snapshot, assert_parity,
    fixtures,
};

/// 场景名（同时作为 golden 基线文件名）。
const SCENARIO: &str = "chat_send_reply_and_persist";

/// 首轮用户消息（固定文本，便于历史与落库断言）。
const USER_MESSAGE: &str = "今天有点累，随便聊聊吧";

/// 脚本回复（短句，字符数断言简洁）。
const REPLY: &str = "嗯，我在听。";

// =========================================================
// 场景执行
// =========================================================

/// 造 fixture：persona（无会话；会话由生成用例按外部标识创建）。
async fn fixture(env: &ParityEnv) -> ParityResult<()> {
    fixtures::seed_persona(env.storage(), DEFAULT_PERSONA_UID).await
}

/// 构造生成请求（指定外部对话标识）。
fn request(conversation_id: &str) -> ChatSendRequest {
    ChatSendRequest {
        message: USER_MESSAGE.to_string(),
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        session_id: None,
        conversation_id: Some(conversation_id.to_string()),
        channel: CHANNEL_MCP.to_string(),
    }
}

/// 在给定环境上执行一轮生成，产出规范化快照。
///
/// 参数:
/// - `env`: 对照环境（脚本 LLM 由调用方构造并保留引用）。
/// - `llm`: 脚本 LLM 引用（用于读取本轮请求的 Prompt 结构指标）。
/// - `conversation_id`: 外部对话标识（决定会话续写或新建；不进入快照）。
async fn snapshot_of(
    env: &ParityEnv,
    llm: &ScriptedLlm,
    conversation_id: &str,
) -> ParityResult<Snapshot> {
    fixture(env).await?;

    let outcome = env
        .engine()
        .chat_send(request(conversation_id))
        .await
        .map_err(|e| ParityError::env("执行生成用例", e))?;

    let session = env
        .storage()
        .get_session(outcome.session_id)
        .await
        .map_err(|e| ParityError::env("读取生成会话", e))?
        .ok_or_else(|| ParityError::env("读取生成会话", "会话应存在"))?;
    let messages = env
        .storage()
        .list_messages(outcome.session_id)
        .await
        .map_err(|e| ParityError::env("读取生成会话消息", e))?;

    let requests = llm.requests();
    let last_request = requests
        .last()
        .ok_or_else(|| ParityError::env("读取 LLM 请求", "应至少记录一次请求"))?;

    Ok(Snapshot::new(
        SCENARIO,
        json!({
            "reply": outcome.reply,
            "chars": outcome.chars,
            "session": {
                "channel": session.channel,
                "has_external_ref": session.external_ref.is_some(),
                "message_count": messages.len(),
            },
            "messages": messages
                .iter()
                .map(|message| json!({
                    "role": message.role,
                    "content": message.content.clone(),
                }))
                .collect::<Vec<_>>(),
            "prompt": {
                "system_prompt_chars": last_request.system_prompt.chars().count(),
                "memory_context_present": last_request.memory_context.is_some(),
                "memory_context_chars": last_request.memory_context.as_ref().map(|text| text.chars().count()).unwrap_or(0),
                "history_len": last_request.history.len(),
                "user_message": last_request.user_message.clone(),
                "template_version": last_request.template_version.clone(),
            },
        }),
    ))
}

/// 构造"脚本回复"环境并返回 LLM 引用（供 Prompt 结构断言）。
async fn env_with_script(tag: &str) -> ParityResult<(ParityEnv, Arc<ScriptedLlm>)> {
    let llm = Arc::new(ScriptedLlm::reply(REPLY));
    let llm_dyn: Arc<dyn LlmProvider> = Arc::clone(&llm) as Arc<dyn LlmProvider>;
    let env = ParityEnv::with_llm(tag, llm_dyn).await?;
    Ok((env, llm))
}

// =========================================================
// 测试
// =========================================================

/// 基线一致：生成输出与冻结基线逐字段一致，且落库与字符数断言成立。
#[tokio::test]
async fn chat_snapshot_matches_golden_baseline() {
    let (env, llm) = env_with_script("chat-golden")
        .await
        .expect("生成对照环境应可构建");
    let snapshot = snapshot_of(&env, &llm, "parity-client")
        .await
        .expect("生成场景应执行成功");

    // 关键行为断言：回复与字符数一致、两条消息落库、Prompt 携带本轮输入
    assert_eq!(
        snapshot.value()["chars"].as_u64(),
        Some(REPLY.chars().count() as u64),
        "字符数应为回复字符数"
    );
    assert_eq!(
        snapshot.value()["session"]["message_count"].as_u64(),
        Some(2),
        "应落库用户消息与助手回复两条"
    );
    assert_eq!(
        snapshot.value()["prompt"]["user_message"].as_str(),
        Some(USER_MESSAGE),
        "Prompt 应携带本轮用户输入"
    );
    assert_eq!(llm.call_count(), 1, "一轮生成应只调用一次 LLM");

    let outcome = GoldenStore::new()
        .expect("基线仓库应可定位")
        .assert_or_record(&snapshot)
        .expect("基线比对或首次生成应成功");
    assert!(
        !outcome.is_updated(),
        "未开启更新模式时不应覆盖基线（{outcome:?}）"
    );
    tracing::info!(
        path = %outcome.path().display(),
        ?outcome,
        "生成基线比对完成"
    );

    env.cleanup().await;
}

/// 会话续写：同一外部对话标识的两轮生成应落在同一会话（历史累计 4 条消息）。
#[tokio::test]
async fn chat_reuses_session_for_same_conversation_id() {
    let (env, _llm) = env_with_script("chat-reuse")
        .await
        .expect("生成对照环境应可构建");
    fixture(&env).await.expect("fixture 应造数成功");

    let first = env
        .engine()
        .chat_send(request("parity-same-client"))
        .await
        .expect("首轮生成应成功");
    let second = env
        .engine()
        .chat_send(request("parity-same-client"))
        .await
        .expect("第二轮生成应成功");
    assert_eq!(
        first.session_id, second.session_id,
        "同一外部标识应续写同一会话"
    );

    let message_count = env
        .storage()
        .list_messages(first.session_id)
        .await
        .expect("读取会话消息应成功")
        .len();
    assert_eq!(message_count, 4, "两轮生成应累计 4 条消息");

    env.cleanup().await;
}

/// 独立产出等价：两个隔离环境各自执行一轮生成，输出应完全一致。
#[tokio::test]
async fn chat_isolated_envs_produce_equivalent_snapshots() {
    let (first_env, first_llm) = env_with_script("chat-parity-a")
        .await
        .expect("首个对照环境应可构建");
    let first = snapshot_of(&first_env, &first_llm, "parity-client")
        .await
        .expect("首轮生成应成功");
    first_env.cleanup().await;

    let (second_env, second_llm) = env_with_script("chat-parity-b")
        .await
        .expect("第二个对照环境应可构建");
    let second = snapshot_of(&second_env, &second_llm, "parity-client")
        .await
        .expect("第二轮生成应成功");
    second_env.cleanup().await;

    assert_parity("chat/send-reply-and-persist", &first, &second);
}

/// LLM 不可用：错误上抛且库内不写半条（不产生孤立用户消息）。
#[tokio::test]
async fn chat_llm_failure_leaves_no_partial_write() {
    let llm: Arc<dyn LlmProvider> = Arc::new(ScriptedLlm::failing("模拟 LLM 后端不可用"));
    let env = ParityEnv::with_llm("chat-failure", llm)
        .await
        .expect("生成对照环境应可构建");
    fixture(&env).await.expect("fixture 应造数成功");

    let error = env
        .engine()
        .chat_send(request("parity-client"))
        .await
        .expect_err("LLM 失败应上抛");
    assert_eq!(error.category(), "llm", "应返回 LLM 类错误: {error}");

    // 会话可能已创建（会话解析先于 LLM 调用），但任何会话内都不得出现消息
    let active_sessions = env
        .storage()
        .list_active_sessions()
        .await
        .expect("读取活跃会话应成功");
    for session in &active_sessions {
        let messages = env
            .storage()
            .list_messages(session.id)
            .await
            .expect("读取会话消息应成功");
        assert!(
            messages.is_empty(),
            "LLM 失败不应写入孤立用户消息（会话 {}）: {messages:?}",
            session.id
        );
    }

    env.cleanup().await;
}
