//! crates/ramaria-service/src/chat.rs - 生成用例（chat_send 的服务层实现）
//!
//! 设计特点:
//! - 与在线管线同源：记忆上下文走共用召回（`assemble_recall`），系统 Prompt 走
//!   `ramaria_memory::chat` 的共用装配（素材加载 / 五段式 / 示例预选 / 脉络素材同源）
//! - 会话口径复用回流写入：会话解析（含惰性封存体检）与 `ingest` 共用同一实现，
//!   保证"同一外部标识是否续写"在两个入口判断一致
//! - 失败不留半条：仅在 LLM 调用成功后才落库（用户消息 + 助手回复），
//!   LLM 失败时库内不产生孤立用户消息（与在线管线"流失败不保存"同口径）
//! - 降级纪律：行为路由 / 知识事实 / 示例 / 脉络任一环节失败或关闭均静默降级，
//!   不阻塞生成；嵌入缺失时召回走 BM25 + 关键词镜像（共用召回内部降级）
//! - 边界：协调注入预算与层间仲裁为桌面专属开关（默认关闭），本用例按默认装配路径执行
//! - 隐私：日志只记会话 id、人格与长度，不记消息与回复内容

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatMessage, ChatRequest, StorageBackend};
use ramaria_core::types::{
    BackendConfig, Message, MessageRole, MessageSource, Session, new_id, now_ms,
};
use ramaria_memory::chat::load_narrative_material;
use ramaria_memory::chat::{PromptMaterialInputs, build_system_prompt, load_examples_for_input};
use ramaria_memory::prompt::PROMPT_TEMPLATE_VERSION;
use ramaria_memory::recall::{RecallGates, RecallInput, RecallMemoryLayers, assemble_recall};
use ramaria_memory::token_budget::{self, TokenBudgetConfig};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{ChatSendOutcome, ChatSendRequest, DEFAULT_PERSONA_UID};

// =========================================================
// 用例入口
// =========================================================

