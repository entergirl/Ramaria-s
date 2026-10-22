//! crates/ramaria-service/src/chat/steps.rs - Ramaria 生成用例前置编排与步骤实现
//!
//! 设计特点:
//! - 请求形态：两入口共用的内部形态（`ChatInput` / `ChatMode`）与编排产出（`PreparedRequest`）
//! - 单一编排：`prepare_request` 把校验 / 门禁 / 会话定位 / 历史窗口 / 弱反馈检测串联为固定顺序，
//!   非流式 / 流式 / 主动生成入口共用同一份前置链路
//! - 门禁语义：状态门禁与线上 provider 隐私确认仅对交互式入口生效
//!   （通道式入口不受约束；主动模式由生成用例预检后静默处理）
//! - 会话定位：交互式按显式 `session_id` > 新建（含新会话桥接注入）；
//!   通道式按显式 `session_id` > 通道 + 外部标识续写；
//!   主动模式按显式 `session_id` > 新建（不注入桥接，话题由锚点承载）
//! - 社交会话只读：显式指定社交平台会话的客户端本地生成一律拒绝；
//!   通道式仅放行入站回复链路（`message_in_history=true`）
//! - 主动模式：消息位为选题锚点（可为空），请求的用户消息位留空，
//!   提示词装配注入主动开口段
//! - 历史窗口：分页倒序加载，条数上限与字符预算取 `[session]` 配置（预算按渲染后正文计），
//!   读取失败保留已加载部分；图片占位符按附件状态渲染（完成 → 描述、处理中 → 提示、
//!   其余保留裸占位符；附件查询失败降级原文）；社交会话按发言人前缀标注他人消息
//! - 降级纪律：会话 NULL 归属回写 / 弱反馈检测失败均只记 warn，不阻塞生成

mod feedback;
mod gates;
mod session;

pub(super) use session::step_resolve_session;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatMessage, ChatRequest};
use ramaria_core::types::{
    Message, MessageRole, MessageSource, Session, is_social_channel, new_id,
};
use ramaria_memory::chat::{PromptMaterialInputs, SegmentStyle};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::DEFAULT_PERSONA_UID;

use super::context::{
    step_apply_token_budget, step_build_prompt, step_build_request, step_load_examples,
    step_load_knowledge, step_load_narrative, step_recall, step_route_behavior,
};
use super::history::step_load_history;
use super::social::is_group_conversation;

use feedback::{build_proactive_prompt, step_detect_feedback};
use gates::{step_backend_config, step_check_privacy, step_check_state, step_validate};

// =========================================================
// 内部请求形态
// =========================================================

/// 生成入口模式（宿主差异参数化）。
///
/// 变体:
/// - `Interactive`: 交互式入口（桌面 / CLI）——状态门禁 / 隐私门禁 / 新会话桥接 / 弱反馈检测开启；
/// - `Channel`: 通道式入口（MCP）——以上四项全关，会话按通道 + 外部标识解析；
/// - `Proactive`: 主动生成——消息位为选题锚点（可为空）、无状态 / 隐私二次检查
///   （由主动生成用例预检后静默处理）、会话不注入桥接。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatMode {
    Interactive,
    Channel,
    /// 主动生成：无用户消息（消息位为选题锚点，可为空）、无状态/隐私二次检查
    /// （由主动生成用例预检后静默处理）、会话不注入桥接。
    Proactive,
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
    /// 消息是否已入历史（社交通道回复：入站消息已落库，请求用户消息位留空）。
    pub(super) message_in_history: bool,
    /// 回复分段格式（通道式入口按目标通道声明；其余入口沿用多气泡契约）。
    pub(super) segment_style: SegmentStyle,
    /// 主动生成输入（仅 `Proactive` 模式使用；其余模式为 None）。
    pub(super) proactive: Option<ProactiveInput>,
}

/// 主动生成输入（`Proactive` 模式专用）。
pub(super) struct ProactiveInput {
    /// 候选锚点摘要（同时作为检索查询；None = 无具体话题）。
    pub(super) anchor: Option<String>,
    /// 开口角度。
    pub(super) angle: Option<String>,
    /// 说话语气。
    pub(super) tone: Option<String>,
}

/// 会话解析产出（含本轮生效人格与新会话桥接内容）。
#[derive(Debug)]
pub(super) struct ResolvedSession {
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
    /// 用户消息是否已在前置编排中落库（交互式入口为 true）。
    pub(super) user_persisted: bool,
    pub(super) chat_request: ChatRequest,
}

// =========================================================
// 前置编排（两入口共用）
// =========================================================

