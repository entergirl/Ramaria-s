//! crates/ramaria-service/src/chat.rs - 生成用例（非流式 chat_send 与流式 chat_stream 的统一实现）
//!
//! 设计特点:
//! - 单一实现：非流式（通道入口）与流式（交互入口）共用同一份前置编排——参数与策略校验 /
//!   状态与隐私门禁 / 会话定位 / 历史窗口 / 记忆召回 / Prompt 装配 / Token 预算；
//!   宿主差异（门禁、桥接、弱反馈、流式转发）由模式参数表达
//! - 与在线管线同源：记忆召回走共用 `assemble_recall`，Prompt 走 `ramaria_memory::chat`
//!   的共用素材加载与渲染（示例预选 / 脉络素材同源）
//! - 历史窗口配置驱动：分页 20 条倒序加载，加载条数与字符预算取 `[session]` 配置
//! - 失败不留半条：非流式仅在 LLM 成功后落库；流式先落用户消息，
//!   助手回复仅在无错且非空时落库（用户消息落库失败经事件流传出错误）
//! - 降级纪律：行为路由 / 知识事实 / 示例 / 脉络任一环节失败或关闭均静默降级，不阻塞生成
//! - 隐私：日志只记会话 id、人格与长度，不记消息与回复内容

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::StreamExt;
use futures::channel::mpsc;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatMessage, ChatRequest, StorageBackend, StreamDelta};
use ramaria_core::types::{
    AppState, BackendConfig, Message, MessageRole, MessageSource, Session, new_id, now_ms,
};
use ramaria_memory::chat::LoadedPromptMaterial;
use ramaria_memory::chat::{
    PromptMaterialInputs, build_system_prompt, load_examples_for_input, load_narrative_material,
    load_prompt_material,
};
use ramaria_memory::prompt::PROMPT_TEMPLATE_VERSION;
use ramaria_memory::prompt::builder::assemble_prompt_coordinated;
use ramaria_memory::recall::{RecallGates, RecallInput, RecallMemoryLayers, assemble_recall};
use ramaria_memory::token_budget::{self, TokenBudgetConfig};
use uuid::Uuid;

use crate::engine::Engine;
use crate::stream_event::{ChatStreamHandle, StreamEvent};
use crate::types::{ChatSendOutcome, ChatSendRequest, ChatStreamRequest, DEFAULT_PERSONA_UID};

/// 历史加载分页条数（实现常量；加载条数与字符预算由 `[session]` 配置提供）。
const HISTORY_PAGE_SIZE: i64 = 20;

// =========================================================
// 内部请求形态
// =========================================================

/// 生成入口模式（宿主差异参数化）。
///
/// 变体:
/// - `Interactive`: 交互式入口（桌面 / CLI）——状态门禁 / 隐私门禁 / 新会话桥接 / 弱反馈检测开启；
/// - `Channel`: 通道式入口（MCP）——以上四项全关，会话按通道 + 外部标识解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatMode {
    Interactive,
    Channel,
}

/// 统一请求（两个入口共用的内部形态）。
struct ChatInput {
    message: String,
    persona: Option<String>,
    session_id: Option<Uuid>,
    mode: ChatMode,
    /// 通道式入口的会话来源通道（交互式入口为空串，不使用该字段）。
    channel: String,
    /// 通道式入口的外部对话标识（交互式入口不使用）。
    conversation_id: Option<String>,
    /// 调用方预置上文（时间正序；不落库，仅进入本轮 prompt 历史段）。
    seed_history: Vec<ChatMessage>,
}

/// 会话解析产出（含本轮生效人格与新会话桥接内容）。
struct ResolvedSession {
    session: Session,
    /// 本轮生效人格（会话已绑定人格时覆盖请求人格）。
    persona: String,
    /// 新会话桥接内容（None = 未加载 / 不注入）。
    bridge_context: Option<String>,
}

/// 装配完成的生成请求（会话定位 + Prompt + 预算裁剪后的 `ChatRequest`）。
struct PreparedRequest {
    request_id: Uuid,
    session_id: Uuid,
    persona: String,
    /// 本轮用户消息（已去除首尾空白）。
    message: String,
    chat_request: ChatRequest,
}

// =========================================================
// 用例入口
// =========================================================

/// 执行非流式生成用例（通道入口）：以指定人格回复一条消息（记忆检索 + Prompt 装配 + LLM）。
///
/// 流程:
/// 1. 参数与策略校验（空消息 / 人格白名单）；
/// 2. 会话定位（显式 `session_id` > `channel` + `conversation_id`，含惰性封存体检）；
/// 3. 载入会话历史（配置驱动的分页窗口）；
/// 4. 共用召回装配记忆上下文（与在线管线同一份实现）；
/// 5. 脉络素材与行为 / 知识 / 示例素材（失败或关闭均静默降级）；
/// 6. 系统 Prompt 装配（普通 / 协调预算两条路径）+ Token 预算裁剪；
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
    let config = engine.config();
    let input = ChatInput {
        message: req.message,
        persona: req.persona,
        session_id: req.session_id,
        mode: ChatMode::Channel,
        channel: req.channel,
        conversation_id: req.conversation_id,
        seed_history: Vec::new(),
    };

    let prepared = prepare_request(engine, &input, config.as_ref()).await?;
    tracing::info!(
        request_id = %prepared.request_id,
        session_id = %prepared.session_id,
        persona = %prepared.persona,
        input_chars = prepared.message.chars().count(),
        "生成用例开始（记忆检索与 Prompt 装配完成）"
    );

    // ---- 非流式：LLM 生成（失败不落库，避免库内留下孤立用户消息） ----
    let reply = step_call_llm(engine, &prepared.chat_request, prepared.session_id).await?;

    // ---- 落库（用户消息 + 助手回复；均带人格归属，供桌面按来源展示） ----
    let storage = engine.storage_ref().as_ref();
    let reply_chars = step_persist_reply(
        storage,
        prepared.session_id,
        &prepared.persona,
        &prepared.message,
        &reply,
    )
    .await?;

    tracing::info!(
        session_id = %prepared.session_id,
        persona = %prepared.persona,
        reply_chars,
        "生成用例完成（消息已落库）"
    );

    Ok(ChatSendOutcome {
        reply,
        session_id: prepared.session_id,
        chars: reply_chars,
    })
}