/// 执行生成用例：以指定人格回复一条消息（记忆检索 + 五段式装配 + LLM）。
///
/// 流程:
/// 1. 参数与策略校验（空消息 / 人格白名单）；
/// 2. 会话定位（显式 `session_id` > `channel` + `conversation_id`，含惰性封存体检）；
/// 3. 载入会话历史（按 `[session].max_history_messages` 取尾部）；
/// 4. 共用召回装配记忆上下文（与在线管线同一份实现）；
/// 5. 脉络素材（近期摘要 + 最后活跃时间）与行为 / 知识 / 示例素材；
/// 6. 五段式系统 Prompt 装配 + token 预算裁剪；
/// 7. LLM 生成（非流式）→ 成功后落库用户消息与助手回复。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 生成请求（消息 / 人格 / 会话定位 / 通道）。
///
/// 返回:
/// - 成功时返回 `reply` / `session_id` / `chars`。
/// - 校验失败返回 `Validation`；人格越权返回 `Privacy`；LLM 不可用返回 `Llm`。
pub(crate) async fn run(engine: &Engine, req: ChatSendRequest) -> RamariaResult<ChatSendOutcome> {
    let message = req.message.trim();
    if message.is_empty() {
        return Err(RamariaError::validation("消息不能为空"));
    }

    let persona = normalize_persona(req.persona.as_deref());
    let policy = engine.recall_policy();
    if !policy.persona_allowed(&persona) {
        tracing::warn!(persona = %persona, "生成请求的人格不在可见白名单内，拒绝");
        return Err(RamariaError::privacy(format!(
            "人格 {persona} 不在可见白名单内（allowed_personas）"
        )));
    }

    let config = engine.config();
    let storage = engine.storage_ref().as_ref();

    // ---- 1. 会话定位（与回流写入共用解析口径；显式 session_id 另行校验归属） ----
    let session = resolve_session(engine, storage, &req, &persona).await?;

    // ---- 2. 历史（库内已有消息；本轮用户消息在生成成功后才落库） ----
    let history = load_history(
        storage,
        session.id,
        config.session.max_history_messages as usize,
    )
    .await;

    // ---- 3. 记忆上下文（共用召回；闸门口径与在线管线一致） ----
    engine.ensure_index_loaded().await?;
    let recall = assemble_recall(RecallInput {
        retriever: &**engine.retriever_slot(),
        keyword_mirror: &**engine.keyword_mirror_ref(),
        storage,
        embedding: engine.embedding_ref().map(|provider| provider.as_ref()),
        query: message,
        persona_uid: Some(&persona),
        retrieval: &config.retrieval,
        decay: &config.decay,
        utt: &config.utt,
        gates: RecallGates {
            memory_rag: true,
            utt: policy.allow_raw_text,
        },
        memory_layers: RecallMemoryLayers::both(),
        now_ms: now_ms(),
    })
    .await;

    let memory_context = recall.memory_context;
    // 知识层去重参照：仅当 RAG 摘要真正注入时才消费覆盖集合
    let rag_covered_labels = if memory_context.is_some() {
        recall.doc_labels
    } else {
        Vec::new()
    };
    let utt_context = recall.utt_context;

    // ---- 4. 脉络素材（跨会话近期摘要 + 最后活跃时间） ----
    let narrative = load_narrative_material(
        storage,
        &**engine.retriever_slot(),
        &config.retrieval,
        &config.decay,
        config.injection.narrative,
        &persona,
        message,
    )
    .await;

    // ---- 5. 行为层情境路由（关闭 / 未命中 / 失败 → 不注入行为块） ----
    let behavior_decision =
        route_behavior(engine, storage, config, &persona, session.id, &history).await;

    // ---- 6. 知识层判定器检索（关闭 / 未命中 / 失败 → 空，不注入知识块） ----
    let knowledge_facts = if config.injection.knowledge && config.knowledge.auto_fact_detect {
        ramaria_memory::fact::retriever::load_knowledge_facts_for_query(
            storage,
            &config.knowledge,
            &persona,
            message,
        )
        .await
    } else {
        Vec::new()
    };

    // ---- 7. 示例预选（关闭 → 空；记忆未命中时走兜底轮换） ----
    let examples = if config.injection.examples {
        load_examples_for_input(
            storage,
            &config.examples,
            Some(&persona),
            message,
            memory_context.is_some(),
        )
        .await
    } else {
        Vec::new()
    };

    // ---- 8. 系统 Prompt 装配（与在线管线默认路径同一份实现） ----
    let system_prompt = build_system_prompt(
        storage,
        &PromptMaterialInputs {
            persona_uid: Some(&persona),
            recent_summaries: &narrative.recent_summaries,
            last_active_at: narrative.last_active_at.as_deref(),
            utt_context: utt_context.as_deref(),
            // 桥接段是"上一会话尾部衔接"，外部通道按空闲切分会话，不注入桥接
            bridge_context: None,
            behavior_decision,
            examples,
            max_examples: config.examples.max_examples as usize,
            knowledge_facts,
            rag_covered_labels: &rag_covered_labels,
            knowledge_budget_chars: Some(config.knowledge.injection_budget_chars),
            injection: &config.injection,
            style_enabled: config.style.enabled,
            rag_text: memory_context.as_deref(),
            layer_dedup: &config.layer_dedup,
        },
    )
    .await;

    // ---- 9. Token 预算（上下文窗口与输出上限取后端配置） ----
    let backend = storage
        .get_backend_config()
        .await?
        .unwrap_or_else(BackendConfig::lm_studio_default);
    let context_window = backend.capability.context_window as usize;
    let budget_config = TokenBudgetConfig::new(context_window, backend.max_tokens);
    let budgeted = token_budget::apply_token_budget(
        &system_prompt,
        memory_context.as_deref(),
        &history,
        message,
        &budget_config,
    );

    // ---- 10. LLM 生成（非流式；MCP 等外部通道无流式回传能力） ----
    let request = ChatRequest {
        system_prompt: budgeted.system_prompt,
        memory_context: budgeted.memory_context,
        history: budgeted.history,
        user_message: message.to_string(),
        temperature: backend.temperature,
        max_tokens: backend.max_tokens,
        request_id: new_id(),
        template_version: PROMPT_TEMPLATE_VERSION.to_string(),
    };
    tracing::info!(
        session_id = %session.id,
        persona = %persona,
        input_chars = message.chars().count(),
        "生成用例开始（记忆检索与 Prompt 装配完成）"
    );
    let reply = match engine.llm_ref().chat(&request).await {
        Ok(reply) => reply,
        Err(e) => {
            // LLM 失败不落库：避免库内留下没有回复的孤立用户消息
            tracing::warn!(
                session_id = %session.id,
                error = %e,
                "LLM 生成失败，本次不写入任何消息"
            );
            return Err(e);
        }
    };

    // ---- 11. 落库（用户消息 + 助手回复；均带人格归属，供桌面按来源展示） ----
    let user_message = Message::new(
        session.id,
        MessageRole::User,
        message.to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(persona.clone()));
    storage.save_message(&user_message).await?;

    let reply_chars = reply.chars().count();
    let assistant_message = Message::new(
        session.id,
        MessageRole::Assistant,
        reply.clone(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(persona.clone()));
    storage.save_message(&assistant_message).await?;

    tracing::info!(
        session_id = %session.id,
        persona = %persona,
        reply_chars,
        "生成用例完成（消息已落库）"
    );

    Ok(ChatSendOutcome {
        reply,
        session_id: session.id,
        chars: reply_chars,
    })
}

// =========================================================
// 会话定位与历史
// =========================================================

/// 解析目标会话：显式 `session_id` 优先，否则按通道 + 外部标识续写或新建。
///
/// 说明:
/// - 显式 `session_id` 会校验存在性、是否已关闭、归属人格是否一致（串人格直接报错）；
/// - 通道路径复用回流写入的解析实现（含惰性封存体检与"标识被他人格占用则另起"）。
async fn resolve_session(
    engine: &Engine,
    storage: &dyn StorageBackend,
    req: &ChatSendRequest,
    persona: &str,
) -> RamariaResult<Session> {
    if let Some(id) = req.session_id {
        let session = storage
            .get_session(id)
            .await?
            .ok_or_else(|| RamariaError::validation(format!("会话不存在：{id}")))?;
        if session.ended_at.is_some() {
            return Err(RamariaError::validation(format!(
                "会话已关闭：{id}（请省略 session_id 由服务端续写或另起）"
            )));
        }
        if let Some(uid) = session.persona_uid.as_deref() {
            if uid != persona {
                return Err(RamariaError::validation(format!(
                    "会话归属人格为 {uid}，与请求人格 {persona} 不一致"
                )));
            }
        }
        return Ok(session);
    }

    crate::ingest::resolve_session(
        engine,
        &req.channel,
        req.conversation_id.as_deref(),
        persona,
    )
    .await
}

/// 载入会话历史（时间正序），按 `[session].max_history_messages` 取尾部窗口。
///
/// 说明:
/// - 读取失败记 warn 并按空历史继续（生成仍可用，只是缺少上下文）；
/// - 截断保留最新窗口：与在线管线的历史窗口口径一致（超长会话不整段入 prompt）。
async fn load_history(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    max_messages: usize,
) -> Vec<ChatMessage> {
    let messages = match storage.list_messages(session_id).await {
        Ok(messages) => messages,
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "读取会话历史失败，按空历史继续");
            return Vec::new();
        }
    };

    let start = messages.len().saturating_sub(max_messages.max(1));
    messages[start..]
        .iter()
        .map(|message| ChatMessage {
            role: message.role,
            content: message.content.clone(),
        })
        .collect()
}

