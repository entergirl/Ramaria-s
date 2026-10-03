//! crates/ramaria-service/src/chat/context.rs - Ramaria 生成上下文素材与提示装配
//!
//! 设计特点:
//! - 记忆召回走共用 `assemble_recall`（与在线管线同一份实现），返回记忆上下文 / 文档覆盖集合 /
//!   原文片段；嵌入缺失 / 无命中等降级由共用实现内部处理
//! - 素材装载：脉络 / 行为 / 知识 / 示例四类素材，任一环节关闭或失败均静默降级（返回空素材）
//! - Prompt 装配：普通路径与协调预算路径共用同一素材结构，协调结果回写记忆上下文
//! - Token 预算：上下文窗口与输出上限取后端配置，协调路径放开整条 system_prompt 句截断
//! - 隐私：日志只记人格与长度口径，不记素材与提示词内容

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatMessage, ChatRequest, StorageBackend};
use ramaria_core::types::{BackendConfig, Message, MessageSource, now_ms};
use ramaria_memory::chat::{
    LoadedPromptMaterial, PromptMaterialInputs, build_system_prompt, load_examples_for_input,
    load_narrative_material, load_prompt_material,
};
use ramaria_memory::prompt::PROMPT_TEMPLATE_VERSION;
use ramaria_memory::prompt::builder::assemble_prompt_coordinated;
use ramaria_memory::recall::{RecallGates, RecallInput, RecallMemoryLayers, assemble_recall};
use ramaria_memory::token_budget::{self, TokenBudgetConfig};
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// 步骤实现：记忆召回与素材装载
// =========================================================

/// 步骤 6：记忆召回（共用 `assemble_recall`），返回记忆上下文 / 文档覆盖集合 / 原文片段。
///
/// 闸门口径:
/// - `memory_rag` 取 `[injection].memory_rag`（配置驱动）；
/// - `utt` = `[injection].utt` × `[utt].enabled` × 宿主策略 `allow_raw_text`；
/// - 摘要路两层（L1 + L2）全开。
///
/// 说明:
/// - 索引懒加载失败上抛（召回前置不可绕过）；
/// - 其余降级（嵌入缺失 / 无命中 / 白名单外）由共用实现内部处理，返回空值。
pub(super) async fn step_recall(
    engine: &Engine,
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    persona: &str,
    message: &str,
) -> RamariaResult<(Option<String>, Vec<String>, Option<String>)> {
    // 无检索输入（主动生成的轻触达场景无锚点）：跳过召回，避免空查询触发嵌入调用；
    // 既有对话路径消息非空，行为不变
    if message.trim().is_empty() {
        tracing::debug!(persona = %persona, "查询为空，跳过记忆召回");
        return Ok((None, Vec::new(), None));
    }

    engine.ensure_index_loaded().await?;

    // 嵌入 provider 取快照后在锁外使用（缺失 → 共用召回内部降级为 BM25 + 关键词镜像）
    let embedding = engine.embedding_ref();
    let policy = engine.recall_policy();
    let retriever = engine.retriever_slot();
    let keyword_mirror = engine.keyword_mirror();
    let recall = assemble_recall(RecallInput {
        retriever: &*retriever,
        keyword_mirror: &*keyword_mirror,
        storage,
        embedding: embedding.as_deref(),
        query: message,
        persona_uid: Some(persona),
        retrieval: &config.retrieval,
        decay: &config.decay,
        utt: &config.utt,
        gates: RecallGates {
            memory_rag: config.injection.memory_rag,
            utt: config.injection.utt && config.utt.enabled && policy.allow_raw_text,
        },
        memory_layers: RecallMemoryLayers::both(),
        now_ms: now_ms(),
    })
    .await;

    let memory_context = recall.memory_context;
    // 知识层去重参照：仅当 RAG 摘要真正注入时才消费覆盖集合
    let doc_labels = if memory_context.is_some() {
        recall.doc_labels
    } else {
        Vec::new()
    };
    Ok((memory_context, doc_labels, recall.utt_context))
}

/// 步骤 8：脉络素材（跨会话近期摘要 + 最后活跃时间）。
///
/// 说明:
/// - 注入闸门 / 加权检索 / "最近 N 条"回退语义由记忆层单份实现承担；
/// - 任何降级路径返回空素材而非错误（不阻塞生成）。
pub(super) async fn step_load_narrative(
    engine: &Engine,
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    persona: &str,
    message: &str,
) -> ramaria_memory::chat::NarrativeMaterial {
    let retriever = engine.retriever_slot();
    load_narrative_material(
        storage,
        &*retriever,
        &config.retrieval,
        &config.decay,
        config.injection.narrative,
        persona,
        message,
    )
    .await
}

