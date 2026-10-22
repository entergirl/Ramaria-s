//! crates/ramaria-service/src/chat/generate/reply.rs - Ramaria 社交通道回复生成
//!
//! 设计特点:
//! - 回复只走非流式路径，成功后落库并回报字数
//! - 生成失败记 warn 并上抛，入站消息已由入站链路落库

use ramaria_core::error::RamariaResult;
use ramaria_memory::chat::SegmentStyle;

use crate::engine::Engine;
use crate::types::{ChannelReplyRequest, ChatSendOutcome};

use super::super::steps::{ChatInput, ChatMode, prepare_request};
use super::steps::{step_call_llm, step_persist_assistant_only};

// =========================================================
// 社交通道回复用例
// =========================================================

/// 执行社交通道回复生成（入站消息已落库；只写助手回复行）。
///
/// 流程:
/// - 前置编排与非流式入口同源（校验 / 会话定位（显式会话 + 归属一致校验）/
///   历史窗口（已含入站消息）/ 召回 / Prompt 装配 / Token 预算）；
/// - 请求的用户消息位留空（消息由历史窗口承载，避免重复注入）；
/// - 发送前检查：触发消息已撤回 → 回复作废（不落库不发送）；
/// - LLM 生成后只落助手回复行；失败不落库（入库用户消息由写入用例负责）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 回复请求（会话 / 人格 / 本轮用户消息文本 / 触发消息 id）。
///
/// 返回:
/// - `Ok(Some(outcome))`: 成功生成（`reply` / `session_id` / `chars`；
///   空回复不落库，由调用方跳过发送）；
/// - `Ok(None)`: 触发消息已撤回，回复作废（不落库不发送）。
pub(crate) async fn reply_to_inbound(
    engine: &Engine,
    req: ChannelReplyRequest,
) -> RamariaResult<Option<ChatSendOutcome>> {
    let config = engine.config();
    let trigger_message_id = req.trigger_message_id;
    let input = ChatInput {
        message: req.message,
        persona: Some(req.persona_uid),
        session_id: Some(req.session_id),
        mode: ChatMode::Channel,
        channel: String::new(),
        conversation_id: None,
        seed_history: Vec::new(),
        message_in_history: true,
        // 社交通道回复：出站为即时通讯平台，提示词追加通道分段声明段
        segment_style: SegmentStyle::Newline,
        proactive: None,
    };

    let prepared = prepare_request(engine, &input, config.as_ref()).await?;
    tracing::info!(
        request_id = %prepared.request_id,
        session_id = %prepared.session_id,
        persona = %prepared.persona,
        input_chars = prepared.message.chars().count(),
        "社交通道回复生成开始（记忆检索与 Prompt 装配完成）"
    );

    // ---- LLM 生成（失败不落库） ----
    let reply = step_call_llm(engine, &prepared.chat_request, prepared.session_id).await?;

    // ---- 发送前检查：触发消息已撤回则回复作废（读取失败保守放行） ----
    if let Some(trigger_id) = trigger_message_id {
        match engine.storage_ref().as_ref().get_message(trigger_id).await {
            Ok(Some(trigger)) => {
                if trigger.recalled_at.is_some() {
                    tracing::info!(
                        session_id = %prepared.session_id,
                        "触发消息已撤回，回复作废丢弃（不落库不发送）"
                    );
                    return Ok(None);
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    session_id = %prepared.session_id,
                    error = %error,
                    "触发消息读取失败，回复按正常路径处理"
                );
            }
        }
    }

    if reply.trim().is_empty() {
        tracing::warn!(
            session_id = %prepared.session_id,
            "社交通道回复未产出内容，本次不落库"
        );
        return Ok(Some(ChatSendOutcome {
            reply,
            session_id: prepared.session_id,
            chars: 0,
        }));
    }

    // ---- 只落助手回复行（入站用户消息已由通道写入用例落库） ----
    let storage = engine.storage_ref().as_ref();
    let reply_chars =
        step_persist_assistant_only(storage, prepared.session_id, &prepared.persona, &reply)
            .await?;

    tracing::info!(
        session_id = %prepared.session_id,
        persona = %prepared.persona,
        reply_chars,
        "社交通道回复生成完成（已落库）"
    );
    Ok(Some(ChatSendOutcome {
        reply,
        session_id: prepared.session_id,
        chars: reply_chars,
    }))
}