/// 执行生成前置步骤（两入口单一实现），产出可直接调用的 `ChatRequest`。
///
/// 流程:
/// 1. 参数与策略校验（空消息 / 人格归一与白名单；主动模式消息位为锚点，不做空校验）；
/// 2. 交互式：应用状态门禁（主动模式由调用方预检）；
/// 3. 后端配置快照 + 交互式：线上 provider 隐私确认（主动模式由调用方预检）；
/// 4. 会话定位（交互式 / 通道式 / 主动模式三种语义）；
/// 5. 历史窗口加载（配置驱动 + 预置上文前置），交互式随后先落库用户消息（首条消息不阻塞）；
/// 6. 记忆召回（配置闸门 × 宿主策略）；
/// 7. 交互式：弱反馈检测；
/// 8. 脉络素材；
/// 9. 行为层情境路由；
/// 10. 知识层判定器检索；
/// 11. 示例预选；
/// 12. 系统 Prompt 装配（普通 / 协调预算路径）；
/// 13. Token 预算裁剪与 `ChatRequest` 构建（主动模式用户消息位留空；
///     思考适配按 `[backend]` 配置注入思考开关 / 角色沉浸 / 对话温度）。
pub(super) async fn prepare_request(
    engine: &Engine,
    input: &ChatInput,
    config: &RamariaConfig,
) -> RamariaResult<PreparedRequest> {
    let storage = engine.storage_ref().as_ref();

    // ---- 1. 参数与策略校验 ----
    let (message, persona) = if input.mode == ChatMode::Proactive {
        // 主动生成：消息位为锚点（可为空，不做空校验）；人格白名单由调用方
        // 以"静默跳过"语义预检，此处只做归一
        (
            input.message.trim(),
            normalize_persona(input.persona.as_deref()),
        )
    } else {
        step_validate(engine, &input.message, input.persona.as_deref())?
    };

    // ---- 2. 应用状态门禁（交互式；主动模式由调用方预检） ----
    if input.mode == ChatMode::Interactive {
        step_check_state(engine)?;
    }

    // ---- 3. 后端配置 + 隐私门禁（交互式；主动模式由调用方预检） ----
    let backend = step_backend_config(engine);
    if input.mode == ChatMode::Interactive {
        step_check_privacy(engine, &backend).await?;
    }

    // ---- 4. 会话定位 ----
    let resolved = step_resolve_session(engine, storage, config, input, &persona).await?;
    let persona = resolved.persona.clone();
    let session_id = resolved.session.id;

    // ---- 5. 历史窗口（配置驱动；预置上文前置；社交会话标注发言人） ----
    let social = is_social_channel(&resolved.session.channel);
    let group_conversation = is_group_conversation(&resolved.session);
    let history = step_load_history(storage, config, session_id, &input.seed_history, social).await;

    // ---- 5.5 交互式用户消息先落库（首条消息不阻塞；历史窗口已加载，不含本条） ----
    let user_persisted = if input.mode == ChatMode::Interactive && !input.message_in_history {
        let user_message = Message::new(
            session_id,
            MessageRole::User,
            message.to_string(),
            MessageSource::Local,
        )
        .with_persona_uid(Some(persona.clone()));
        storage.save_message(&user_message).await?;
        true
    } else {
        false
    };

    // ---- 6. 记忆召回（配置闸门 × 宿主策略） ----
    let (memory_context, rag_covered_labels, utt_context) =
        step_recall(engine, storage, config, &persona, message).await?;

    // ---- 7. 弱反馈检测（交互式；失败只 warn） ----
    if input.mode == ChatMode::Interactive {
        step_detect_feedback(
            storage,
            config,
            session_id,
            &persona,
            message,
            user_persisted,
        )
        .await;
    }

    // ---- 8. 脉络素材（跨会话近期摘要 + 最后活跃时间） ----
    let narrative = step_load_narrative(engine, storage, config, &persona, message).await;

    // ---- 9. 行为层情境路由（关闭 / 未命中 / 失败 → 不注入行为块） ----
    let behavior_decision =
        step_route_behavior(engine, storage, config, &persona, session_id, &history).await;

    // ---- 10. 知识层判定器检索（关闭 / 未命中 / 失败 → 空，不注入知识块） ----
    let knowledge_facts = step_load_knowledge(storage, config, &persona, message).await;

    // ---- 11. 示例预选（常驻；注入闸门关闭 → 空） ----
    let examples = step_load_examples(storage, config, &persona).await;

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
        segment_style: input.segment_style,
        group_conversation,
        recent_state: ramaria_memory::chat::RecentStateConfig {
            enabled: config.proactive.recent_state_enabled,
            window_days: config.proactive.recent_state_window_days,
        },
        proactive: if input.mode == ChatMode::Proactive {
            build_proactive_prompt(input)
        } else {
            None
        },
    };
    let (system_prompt, memory_context) =
        step_build_prompt(storage, config, &inputs, memory_context.clone()).await;

    // ---- 13. Token 预算与 ChatRequest ----
    let request_id = new_id();
    // 主动生成无用户消息；社交通道回复的入站消息已落库（由历史窗口承载）：
    // 两类均将请求的 user 消息位留空（provider 侧跳过空消息）
    let request_message = if input.mode == ChatMode::Proactive || input.message_in_history {
        ""
    } else {
        message
    };
    let budgeted = step_apply_token_budget(
        config,
        &backend,
        &system_prompt,
        memory_context.as_deref(),
        &history,
        request_message,
        request_id,
    );
    let chat_request = step_build_request(
        &backend,
        &config.backend,
        budgeted,
        request_message,
        request_id,
    );

    Ok(PreparedRequest {
        request_id,
        session_id,
        persona,
        message: message.to_string(),
        user_persisted,
        chat_request,
    })
}
pub(super) fn normalize_persona(persona: Option<&str>) -> String {
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
