//! crates/ramaria-service/src/chat/steps.rs - Ramaria 生成用例前置编排与步骤实现
//!
//! 设计特点:
//! - 请求形态：两入口共用的内部形态（`ChatInput` / `ChatMode`）与编排产出（`PreparedRequest`）
//! - 单一编排：`prepare_request` 把校验 / 门禁 / 会话定位 / 历史窗口 / 弱反馈检测串联为固定顺序，
//!   非流式与流式入口共用同一份前置链路
//! - 门禁语义：状态门禁与线上 provider 隐私确认仅对交互式入口生效（通道式入口不受约束）
//! - 会话定位：交互式按显式 `session_id` > 新建（含新会话桥接注入）；
//!   通道式按显式 `session_id` > 通道 + 外部标识续写
//! - 历史窗口：分页倒序加载，条数上限与字符预算取 `[session]` 配置，读取失败保留已加载部分
//! - 降级纪律：会话 NULL 归属回写 / 弱反馈检测失败均只记 warn，不阻塞生成

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatMessage, ChatRequest, StorageBackend};
use ramaria_core::types::{
    AppState, BackendConfig, Message, MessageRole, MessageSource, Session, new_id,
};
use ramaria_memory::chat::PromptMaterialInputs;
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::DEFAULT_PERSONA_UID;

use super::context::{
    step_apply_token_budget, step_build_prompt, step_build_request, step_load_examples,
    step_load_knowledge, step_load_narrative, step_recall, step_route_behavior,
};

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
pub(super) enum ChatMode {
    Interactive,
    Channel,
}

/// 统一请求（两个入口共用的内部形态）。
pub(super) struct ChatInput {
    pub(super) message: String,
    pub(super) persona: Option<String>,
    pub(super) session_id: Option<Uuid>,
    pub(super) mode: ChatMode,
    /// 通道式入口的会话来源通道（交互式入口为空串，不使用该字段）。
    pub(super) channel: String,
    /// 通道式入口的外部对话标识（交互式入口不使用）。
    pub(super) conversation_id: Option<String>,
    /// 调用方预置上文（时间正序；不落库，仅进入本轮 prompt 历史段）。
    pub(super) seed_history: Vec<ChatMessage>,
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
pub(super) struct PreparedRequest {
    pub(super) request_id: Uuid,
    pub(super) session_id: Uuid,
    pub(super) persona: String,
    /// 本轮用户消息（已去除首尾空白）。
    pub(super) message: String,
    pub(super) chat_request: ChatRequest,
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
pub(super) async fn prepare_request(
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
// 步骤实现（校验 / 门禁 / 会话定位 / 历史窗口 / 弱反馈）
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
pub(super) async fn step_load_history(
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