/// 执行流式生成用例（交互入口）：装配完成后返回增量事件流句柄。
///
/// 流程:
/// - 前置编排与非流式完全同源（参数校验 / 门禁 / 会话 / 历史 / 召回 / Prompt / 预算）；
/// - 打开 LLM 增量流失败：不落库、不启动转发任务，返回只含一个 `Error` 事件的流；
/// - 打开成功：后台任务先落库用户消息，再逐增量转发 `Delta`，结束时落库助手回复
///   （仅当无错且非空）并发 `Done`（错误经 `Error` 事件表达，不再发 `Done`）。
///
/// 参数:
/// - `engine`: 服务层引擎（后台转发任务持有存储句柄）。
/// - `req`: 流式生成请求（消息 / 人格 / 会话 / 预置上文 / 配置覆盖）。
///
/// 返回:
/// - 成功时返回 `ChatStreamHandle`（会话定位 + 事件流）；前置编排失败返回对应错误。
pub(crate) async fn stream(
    engine: &Arc<Engine>,
    req: ChatStreamRequest,
) -> RamariaResult<ChatStreamHandle> {
    let config = req
        .config_override
        .clone()
        .unwrap_or_else(|| engine.config());
    let input = ChatInput {
        message: req.message,
        persona: req.persona,
        session_id: req.session_id,
        mode: ChatMode::Interactive,
        channel: String::new(),
        conversation_id: None,
        seed_history: req.seed_history,
    };

    let prepared = prepare_request(engine.as_ref(), &input, config.as_ref()).await?;
    let session_id = prepared.session_id;
    let request_id = prepared.request_id;
    tracing::info!(
        request_id = %request_id,
        session_id = %session_id,
        persona = %prepared.persona,
        input_chars = prepared.message.chars().count(),
        "流式生成开始（记忆检索与 Prompt 装配完成）"
    );

    // ---- 流式：打开 LLM 增量流（失败返回只含一个 Error 事件的事件流，不落库） ----
    let raw_stream =
        match step_open_stream(engine, &prepared.chat_request, request_id, session_id).await {
            Ok(raw_stream) => raw_stream,
            Err(handle) => return Ok(handle),
        };

    // ---- 后台任务：转发增量事件并落库（用户消息先落，助手回复按无错且非空落） ----
    let storage = Arc::clone(engine.storage_ref());
    let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();
    tokio::spawn(forward_and_persist(
        storage,
        raw_stream,
        tx,
        session_id,
        prepared.message,
        request_id,
        Some(prepared.persona),
    ));

    Ok(ChatStreamHandle {
        request_id,
        session_id,
        events: Box::pin(rx),
    })
}

// =========================================================
// 前置编排（两入口共用）
// =========================================================

/// 执行生成前置步骤（两入口单一实现），产出可直接调用的 `ChatRequest`。
///
/// 流程:
/// 1. 参数与策略校验（空消息 / 人格归一与白名单）；
/// 2. 交互式：应用状态门禁；
/// 3. 后端配置快照 + 交互式：线上 provider 隐私确认；
/// 4. 会话定位（交互式 / 通道式两种语义）；
/// 5. 历史窗口加载（配置驱动 + 预置上文前置）；
/// 6. 记忆召回（配置闸门 × 宿主策略）；
/// 7. 交互式：弱反馈检测；
/// 8. 脉络素材；
/// 9. 行为层情境路由；
/// 10. 知识层判定器检索；
/// 11. 示例预选；
/// 12. 系统 Prompt 装配（普通 / 协调预算路径）；
/// 13. Token 预算裁剪与 `ChatRequest` 构建。
async fn prepare_request(
    engine: &Engine,
    input: &ChatInput,
    config: &RamariaConfig,
) -> RamariaResult<PreparedRequest> {
    let storage = engine.storage_ref().as_ref();

    // ---- 1. 参数与策略校验 ----
    let (message, persona) = step_validate(engine, &input.message, input.persona.as_deref())?;

    // ---- 2. 应用状态门禁（交互式） ----
    if input.mode == ChatMode::Interactive {
        step_check_state(engine)?;
    }

    // ---- 3. 后端配置 + 隐私门禁（交互式） ----
    let backend = step_backend_config(engine);
    if input.mode == ChatMode::Interactive {
        step_check_privacy(engine, &backend).await?;
    }

    // ---- 4. 会话定位 ----
    let resolved = step_resolve_session(engine, storage, config, input, &persona).await?;
    let persona = resolved.persona.clone();
    let session_id = resolved.session.id;

    // ---- 5. 历史窗口（配置驱动；预置上文前置） ----
    let history = step_load_history(storage, config, session_id, &input.seed_history).await;

    // ---- 6. 记忆召回（配置闸门 × 宿主策略） ----
    let (memory_context, rag_covered_labels, utt_context) =
        step_recall(engine, storage, config, &persona, message).await?;

    // ---- 7. 弱反馈检测（交互式；失败只 warn） ----
    if input.mode == ChatMode::Interactive {
        step_detect_feedback(storage, config, session_id, &persona, message).await;
    }

    // ---- 8. 脉络素材（跨会话近期摘要 + 最后活跃时间） ----
    let narrative = step_load_narrative(engine, storage, config, &persona, message).await;

    // ---- 9. 行为层情境路由（关闭 / 未命中 / 失败 → 不注入行为块） ----
    let behavior_decision =
        step_route_behavior(engine, storage, config, &persona, session_id, &history).await;

    // ---- 10. 知识层判定器检索（关闭 / 未命中 / 失败 → 空，不注入知识块） ----
    let knowledge_facts = step_load_knowledge(storage, config, &persona, message).await;

    // ---- 11. 示例预选（关闭 → 空；记忆未命中时走兜底轮换） ----
    let examples =
        step_load_examples(storage, config, &persona, message, memory_context.is_some()).await;

    // ---- 12. 系统 Prompt 装配（协调路径可能裁剪 / 丢弃 RAG，以结果回写记忆上下文） ----
    let inputs = PromptMaterialInputs {
        persona_uid: Some(&persona),
        recent_summaries: &narrative.recent_summaries,
        last_active_at: narrative.last_active_at.as_deref(),
        utt_context: utt_context.as_deref(),
        bridge_context: resolved.bridge_context.as_deref(),
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
    };
    let (system_prompt, memory_context) =
        step_build_prompt(storage, config, &inputs, memory_context.clone()).await;

    // ---- 13. Token 预算与 ChatRequest ----
    let request_id = new_id();
    let budgeted = step_apply_token_budget(
        config,
        &backend,
        &system_prompt,
        memory_context.as_deref(),
        &history,
        message,
        request_id,
    );
    let chat_request = step_build_request(&backend, budgeted, message, request_id);

    Ok(PreparedRequest {
        request_id,
        session_id,
        persona,
        message: message.to_string(),
        chat_request,
    })
}