/// 步骤 9：行为层情境路由（合并主 / 次规则；关闭或失败返回 None）。
///
/// 说明:
/// - 路由输入取本轮 prompt 历史（查询构造只用角色与内容）；
/// - `[injection].behavior` 或 `[behavior].enabled` 关闭时不做任何查询。
pub(super) async fn step_route_behavior(
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

    // 嵌入 provider 取快照后在锁外使用（缺失 → 行为路由走无向量通道）
    let embedding = engine.embedding_ref();
    match ramaria_memory::behavior::orchestrate::route(
        storage,
        &config.behavior,
        embedding.as_deref(),
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

/// 步骤 10：知识层判定器检索（关闭 / 未命中 / 失败 → 空，不注入知识块）。
pub(super) async fn step_load_knowledge(
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    persona: &str,
    message: &str,
) -> Vec<ramaria_core::types::PersonaFact> {
    if config.injection.knowledge && config.knowledge.auto_fact_detect {
        ramaria_memory::fact::retriever::load_knowledge_facts_for_query(
            storage,
            &config.knowledge,
            persona,
            message,
        )
        .await
    } else {
        Vec::new()
    }
}

/// 步骤 11：示例预选（注入闸门关闭 → 空；记忆命中 → 兜底不注入）。
pub(super) async fn step_load_examples(
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    persona: &str,
    message: &str,
    memory_hit: bool,
) -> Vec<ramaria_core::types::PersonaExample> {
    if config.injection.examples {
        load_examples_for_input(
            storage,
            &config.examples,
            Some(persona),
            message,
            memory_hit,
        )
        .await
    } else {
        Vec::new()
    }
}

// =========================================================
// 步骤实现：Prompt 装配与 Token 预算
// =========================================================

/// 步骤 12：系统 Prompt 装配（普通路径 / 协调预算路径）。
///
/// 返回:
/// - `(system_prompt, memory_context)`；协调路径可能裁剪 / 丢弃 RAG，返回值即最终记忆上下文
///   （普通路径原样返回传入值）。
pub(super) async fn step_build_prompt(
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    inputs: &PromptMaterialInputs<'_>,
    memory_context: Option<String>,
) -> (String, Option<String>) {
    if config.injection_budget.enabled {
        // 协调路径：RAG 基座 + 注入层在统一 token 池内按可配顺序分配
        // （替代"整条 system_prompt 事后无差别截断 + RAG 独立成摊"）
        let built = match load_prompt_material(storage, inputs).await {
            LoadedPromptMaterial::Plain(prompt) => {
                // 纯文本降级仅对 RAG 做协调裁剪（无注入层块）
                let alloc = token_budget::allocate_injection_budget(
                    &[],
                    memory_context.as_deref(),
                    &config.injection_budget,
                );
                ramaria_memory::prompt::builder::CoordinatedPrompt {
                    system_prompt: prompt,
                    memory_context: alloc.memory_context,
                    dropped: alloc.dropped,
                    injected_tokens: alloc.injected_tokens,
                    fallback_truncated: alloc.fallback_truncated,
                }
            }
            LoadedPromptMaterial::Structured(ctx, render_cfg) => assemble_prompt_coordinated(
                &ctx,
                &render_cfg,
                &config.injection_budget,
                memory_context.as_deref(),
            ),
        };
        tracing::debug!(
            dropped = ?built.dropped,
            fallback_truncated = built.fallback_truncated,
            injected_tokens = built.injected_tokens,
            "注入协调预算已应用（system_prompt 内注入 + RAG）"
        );
        return (built.system_prompt, built.memory_context);
    }

    let system_prompt = build_system_prompt(storage, inputs).await;
    (system_prompt, memory_context)
}

/// 步骤 13a：Token 预算裁剪（上下文窗口与输出上限取后端配置）。
///
/// 说明:
/// - 协调路径已在装配期约束 system_prompt 内注入，此处放开整条句截断，
///   避免对协调结果二次无差别裁剪；
/// - 超窗时记 warn（可能发生截断），不阻塞生成。
pub(super) fn step_apply_token_budget(
    config: &RamariaConfig,
    backend: &BackendConfig,
    system_prompt: &str,
    memory_context: Option<&str>,
    history: &[ChatMessage],
    message: &str,
    request_id: Uuid,
) -> token_budget::BudgetedContext {
    let context_window = backend.capability.context_window as usize;
    let mut budget_config = TokenBudgetConfig::new(context_window, backend.max_tokens);
    if config.injection_budget.enabled {
        budget_config.system_prompt_reserve = context_window;
    }
    let budgeted = token_budget::apply_token_budget(
        system_prompt,
        memory_context,
        history,
        message,
        &budget_config,
    );

    if budgeted.estimated_tokens > context_window {
        tracing::warn!(
            request_id = %request_id,
            estimated = budgeted.estimated_tokens,
            window = context_window,
            "token 预算超出上下文窗口，可能发生截断"
        );
    }
    tracing::debug!(
        request_id = %request_id,
        estimated_tokens = budgeted.estimated_tokens,
        context_window = context_window,
        history_kept = budgeted.history.len(),
        history_original = history.len(),
        "token 预算已应用"
    );

    budgeted
}

/// 步骤 13b：构建 `ChatRequest`（预算裁剪结果 + 后端生成参数）。
pub(super) fn step_build_request(
    backend: &BackendConfig,
    budgeted: token_budget::BudgetedContext,
    message: &str,
    request_id: Uuid,
) -> ChatRequest {
    ChatRequest {
        system_prompt: budgeted.system_prompt,
        memory_context: budgeted.memory_context,
        history: budgeted.history,
        user_message: message.to_string(),
        temperature: backend.temperature,
        max_tokens: backend.max_tokens,
        request_id,
        template_version: PROMPT_TEMPLATE_VERSION.to_string(),
    }
}
