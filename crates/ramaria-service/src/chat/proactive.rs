//! crates/ramaria-service/src/chat/proactive.rs - Ramaria 主动生成用例
//!
//! 设计特点:
//! - assistant-only：输入为选题指令而非用户消息；成功且非空才写 1 条
//!   `is_proactive=true` 的 assistant 行，失败不留半条
//! - 门禁静默跳过：人格不可见 / 状态未就绪 / 隐私未确认 / 目标会话不可用
//!   均返回 `Ok(None)`（不落库不投递）；LLM 与存储失败返回错误
//! - 复用前置编排：检索 / 历史 / 素材 / Prompt / 预算与既有生成链路同一份实现
//! - 空回复防御：LLM 成功但内容为空时本轮不落库
//! - 隐私：日志只记会话 id、人格、来源标识与长度，不记锚点 / 角度 / 语气与回复内容

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{AppState, Message, MessageRole, MessageSource};

use crate::engine::Engine;
use crate::proactive::{ProactiveDirective, ProactiveOutcome};

use super::steps::{ChatInput, ChatMode, ProactiveInput, normalize_persona, prepare_request};

// =========================================================
// 主动生成用例
// =========================================================

/// 执行主动生成用例：为指定人格生成一条主动消息（非流式、assistant-only）。
///
/// 流程:
/// 1. 输入归一（人格缺省取默认；锚点 / 角度 / 语气各自 trim 后空置 None）；
/// 2. 人格可见性检查（白名单外静默跳过）；
/// 3. 应用状态门禁（非 `Ready` 静默跳过）；
/// 4. 线上 provider 隐私确认门禁（未确认静默跳过）；
/// 5. 目标会话预检（不存在 / 已关闭 / 归属不符静默跳过）；
/// 6. 复用 `prepare_request` 装配（主动模式：锚点为消息位，请求用户消息位留空）；
/// 7. LLM 非流式生成（失败上抛且不落库）；
/// 8. 空回复防御（不落库）；
/// 9. 写入 1 条 `is_proactive=true` 的 assistant 消息（来源线上）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `directive`: 主动生成指令（选题器 / 判据裁决产出）。
///
/// 返回:
/// - `Ok(Some(outcome))`: 生成成功并已落库；
/// - `Ok(None)`: 门禁未通过或空回复（静默跳过，不落库不投递）；
/// - `Err`: 隐私状态读取 / 前置编排 / LLM / 落库失败。
pub(crate) async fn run(
    engine: &Engine,
    directive: ProactiveDirective,
) -> RamariaResult<Option<ProactiveOutcome>> {
    // ---- 1. 输入归一 ----
    let ProactiveDirective {
        persona,
        session_id,
        source,
        topic_key,
        anchor,
        angle,
        tone,
        valence,
    } = directive;
    let persona = normalize_persona(Some(persona.as_str()));
    let topic_key = normalize_optional(topic_key);
    let anchor = normalize_optional(anchor);
    let angle = normalize_optional(angle);
    let tone = normalize_optional(tone);
    tracing::info!(persona = %persona, source = %source, "主动生成开始");

    // ---- 2. 人格可见性：白名单外静默跳过（不落库不投递） ----
    if !engine.recall_policy().persona_allowed(&persona) {
        tracing::debug!(persona = %persona, "主动生成跳过：人格不在可见白名单");
        return Ok(None);
    }

    // ---- 3. 应用状态门禁：非就绪静默跳过 ----
    if engine.current_state() != AppState::Ready {
        tracing::debug!(
            persona = %persona,
            state = %engine.current_state(),
            "主动生成跳过：应用状态未就绪"
        );
        return Ok(None);
    }

    // ---- 4. 隐私门禁：线上 provider 未确认时静默跳过（存储读取失败上抛） ----
    if !crate::privacy::online_privacy_confirmed(engine).await? {
        tracing::debug!(persona = %persona, "主动生成跳过：线上服务未完成隐私确认");
        return Ok(None);
    }

    // ---- 5. 目标会话预检 ----
    // 已关闭 / 归属不符的会话不再接收任何消息，主动消息不例外；跳过原因记 debug
    if let Some(sid) = session_id {
        match engine.storage_ref().as_ref().get_session(sid).await? {
            None => {
                tracing::debug!(
                    session_id = %sid,
                    persona = %persona,
                    "主动生成跳过：目标会话不存在"
                );
                return Ok(None);
            }
            Some(session) => {
                if session.ended_at.is_some() {
                    tracing::debug!(
                        session_id = %sid,
                        persona = %persona,
                        "主动生成跳过：目标会话已关闭"
                    );
                    return Ok(None);
                }
                if let Some(uid) = session.persona_uid.as_deref() {
                    if uid != persona.as_str() {
                        tracing::debug!(
                            session_id = %sid,
                            persona = %persona,
                            "主动生成跳过：目标会话归属人格不符"
                        );
                        return Ok(None);
                    }
                }
            }
        }
    }

    // ---- 6. 复用前置编排（主动模式：锚点为消息位，请求用户消息位在编排内留空） ----
    let config = engine.config();
    let input = ChatInput {
        message: anchor.clone().unwrap_or_default(),
        persona: Some(persona.clone()),
        session_id,
        mode: ChatMode::Proactive,
        channel: String::new(),
        conversation_id: None,
        seed_history: Vec::new(),
        proactive: Some(ProactiveInput {
            anchor,
            angle,
            tone,
        }),
    };
    let prepared = prepare_request(engine, &input, config.as_ref()).await?;

    // ---- 7. LLM 生成（非流式；失败上抛且不落库） ----
    let llm = engine.llm_ref();
    let reply = match llm.chat(&prepared.chat_request).await {
        Ok(reply) => reply,
        Err(e) => {
            tracing::warn!(
                session_id = %prepared.session_id,
                persona = %prepared.persona,
                error = %e,
                "主动生成失败：LLM 调用错误"
            );
            return Err(e);
        }
    };

    // ---- 8. 空回复防御：本轮不落库 ----
    if reply.trim().is_empty() {
        tracing::warn!(
            session_id = %prepared.session_id,
            persona = %prepared.persona,
            "主动生成未产出内容，本轮不落库"
        );
        return Ok(None);
    }

    // ---- 9. assistant-only 落库（不写用户消息；来源线上，带人格与主动标记） ----
    let reply_chars = reply.chars().count();
    let assistant = Message::new(
        prepared.session_id,
        MessageRole::Assistant,
        reply.clone(),
        MessageSource::Online,
    )
    .with_persona_uid(Some(prepared.persona.clone()))
    .with_proactive(true);
    engine
        .storage_ref()
        .as_ref()
        .save_message(&assistant)
        .await?;

    // ---- 10. 完成日志（仅元数据）与结果 ----
    tracing::info!(
        session_id = %prepared.session_id,
        persona = %prepared.persona,
        source = %source,
        reply_chars,
        "主动生成完成"
    );
    Ok(Some(ProactiveOutcome {
        content: reply,
        session_id: prepared.session_id,
        persona: prepared.persona,
        source,
        topic_key,
        valence,
    }))
}

/// 归一可选文本：trim 后为空 → None。
fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