/// 行为层情境路由（合并主 / 次规则；关闭或失败返回 None）。
///
/// 说明:
/// - 路由输入取本次会话历史（查询构造只用角色与内容），与在线管线口径一致；
/// - `[injection].behavior` 或 `[behavior].enabled` 关闭时不做任何查询。
async fn route_behavior(
    engine: &Engine,
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    persona: &str,
    session_id: Uuid,
    history: &[ChatMessage],
) -> Option<ramaria_memory::behavior::MergedDecision> {
    if !config.injection.behavior || !config.behavior.enabled {
        return None;
    }

    let route_messages: Vec<Message> = history
        .iter()
        .map(|message| {
            Message::new(
                session_id,
                message.role,
                message.content.clone(),
                MessageSource::Local,
            )
        })
        .collect();

    match ramaria_memory::behavior::orchestrate::route(
        storage,
        &config.behavior,
        engine.embedding_ref().map(|provider| provider.as_ref()),
        persona,
        &route_messages,
    )
    .await
    {
        Ok(result) if result.matched => result.primary.as_ref().map(|primary| {
            ramaria_memory::behavior::merge_route_targets(primary, &result.secondary)
        }),
        // 未命中 → 静默降级（不注入行为块）
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(persona = %persona, error = %e, "行为情境路由失败，静默降级不注入行为块");
            None
        }
    }
}

