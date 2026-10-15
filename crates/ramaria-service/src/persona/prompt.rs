//! crates/ramaria-service/src/persona/prompt.rs - Ramaria 人格提示词预览用例
//!
//! 设计特点:
//! - 两档预览：静态段（无输入骨架，不检索）与全量（给定输入，复用生成前置的召回与素材装载）
//! - 与生成前置同源：全量档复用 `chat` 模块的步骤实现，不写第二份装配逻辑
//! - 显式只读：不建会话、不写库、不触发弱反馈检测、不做状态 / 隐私门禁、不调用 LLM
//! - 隐私：日志只记 uid / 档位 / 度量，不记提示词、记忆上下文与输入文本

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::ChatMessage;
use ramaria_core::types::MessageRole;
use ramaria_memory::chat::{
    PromptMaterialInputs, RecentStateConfig, SegmentStyle, build_system_prompt,
};
use ramaria_memory::prompt::builder::measure_prompt_volume;
use uuid::Uuid;

use crate::chat::{
    step_build_prompt, step_load_examples, step_load_knowledge, step_load_narrative, step_recall,
    step_route_behavior,
};
use crate::engine::Engine;
use crate::types::{PersonaPromptPreview, PromptPreviewTier};

/// 渲染指定人格的 system prompt（静态段 / 全量两档）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标人格 uid（空白 → `Validation`）。
/// - `input`: 给定输入文本（None / 空白 = 静态段预览；非空 = 全量预览，含记忆召回）。
///
/// 返回:
/// - 预览结果（system_prompt + 记忆上下文 + 度量）；uid 空或人格不存在返回 `Validation`。
///
/// 说明:
/// - 全量档按生成前置步骤 6~12 的顺序调用同一批步骤实现，但只保留到提示装配为止
///   （不做整条 token 预算裁剪——预览无会话历史与请求上下文）；
/// - 静态档不预选示例（示例选取依赖输入），也不做记忆召回。
pub(crate) async fn preview(
    engine: &Engine,
    uid: &str,
    input: Option<&str>,
) -> RamariaResult<PersonaPromptPreview> {
    let uid = uid.trim();
    if uid.is_empty() {
        return Err(RamariaError::validation("uid 不能为空"));
    }

    let storage = engine.storage_ref().as_ref();
    if storage.get_persona_by_uid(uid).await?.is_none() {
        return Err(RamariaError::validation(format!("人格不存在: {uid}")));
    }

    let config = engine.config();
    let given_input = input.map(str::trim).filter(|text| !text.is_empty());

    let (tier, system_prompt, memory_context) = match given_input {
        None => {
            // 静态段预览：无输入骨架（不检索、不预选示例）
            let inputs = PromptMaterialInputs {
                persona_uid: Some(uid),
                recent_summaries: &[],
                last_active_at: None,
                utt_context: None,
                bridge_context: None,
                behavior_decision: None,
                examples: Vec::new(),
                max_examples: config.examples.max_examples as usize,
                knowledge_facts: Vec::new(),
                rag_covered_labels: &[],
                knowledge_budget_chars: Some(config.knowledge.injection_budget_chars),
                injection: &config.injection,
                style_enabled: config.style.enabled,
                rag_text: None,
                layer_dedup: &config.layer_dedup,
                segment_style: SegmentStyle::Pipe,
                group_conversation: false,
                recent_state: RecentStateConfig {
                    enabled: config.proactive.recent_state_enabled,
                    window_days: config.proactive.recent_state_window_days,
                },
                proactive: None,
            };
            let system_prompt = build_system_prompt(storage, &inputs).await;
            (PromptPreviewTier::Static, system_prompt, None)
        }
        Some(message) => {
            // 全量预览：与生成前置步骤 6~12 同源（召回 → 脉络 → 行为 → 知识 → 示例 → 装配）
            let (memory_context, rag_covered_labels, utt_context) =
                step_recall(engine, storage, &config, uid, message).await?;
            let narrative = step_load_narrative(engine, storage, &config, uid, message).await;
            // 预览无会话历史：以「给定输入」充当路由查询输入，会话位仅作消息元数据
            let route_messages = vec![ChatMessage {
                role: MessageRole::User,
                content: message.to_string(),
            }];
            let behavior_decision =
                step_route_behavior(engine, storage, &config, uid, Uuid::nil(), &route_messages)
                    .await;
            let knowledge_facts = step_load_knowledge(storage, &config, uid, message).await;
            let examples = step_load_examples(storage, &config, uid).await;

            let inputs = PromptMaterialInputs {
                persona_uid: Some(uid),
                recent_summaries: &narrative.recent_summaries,
                last_active_at: narrative.last_active_at.as_deref(),
                utt_context: utt_context.as_deref(),
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
                segment_style: SegmentStyle::Pipe,
                group_conversation: false,
                recent_state: RecentStateConfig {
                    enabled: config.proactive.recent_state_enabled,
                    window_days: config.proactive.recent_state_window_days,
                },
                proactive: None,
            };
            let (system_prompt, memory_context) =
                step_build_prompt(storage, &config, &inputs, memory_context.clone()).await;
            (PromptPreviewTier::Full, system_prompt, memory_context)
        }
    };

    let volume = measure_prompt_volume(&system_prompt);
    tracing::debug!(
        %uid,
        tier = ?tier,
        chars = volume.chars,
        tokens = volume.tokens,
        "人格提示词预览完成"
    );

    Ok(PersonaPromptPreview {
        uid: uid.to_string(),
        tier,
        system_prompt,
        memory_context,
        chars: volume.chars,
        tokens: volume.tokens,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