// =========================================================
// 各步骤实现
// =========================================================

/// 步骤 1：参数与策略校验。
///
/// 返回:
/// - 去除首尾空白的用户消息 + 归一化人格（空 / None → [`DEFAULT_PERSONA_UID`]）。
/// - 空消息返回 `Validation`；人格不在可见白名单内返回 `Privacy`。
fn step_validate<'a>(
    engine: &Engine,
    message: &'a str,
    persona: Option<&str>,
) -> RamariaResult<(&'a str, String)> {
    let message = message.trim();
    if message.is_empty() {
        return Err(RamariaError::validation("消息不能为空"));
    }

    let persona = normalize_persona(persona);
    let policy = engine.recall_policy();
    if !policy.persona_allowed(&persona) {
        tracing::warn!(persona = %persona, "生成请求的人格不在可见白名单内，拒绝");
        return Err(RamariaError::privacy(format!(
            "人格 {persona} 不在可见白名单内（allowed_personas）"
        )));
    }

    Ok((message, persona))
}

/// 步骤 2（仅交互式）：应用状态门禁（`Ready` / `Degraded` 放行，其余拒绝）。
fn step_check_state(engine: &Engine) -> RamariaResult<()> {
    match engine.current_state() {
        AppState::Ready => {
            tracing::debug!(state = %AppState::Ready, "状态检查通过：Ready");
            Ok(())
        }
        AppState::Degraded => {
            tracing::warn!(
                state = %AppState::Degraded,
                "应用处于降级状态，对话功能可用但向量检索已降级"
            );
            Ok(())
        }
        AppState::FatalError => {
            tracing::error!(state = %AppState::FatalError, "状态检查失败：应用处于 FatalError 状态");
            Err(RamariaError::validation(
                "应用发生严重错误，请查看日志后重启应用。",
            ))
        }
        other => {
            tracing::warn!(state = %other, "状态检查失败：应用尚未就绪");
            Err(RamariaError::validation(format!(
                "应用尚未就绪（当前状态: {other}）。请先完成设置流程。"
            )))
        }
    }
}

/// 步骤 3a：读取当前 LLM 后端配置快照（Token 预算与请求参数来源）。
fn step_backend_config(engine: &Engine) -> BackendConfig {
    engine.llm_ref().config().clone()
}

/// 步骤 3b（仅交互式）：线上 provider 的隐私确认门禁。
async fn step_check_privacy(engine: &Engine, backend: &BackendConfig) -> RamariaResult<()> {
    if backend.provider.is_online() {
        crate::privacy::require_privacy(
            engine.storage_ref().as_ref(),
            backend.provider,
            &backend.base_url,
        )
        .await?;
    }
    Ok(())
}

/// 步骤 4：会话定位（交互式 / 通道式语义参数化）。
///
/// 交互式（桌面 / CLI）:
/// - 显式 `session_id`：校验存在性与未关闭；存量 NULL 归属按调用方显式人格回写
///   （失败只 warn 不阻塞）；会话已绑定人格时覆盖本轮生效人格。
/// - 无 `session_id`：新建会话；注入闸门开启时加载新会话桥接内容。
///
/// 通道式（MCP）:
/// - 显式 `session_id`：校验存在性、未关闭与人格归属一致性；
/// - 无 `session_id`：复用回流写入的会话解析（含惰性封存体检与"标识被他人格占用则另起"）。
///
/// 返回:
/// - 目标会话 + 本轮生效人格 + 新会话桥接内容。
async fn step_resolve_session(
    engine: &Engine,
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    input: &ChatInput,
    persona: &str,
) -> RamariaResult<ResolvedSession> {
    match input.mode {
        ChatMode::Interactive => match input.session_id {
            Some(sid) => {
                let mut session = storage
                    .get_session(sid)
                    .await?
                    .ok_or_else(|| RamariaError::validation(format!("会话不存在: {sid}")))?;
                if session.ended_at.is_some() {
                    return Err(RamariaError::validation(format!(
                        "会话已关闭（session {sid}），请开启新对话。"
                    )));
                }

                // 存量 NULL 会话归属回写（仅当调用方显式提供人格；失败只 warn 不阻塞）
                if session.persona_uid.is_none() && persona_explicit(input.persona.as_deref()) {
                    match storage.bind_session_persona_uid(session.id, persona).await {
                        Ok(()) => {
                            session.persona_uid = Some(persona.to_string());
                            tracing::info!(
                                session_id = %session.id,
                                persona = %persona,
                                "会话 NULL 归属已回写绑定"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                session_id = %session.id,
                                error = %e,
                                "会话 NULL 归属回写失败（不阻塞本轮生成）"
                            );
                        }
                    }
                }

                // 会话已绑定人格 → 覆盖本轮生效人格（避免跨人格上下文串用）
                let effective = session
                    .persona_uid
                    .clone()
                    .unwrap_or_else(|| persona.to_string());
                Ok(ResolvedSession {
                    session,
                    persona: effective,
                    bridge_context: None,
                })
            }
            None => {
                let session = storage
                    .create_session(Some(persona))
                    .await
                    .map_err(|e| RamariaError::storage_with_source("创建 session 失败", e))?;
                tracing::info!(session_id = %session.id, "交互式生成新建会话");

                // 新会话桥接：最近一个已关闭会话的尾部原文（两级降级，失败跳过）
                let bridge_context = if config.injection.bridge {
                    let bridge = crate::bridge::load_bridge_context(
                        storage,
                        &config.bridge,
                        &config.utt,
                        Some(persona),
                    )
                    .await;
                    bridge.content
                } else {
                    tracing::debug!("桥接注入闸门关闭，跳过桥接加载");
                    None
                };

                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context,
                })
            }
        },
        ChatMode::Channel => {
            if let Some(id) = input.session_id {
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
                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context: None,
                })
            } else {
                let session = crate::ingest::resolve_session(
                    engine,
                    &input.channel,
                    input.conversation_id.as_deref(),
                    persona,
                )
                .await?;
                Ok(ResolvedSession {
                    session,
                    persona: persona.to_string(),
                    bridge_context: None,
                })
            }
        }
    }
}