/// 归一化人格 uid（缺省取默认人格）。
fn normalize_persona(persona: Option<&str>) -> String {
    persona
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_PERSONA_UID)
        .to_string()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::RecallPolicy;
    use crate::test_support::{
        engine_with_db, engine_with_failing_llm, engine_with_l1_reply, seed_persona,
    };
    use crate::types::CHANNEL_MCP;
    use ramaria_core::traits::StoreCrud;

    /// 固定回复（供"生成成功"路径断言）。
    const REPLY: &str = "嗯，我在听。";

    /// 构造生成请求（默认人格 rama-0001、通道 mcp）。
    fn request(message: &str, conversation_id: Option<&str>) -> ChatSendRequest {
        ChatSendRequest {
            message: message.to_string(),
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            session_id: None,
            conversation_id: conversation_id.map(str::to_string),
            channel: CHANNEL_MCP.to_string(),
        }
    }

    /// 生成成功：返回回复、落库两条消息、会话带通道标识。
    #[tokio::test]
    async fn generates_reply_and_persists_both_messages() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-ok", REPLY).await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;

        let outcome = engine
            .chat_send(request("你好", Some("client-A")))
            .await
            .expect("生成应成功");
        assert_eq!(outcome.reply, REPLY);
        assert_eq!(outcome.chars, REPLY.chars().count());

        let session = storage
            .get_session(outcome.session_id)
            .await
            .expect("读取会话成功")
            .expect("会话应存在");
        assert_eq!(session.channel, CHANNEL_MCP, "外部生成会话应带通道标识");
        assert_eq!(session.external_ref.as_deref(), Some("client-A"));

        let messages = storage
            .list_messages(outcome.session_id)
            .await
            .expect("读取消息成功");
        assert_eq!(messages.len(), 2, "应写入用户消息与助手回复");
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].content, "你好");
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert_eq!(messages[1].content, REPLY);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同一外部对话标识：续写同一会话（多轮累计消息）。
    #[tokio::test]
    async fn reuses_same_session_for_same_conversation_id() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-reuse", REPLY).await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;

        let first = engine
            .chat_send(request("第一轮", Some("client-A")))
            .await
            .expect("首轮应成功");
        let second = engine
            .chat_send(request("第二轮", Some("client-A")))
            .await
            .expect("二轮应成功");
        assert_eq!(
            first.session_id, second.session_id,
            "同一标识应续写同一会话"
        );

        let messages = storage
            .list_messages(first.session_id)
            .await
            .expect("读取消息成功");
        assert_eq!(messages.len(), 4, "两轮对话共 4 条消息");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空消息：显式校验错误（不产生任何写入）。
    #[tokio::test]
    async fn rejects_empty_message() {
        let (engine, storage, dir) = engine_with_db("chat-empty").await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;

        let err = engine
            .chat_send(request("   ", None))
            .await
            .expect_err("空消息应报错");
        assert_eq!(err.category(), "validation");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// LLM 失败：错误上抛且不写半条（库内无孤立用户消息）。
    #[tokio::test]
    async fn llm_failure_leaves_no_partial_write() {
        let (engine, storage, dir) = engine_with_failing_llm("chat-fail").await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;

        let err = engine
            .chat_send(request("你好", Some("client-A")))
            .await
            .expect_err("LLM 失败应上抛");
        assert_eq!(err.category(), "llm");

        let session = storage
            .find_active_session_by_channel(CHANNEL_MCP, Some("client-A"))
            .await
            .expect("按通道查询成功")
            .expect("会话应已创建（便于重试）");
        let messages = storage
            .list_messages(session.id)
            .await
            .expect("读取消息成功");
        assert!(messages.is_empty(), "LLM 失败不应写入孤立用户消息");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 人格白名单：越权人格直接拒绝（Privacy）。
    #[tokio::test]
    async fn persona_whitelist_rejects_generation() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-whitelist", REPLY).await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        engine.set_recall_policy(
            RecallPolicy::default().with_allowed_personas(vec!["char-0001".to_string()]),
        );

        let err = engine
            .chat_send(request("你好", None))
            .await
            .expect_err("越权人格应被拒绝");
        assert_eq!(err.category(), "privacy");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 显式 session_id：串人格拒绝，归属一致则复用。
    #[tokio::test]
    async fn explicit_session_id_checks_persona_binding() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-session", REPLY).await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        seed_persona(&storage, "char-0001").await;

        let session = storage
            .create_session_in_channel(Some("char-0001"), CHANNEL_MCP, Some("client-B"))
            .await
            .expect("建会话成功");

        let mut mismatch = request("你好", None);
        mismatch.session_id = Some(session.id);
        let err = engine
            .chat_send(mismatch)
            .await
            .expect_err("会话归属人格不一致应报错");
        assert_eq!(err.category(), "validation");

        let mut same_persona = request("你好", None);
        same_persona.persona = Some("char-0001".to_string());
        same_persona.session_id = Some(session.id);
        let outcome = engine
            .chat_send(same_persona)
            .await
            .expect("归属一致应可复用会话");
        assert_eq!(outcome.session_id, session.id);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