/// 步骤 5：载入会话历史（时间正序），配置驱动的分页窗口。
///
/// 语义:
/// - 分页 20 条倒序加载（先检查条数上限与字符预算，再取页）；
/// - 加载条数上限取 `[session].max_history_messages`，字符预算取 `[session].max_history_chars`，
///   每条按 `content.chars().count() + 16`（role 标记开销）计；
/// - 读取失败记 warn 并保留已加载部分（首屏失败即空历史），不阻塞生成；
/// - 按 `created_at` 升序排列后，前置拼接调用方预置上文（seed 早于本会话历史）。
async fn step_load_history(
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    session_id: Uuid,
    seed_history: &[ChatMessage],
) -> Vec<ChatMessage> {
    let max_messages = config.session.max_history_messages.max(1) as i64;
    let char_budget = config.session.max_history_chars.max(1) as usize;

    let mut loaded: Vec<Message> = Vec::new();
    let mut total_chars: usize = 0;
    let mut offset: i64 = 0;

    loop {
        if loaded.len() as i64 >= max_messages {
            tracing::debug!(
                session_id = %session_id,
                loaded = loaded.len(),
                max = max_messages,
                "历史消息已达到加载条数上限"
            );
            break;
        }
        if total_chars >= char_budget {
            tracing::debug!(
                session_id = %session_id,
                total_chars,
                budget = char_budget,
                "历史消息已达到字符预算"
            );
            break;
        }

        let page = match storage
            .list_messages_paginated(session_id, HISTORY_PAGE_SIZE, offset)
            .await
        {
            Ok(page) => page,
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    loaded = loaded.len(),
                    error = %e,
                    "分页加载会话历史失败，按已加载部分继续"
                );
                break;
            }
        };
        if page.is_empty() {
            break; // 没有更多消息
        }

        let page_len = page.len() as i64;
        for message in &page {
            total_chars += message.content.chars().count() + 16;
        }
        loaded.extend(page);
        offset += HISTORY_PAGE_SIZE;

        // 如果返回的页不满，说明已是最后一批
        if page_len < HISTORY_PAGE_SIZE {
            break;
        }
    }

    // 按 created_at 升序排列（分页返回为倒序；相同时刻保持稳定不翻转）
    loaded.sort_by_key(|message| message.created_at);

    let mut merged = seed_history.to_vec();
    merged.extend(loaded.into_iter().map(|message| ChatMessage {
        role: message.role,
        content: message.content,
    }));
    merged
}

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
async fn step_recall(
    engine: &Engine,
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    persona: &str,
    message: &str,
) -> RamariaResult<(Option<String>, Vec<String>, Option<String>)> {
    engine.ensure_index_loaded().await?;

    // 嵌入 provider 取快照后在锁外使用（缺失 → 共用召回内部降级为 BM25 + 关键词镜像）
    let embedding = engine.embedding_ref();
    let policy = engine.recall_policy();
    let recall = assemble_recall(RecallInput {
        retriever: &**engine.retriever_slot(),
        keyword_mirror: &**engine.keyword_mirror_ref(),
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

/// 步骤 7（仅交互式）：弱反馈检测（S2 纠正 / S3 继续）。
///
/// 说明:
/// - 检测序列 = 会话已落库消息 + 追加的本轮用户消息（时间取当前），检测后不落库；
/// - `[feedback].enabled=false` 或读取失败 / 写入失败时静默降级（记 warn，不阻塞生成）。
async fn step_detect_feedback(
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    session_id: Uuid,
    persona: &str,
    message: &str,
) {
    if !config.feedback.enabled {
        return;
    }

    let mut recent = match storage.list_messages(session_id).await {
        Ok(messages) => messages,
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "读取会话消息用于弱反馈检测失败，跳过");
            Vec::new()
        }
    };
    // 追加当前用户消息作为检测输入的最后一条（时间戳取当前，供间隔判定）
    recent.push(Message::new(
        session_id,
        MessageRole::User,
        message.to_string(),
        MessageSource::Local,
    ));

    if let Err(e) = crate::feedback::process_feedback_for_new_message(
        storage,
        &config.feedback,
        session_id,
        Some(persona),
        &recent,
    )
    .await
    {
        tracing::warn!(session_id = %session_id, error = %e, "弱反馈信号处理失败（不阻塞对话主流程）");
    }
}

/// 步骤 8：脉络素材（跨会话近期摘要 + 最后活跃时间）。
///
/// 说明:
/// - 注入闸门 / 加权检索 / "最近 N 条"回退语义由记忆层单份实现承担；
/// - 任何降级路径返回空素材而非错误（不阻塞生成）。
async fn step_load_narrative(
    engine: &Engine,
    storage: &dyn StorageBackend,
    config: &RamariaConfig,
    persona: &str,
    message: &str,
) -> ramaria_memory::chat::NarrativeMaterial {
    load_narrative_material(
        storage,
        &**engine.retriever_slot(),
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
async fn step_route_behavior(
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
async fn step_load_knowledge(
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
async fn step_load_examples(
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

/// 步骤 12：系统 Prompt 装配（普通路径 / 协调预算路径）。
///
/// 返回:
/// - `(system_prompt, memory_context)`；协调路径可能裁剪 / 丢弃 RAG，返回值即最终记忆上下文
///   （普通路径原样返回传入值）。
async fn step_build_prompt(
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
fn step_apply_token_budget(
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
fn step_build_request(
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

// =========================================================
// 生成侧（非流式 / 流式）
// =========================================================

/// 非流式生成：调用 LLM 并返回回复全文；失败记 warn 并上抛（不落库）。
async fn step_call_llm(
    engine: &Engine,
    request: &ChatRequest,
    session_id: Uuid,
) -> RamariaResult<String> {
    // LLM provider 取快照后在锁外调用（热更新期间取到旧快照或新快照均自洽）
    let llm = engine.llm_ref();
    match llm.chat(request).await {
        Ok(reply) => Ok(reply),
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "LLM 生成失败，本次不写入任何消息"
            );
            Err(e)
        }
    }
}

/// 非流式落库：写入用户消息与助手回复（均带人格归属，来源为本地）。
async fn step_persist_reply(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    persona: &str,
    message: &str,
    reply: &str,
) -> RamariaResult<usize> {
    let user_message = Message::new(
        session_id,
        MessageRole::User,
        message.to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(persona.to_string()));
    storage.save_message(&user_message).await?;

    let reply_chars = reply.chars().count();
    let assistant_message = Message::new(
        session_id,
        MessageRole::Assistant,
        reply.to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(persona.to_string()));
    storage.save_message(&assistant_message).await?;

    Ok(reply_chars)
}

/// LLM 原始增量流类型（provider 层协议）。
type RawDeltaStream = Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>;

/// 步骤 15a（流式）：打开 LLM 增量流。
///
/// 返回:
/// - `Ok(stream)`: 增量流已打开，交由转发任务消费；
/// - `Err(handle)`: 流未打开——返回只含一个 `Error` 事件的事件流句柄（不落库）。
async fn step_open_stream(
    engine: &Engine,
    request: &ChatRequest,
    request_id: Uuid,
    session_id: Uuid,
) -> std::result::Result<RawDeltaStream, ChatStreamHandle> {
    let llm = engine.llm_ref();
    match llm.chat_stream(request).await {
        Ok(stream) => Ok(stream),
        Err(e) => {
            tracing::error!(
                error = %e,
                request_id = %request_id,
                session_id = %session_id,
                "LLM chat_stream 调用失败，构造 Error 事件流"
            );
            let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();
            let _ = tx.unbounded_send(Ok(StreamEvent::error(request_id, e.to_string())));
            Err(ChatStreamHandle {
                request_id,
                session_id,
                events: Box::pin(rx),
            })
        }
    }
}

/// 流式转发任务：消费原始增量流，转发事件并落库消息。
///
/// 语义（与非流式路径的失败纪律对齐）:
/// 1. 先落库用户消息（来源本地）；落库失败 → 事件流发送错误项并终止；
/// 2. 逐增量转发 `Delta`；`done=true` 的增量记录后端标识并结束读取；
/// 3. 流中错误 → 发送 `Error` 事件并标记错误（不再发 `Done`）；
/// 4. 结束时仅当无错且回复非空才落库助手消息（来源线上）；
/// 5. 仅在无错时发送 `Done`（含会话、后端标识与总字符数）。
async fn forward_and_persist(
    storage: Arc<dyn StorageBackend>,
    raw_stream: RawDeltaStream,
    tx: mpsc::UnboundedSender<RamariaResult<StreamEvent>>,
    session_id: Uuid,
    user_message: String,
    request_id: Uuid,
    persona_uid: Option<String>,
) {
    futures::pin_mut!(raw_stream);

    let mut full_reply = String::new();
    let mut backend_id: Option<String> = None;
    let mut has_error = false;
    let started_at = now_ms();

    // 1. 保存用户消息（携带人格归属，表示"在此 persona 的对话上下文中"）
    let user_msg = Message::new(
        session_id,
        MessageRole::User,
        user_message,
        MessageSource::Local,
    )
    .with_persona_uid(persona_uid.clone());
    if let Err(e) = storage.save_message(&user_msg).await {
        tracing::error!(session_id = %session_id, error = %e, "保存用户消息失败");
        let _ = tx.unbounded_send(Err(e));
        return;
    }

    // 2. 消费 LLM 流
    while let Some(delta_result) = raw_stream.next().await {
        match delta_result {
            Ok(delta) => {
                full_reply.push_str(&delta.content);

                // 转发 Delta 事件
                let event = StreamEvent::delta(request_id, delta.content);
                if tx.unbounded_send(Ok(event)).is_err() {
                    return; // 接收端已断开
                }

                if delta.done {
                    backend_id = delta.metadata;
                    break;
                }
            }
            Err(e) => {
                has_error = true;
                tracing::error!(session_id = %session_id, error = %e, "LLM 流错误");
                let event = StreamEvent::error(request_id, e.to_string());
                let _ = tx.unbounded_send(Ok(event));
                break;
            }
        }
    }

    // 3. 保存助手消息（仅在非错误且非空时；来源线上，携带人格归属）
    if !has_error && !full_reply.is_empty() {
        let assistant_msg = Message::new(
            session_id,
            MessageRole::Assistant,
            full_reply.clone(),
            MessageSource::Online,
        )
        .with_persona_uid(persona_uid.clone());
        if let Err(e) = storage.save_message(&assistant_msg).await {
            tracing::error!(session_id = %session_id, error = %e, "保存 assistant 消息失败");
        }
    }

    // 4. 发送 Done 事件（仅在无错误时——错误已通过 Error 事件发送，无需再发 Done）
    if !has_error {
        let done_event = StreamEvent::done(
            request_id,
            Some(session_id),
            backend_id,
            full_reply.chars().count(),
        );
        let _ = tx.unbounded_send(Ok(done_event));
    }

    tracing::info!(
        request_id = %request_id,
        session_id = %session_id,
        reply_chars = full_reply.chars().count(),
        has_error,
        duration_ms = now_ms() - started_at,
        "流式生成完成"
    );
}

/// 归一化人格 uid（缺省取默认人格）。
fn normalize_persona(persona: Option<&str>) -> String {
    persona
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_PERSONA_UID)
        .to_string()
}

/// 调用方是否显式提供了人格（控制存量 NULL 会话的归属回写）。
fn persona_explicit(persona: Option<&str>) -> bool {
    persona.is_some_and(|value| !value.trim().is_empty())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recall::RecallPolicy;
    use crate::stream_event::ChatEventStream;
    use crate::test_support::{
        MockLlm, engine_with_db, engine_with_failing_llm, engine_with_l1_reply,
        engine_with_shared_llm, seed_closed_session_with_messages, seed_persona,
        seed_session_with_messages,
    };
    use crate::types::CHANNEL_MCP;
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use ramaria_core::types::PrivacyConsent;

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

    /// 构造流式生成请求（默认人格、无预置上文）。
    fn stream_request(message: &str, session_id: Option<Uuid>) -> ChatStreamRequest {
        ChatStreamRequest {
            message: message.to_string(),
            persona: Some(DEFAULT_PERSONA_UID.to_string()),
            session_id,
            seed_history: Vec::new(),
            config_override: None,
        }
    }

    /// 收集事件流全部事件（出现流内错误项即失败，用于"成功路径"断言）。
    async fn collect_events(events: ChatEventStream) -> Vec<StreamEvent> {
        let mut stream = events;
        let mut collected = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => collected.push(event),
                Err(e) => panic!("事件流不应返回错误项: {e}"),
            }
        }
        collected
    }

    /// 提取事件类型序列。
    fn kinds(events: &[StreamEvent]) -> Vec<&'static str> {
        events.iter().map(StreamEvent::kind).collect()
    }

    // =========================================================
    // 非流式（通道入口）——既有断言保持
    // =========================================================

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

    // =========================================================
    // 流式（交互入口）
    // =========================================================

    /// 流式成功：事件序列 Delta… → Done；消息落库两条（user 本地 / assistant 线上，均带人格）。
    #[tokio::test]
    async fn stream_success_emits_deltas_done_and_persists_messages() {
        // 多片段流式（拼接后等于回复全文）
        let llm = Arc::new(MockLlm::with_stream_chunks(&["嗯，", "我在", "听。"]));
        let (engine, storage, dir) =
            engine_with_shared_llm("chat-stream-ok", llm, RamariaConfig::default(), None).await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        engine.set_state(AppState::Ready);

        let handle = engine
            .chat_stream(stream_request("你好", None))
            .await
            .expect("流式生成应成功");
        let session_id = handle.session_id;
        let events = collect_events(handle.events).await;

        // 事件序列：Delta… → Done（无 Error）
        assert_eq!(
            kinds(&events),
            vec!["delta", "delta", "delta", "done"],
            "三个增量片段后跟一个完成事件"
        );
        assert_eq!(events.last().map(StreamEvent::kind), Some("done"));
        let delta_text: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::Delta { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(delta_text, REPLY, "增量文本拼接应等于回复全文");

        let done = events.iter().find_map(|event| match event {
            StreamEvent::Done {
                session_id,
                backend_id,
                total_chars,
                ..
            } => Some((*session_id, backend_id.clone(), *total_chars)),
            _ => None,
        });
        let (done_session, backend_id, total_chars) = done.expect("应包含 Done 事件");
        assert_eq!(done_session, Some(session_id));
        assert_eq!(backend_id.as_deref(), Some("stop"));
        assert_eq!(total_chars, REPLY.chars().count());

        // 落库：用户消息（本地来源）+ 助手回复（线上来源），均带人格
        let messages = storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].content, "你好");
        assert_eq!(messages[0].source, MessageSource::Local);
        assert_eq!(
            messages[0].persona_uid.as_deref(),
            Some(DEFAULT_PERSONA_UID)
        );
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert_eq!(messages[1].content, REPLY);
        assert_eq!(messages[1].source, MessageSource::Online);
        assert_eq!(
            messages[1].persona_uid.as_deref(),
            Some(DEFAULT_PERSONA_UID)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流打不开：返回 Ok，首个事件为 Error、无 Done；库内无消息。
    #[tokio::test]
    async fn stream_open_failure_returns_error_event_without_persistence() {
        let (engine, storage, dir) = engine_with_failing_llm("chat-stream-open-fail").await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        engine.set_state(AppState::Ready);

        let handle = engine
            .chat_stream(stream_request("你好", None))
            .await
            .expect("流打不开也应返回事件流句柄");
        let session_id = handle.session_id;
        let request_id = handle.request_id;
        let events = collect_events(handle.events).await;

        assert_eq!(
            kinds(&events),
            vec!["error"],
            "流打不开时应只有一个 Error 事件"
        );
        assert_eq!(events[0].request_id(), request_id);

        let messages = storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功");
        assert!(messages.is_empty(), "流打不开不应写入任何消息");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流中错误：先收到增量、再收到 Error、无 Done；用户消息已落库、助手回复未落库。
    #[tokio::test]
    async fn stream_error_after_deltas_keeps_user_message_only() {
        let llm = Arc::new(MockLlm::stream_fails_after(&["好的", "，继续"]));
        let (engine, storage, dir) =
            engine_with_shared_llm("chat-stream-mid-fail", llm, RamariaConfig::default(), None)
                .await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        engine.set_state(AppState::Ready);

        let handle = engine
            .chat_stream(stream_request("你好", None))
            .await
            .expect("应返回事件流句柄");
        let session_id = handle.session_id;
        let events = collect_events(handle.events).await;

        let event_kinds = kinds(&events);
        assert!(event_kinds.len() >= 2, "应先收到增量再收到错误");
        assert_eq!(
            event_kinds[event_kinds.len() - 1],
            "error",
            "流中错误应转为 Error 事件"
        );
        assert!(!event_kinds.contains(&"done"), "出错后不应再发 Done");
        assert!(
            event_kinds[..event_kinds.len() - 1]
                .iter()
                .all(|kind| *kind == "delta"),
            "错误之前的序列应为增量事件"
        );

        let messages = storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功");
        assert_eq!(messages.len(), 1, "用户消息已落库、助手回复不落库");
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].content, "你好");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状态门禁：交互入口在非就绪状态被拒、就绪后放行；通道入口不受门禁（初值 NeedsSetup）。
    #[tokio::test]
    async fn interactive_state_gate_blocks_until_ready_but_channel_passes() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-state-gate", REPLY).await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;

        // 通道入口（MCP）：状态机初值 NeedsSetup 下仍可生成
        assert_eq!(engine.current_state(), AppState::NeedsSetup);
        let outcome = engine
            .chat_send(request("通道你好", Some("client-state")))
            .await
            .expect("通道入口不应受状态门禁约束");
        assert_eq!(outcome.reply, REPLY);

        // 交互入口：非就绪状态拒绝
        engine.set_state(AppState::Indexing);
        let err = engine
            .chat_stream(stream_request("你好", None))
            .await
            .expect_err("未就绪状态应拒绝流式生成");
        assert_eq!(err.category(), "validation");
        assert!(
            err.context().contains("尚未就绪"),
            "错误应含未就绪提示: {err}"
        );

        // 交互入口：就绪状态放行
        engine.set_state(AppState::Ready);
        let handle = engine
            .chat_stream(stream_request("你好", None))
            .await
            .expect("就绪状态应放行");
        let events = collect_events(handle.events).await;
        assert_eq!(events.last().map(StreamEvent::kind), Some("done"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 隐私门禁：线上 provider 未确认时拒绝（Privacy）；写入确认后放行。
    #[tokio::test]
    async fn interactive_privacy_gate_requires_consent_for_online_provider() {
        let (engine, storage, dir) = engine_with_shared_llm(
            "chat-privacy",
            Arc::new(MockLlm::online()),
            RamariaConfig::default(),
            None,
        )
        .await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        engine.set_state(AppState::Ready);

        let err = engine
            .chat_stream(stream_request("你好", None))
            .await
            .expect_err("线上 provider 未确认隐私应被拒绝");
        assert_eq!(err.category(), "privacy");

        // 写入确认后放行
        let backend = BackendConfig::deepseek_default();
        storage
            .save_privacy_consent(&PrivacyConsent::new(
                backend.provider,
                backend.base_url.clone(),
                true,
            ))
            .await
            .expect("写入隐私确认应成功");

        let handle = engine
            .chat_stream(stream_request("你好", None))
            .await
            .expect("确认后应放行");
        let events = collect_events(handle.events).await;
        assert_eq!(events.last().map(StreamEvent::kind), Some("done"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // =========================================================
    // 历史窗口（配置驱动）
    // =========================================================

    /// 历史窗口：默认条数上限 200——250 条消息只加载最近 200 条进入 Prompt。
    #[tokio::test]
    async fn history_window_caps_at_default_messages_limit() {
        let llm = Arc::new(MockLlm::with_reply(REPLY));
        let (engine, storage, dir) = engine_with_shared_llm(
            "chat-history-cap",
            Arc::clone(&llm),
            RamariaConfig::default(),
            None,
        )
        .await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        let session_id =
            seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 250, now_ms() - 250_000)
                .await;

        let mut req = request("你好", None);
        req.session_id = Some(session_id);
        let outcome = engine.chat_send(req).await.expect("生成应成功");
        assert_eq!(outcome.reply, REPLY);

        let recorded = llm.requests();
        let last_request = recorded.last().expect("应记录一次 LLM 请求");
        assert_eq!(
            last_request.history.len(),
            200,
            "进入 Prompt 的历史应为默认上限（200）"
        );
        assert_eq!(
            last_request.history[0].content, "消息内容 50",
            "窗口应从第 50 条开始（最近 200 条）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 历史窗口：字符预算越界即停（40 条 × 200 字符在第 2 页后越界 → 加载 40 条）。
    #[tokio::test]
    async fn history_window_stops_at_char_budget() {
        let (engine, storage, dir) = engine_with_db("chat-history-budget").await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        let session = storage
            .create_session(Some(DEFAULT_PERSONA_UID))
            .await
            .expect("创建会话应成功");

        // 40 条 × 200 字符：每条约 216 字符（含 role 标记开销），第 2 页后累计 8640 > 预算 6000
        let base_ts = now_ms() - 100_000;
        for i in 0..40 {
            let mut message = Message::new(
                session.id,
                MessageRole::User,
                "字".repeat(200),
                MessageSource::Local,
            )
            .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
            message.created_at = base_ts + i as i64;
            storage
                .save_message(&message)
                .await
                .expect("写入消息应成功");
        }

        let config = engine.config();
        let history = step_load_history(storage.as_ref(), config.as_ref(), session.id, &[]).await;
        assert_eq!(history.len(), 40, "字符预算在第 2 页后越界即停");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 历史窗口：条数上限按配置覆盖（设 40 时仅加载 40 条）。
    #[tokio::test]
    async fn history_window_follows_config_override() {
        let llm = Arc::new(MockLlm::with_reply(REPLY));
        let mut config = RamariaConfig::default();
        config.session.max_history_messages = 40;
        let (engine, storage, dir) =
            engine_with_shared_llm("chat-history-config", Arc::clone(&llm), config, None).await;
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        let session_id =
            seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 120, now_ms() - 120_000)
                .await;

        let mut req = request("你好", None);
        req.session_id = Some(session_id);
        engine.chat_send(req).await.expect("生成应成功");

        let recorded = llm.requests();
        let last_request = recorded.last().expect("应记录一次 LLM 请求");
        assert_eq!(last_request.history.len(), 40, "条数上限应按配置截断");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // =========================================================
    // 交互入口语义（桥接 / 弱反馈 / 预置上文 / 人格覆盖）
    // =========================================================

    /// 新会话桥接：system_prompt 含上一已关闭会话的尾部文本；注入闸门关闭时不加载。
    #[tokio::test]
    async fn bridge_context_injected_into_new_interactive_session() {
        // 正例：开桥接
        let llm = Arc::new(MockLlm::with_reply(REPLY));
        let (engine, storage, dir) = engine_with_shared_llm(
            "chat-bridge-on",
            Arc::clone(&llm),
            RamariaConfig::default(),
            None,
        )
        .await;
        let engine = Arc::new(engine);
        seed_persona(&storage, "char-0001").await;
        seed_closed_session_with_messages(&storage, "char-0001", 10, now_ms() - 60_000).await;
        engine.set_state(AppState::Ready);

        let mut req = stream_request("你好", None);
        req.persona = Some("char-0001".to_string());
        let handle = engine.chat_stream(req).await.expect("流式生成应成功");
        let events = collect_events(handle.events).await;
        assert_eq!(events.last().map(StreamEvent::kind), Some("done"));

        let recorded = llm.requests();
        let last_request = recorded.last().expect("应记录一次 LLM 请求");
        assert!(
            last_request.system_prompt.contains("消息内容 9"),
            "新会话应注入上一会话尾部文本"
        );

        let _ = std::fs::remove_dir_all(&dir);

        // 反例：关桥接
        let llm_off = Arc::new(MockLlm::with_reply(REPLY));
        let mut config = RamariaConfig::default();
        config.injection.bridge = false;
        let (engine_off, storage_off, dir_off) =
            engine_with_shared_llm("chat-bridge-off", Arc::clone(&llm_off), config, None).await;
        let engine_off = Arc::new(engine_off);
        seed_persona(&storage_off, "char-0001").await;
        seed_closed_session_with_messages(&storage_off, "char-0001", 10, now_ms() - 60_000).await;
        engine_off.set_state(AppState::Ready);

        let mut req_off = stream_request("你好", None);
        req_off.persona = Some("char-0001".to_string());
        let handle_off = engine_off
            .chat_stream(req_off)
            .await
            .expect("流式生成应成功");
        let _ = collect_events(handle_off.events).await;

        let recorded_off = llm_off.requests();
        let last_request_off = recorded_off.last().expect("应记录一次 LLM 请求");
        assert!(
            !last_request_off.system_prompt.contains("消息内容 9"),
            "桥接闸门关闭不应注入上一会话文本"
        );

        let _ = std::fs::remove_dir_all(&dir_off);
    }

    /// 弱反馈：窗口内的纠正前缀写入 feedback_log；`[feedback].enabled=false` 不产生记录。
    #[tokio::test]
    async fn weak_feedback_detected_unless_disabled() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-feedback", REPLY).await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        let session = storage
            .create_session(Some(DEFAULT_PERSONA_UID))
            .await
            .expect("创建会话应成功");
        // 会话已有上一条助手回复（窗口内），当前消息为纠正前缀
        let mut assistant = Message::new(
            session.id,
            MessageRole::Assistant,
            "我认为应该是甲方案".to_string(),
            MessageSource::Local,
        )
        .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
        assistant.created_at = now_ms() - 10_000;
        storage
            .save_message(&assistant)
            .await
            .expect("写入助手消息应成功");
        engine.set_state(AppState::Ready);

        let handle = engine
            .chat_stream(stream_request("不对，你说错了", Some(session.id)))
            .await
            .expect("流式生成应成功");
        let _ = collect_events(handle.events).await;

        let logs = storage
            .list_feedback_logs_by_persona(DEFAULT_PERSONA_UID)
            .await
            .expect("读取反馈日志应成功");
        assert_eq!(logs.len(), 1, "应写入一条纠正反馈");
        assert_eq!(
            logs[0].signal_type,
            ramaria_core::behavior::SignalType::Correction
        );

        // 关闭开关后不再产生记录（配置覆盖路径）
        let mut config = RamariaConfig::default();
        config.feedback.enabled = false;
        let mut req = stream_request("不对，你又错了", None);
        req.config_override = Some(Arc::new(config));
        let handle_off = engine.chat_stream(req).await.expect("流式生成应成功");
        let _ = collect_events(handle_off.events).await;

        let logs_after = storage
            .list_feedback_logs_by_persona(DEFAULT_PERSONA_UID)
            .await
            .expect("读取反馈日志应成功");
        assert_eq!(logs_after.len(), 1, "关闭弱反馈后不应新增记录");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预置上文：非空 seed 前置进入本轮 Prompt 历史段（顺序保持），且不落库。
    #[tokio::test]
    async fn seed_history_prepended_into_prompt() {
        let llm = Arc::new(MockLlm::with_reply(REPLY));
        let (engine, storage, dir) = engine_with_shared_llm(
            "chat-seed",
            Arc::clone(&llm),
            RamariaConfig::default(),
            None,
        )
        .await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        engine.set_state(AppState::Ready);

        let mut req = stream_request("继续聊", None);
        req.seed_history = vec![
            ChatMessage {
                role: MessageRole::User,
                content: "昨晚在写 Rust".to_string(),
            },
            ChatMessage {
                role: MessageRole::Assistant,
                content: "写到哪一步了".to_string(),
            },
        ];
        let handle = engine.chat_stream(req).await.expect("流式生成应成功");
        let session_id = handle.session_id;
        let _ = collect_events(handle.events).await;

        let recorded = llm.requests();
        let last_request = recorded.last().expect("应记录一次 LLM 请求");
        assert_eq!(last_request.history.len(), 2, "新会话无库内历史");
        assert_eq!(last_request.history[0].content, "昨晚在写 Rust");
        assert_eq!(last_request.history[1].content, "写到哪一步了");

        // 预置上文不落库：会话内仅有本轮两条消息
        let messages = storage
            .list_messages(session_id)
            .await
            .expect("读取消息成功");
        assert_eq!(messages.len(), 2, "预置上文不应落库");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 交互式显式会话：会话已绑定人格覆盖请求人格（不报错，按会话归属生成与落库）。
    #[tokio::test]
    async fn interactive_session_persona_overrides_request_persona() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-persona-override", REPLY).await;
        let engine = Arc::new(engine);
        seed_persona(&storage, DEFAULT_PERSONA_UID).await;
        seed_persona(&storage, "char-0001").await;
        engine.set_state(AppState::Ready);
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("建会话成功");

        let handle = engine
            .chat_stream(stream_request("你好", Some(session.id)))
            .await
            .expect("会话归属覆盖应放行");
        assert_eq!(handle.session_id, session.id);
        let _ = collect_events(handle.events).await;

        let messages = storage
            .list_messages(session.id)
            .await
            .expect("读取消息成功");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(messages[1].persona_uid.as_deref(), Some("char-0001"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 交互式显式会话：存量 NULL 归属会话按调用方显式人格回写绑定。
    #[tokio::test]
    async fn interactive_null_persona_session_is_bound() {
        let (engine, storage, dir) = engine_with_l1_reply("chat-null-bind", REPLY).await;
        let engine = Arc::new(engine);
        seed_persona(&storage, "char-0001").await;
        engine.set_state(AppState::Ready);
        let session = storage.create_session(None).await.expect("建会话成功");

        let mut req = stream_request("你好", Some(session.id));
        req.persona = Some("char-0001".to_string());
        let handle = engine.chat_stream(req).await.expect("回写绑定应放行");
        let _ = collect_events(handle.events).await;

        let stored = storage
            .get_session(session.id)
            .await
            .expect("读取会话成功")
            .expect("会话应存在");
        assert_eq!(stored.persona_uid.as_deref(), Some("char-0001"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
