//! crates/ramaria-memory/src/prompt/builder/experiment.rs - 说话方式与回复规则子段（角色层内）
//!
//! 设计特点:
//! - 组装说话方式 + 核心规则 + 回复格式三子段（扁平结构，无父级标题）
//! - 说话方式为全局体裁约束（陈述档不注入）；核心规则为 persona 个性化（缺省中性默认）
//! - 陈述档（`include_social_tone=false`）不注入说话方式与回复格式，核心规则回退中性默认
//! - 纯字符串拼接，无 I/O 与 LLM 依赖

use super::{
    CORE_RULES_DEFAULT, PromptConfig, PromptContext, RESPONSE_ANCHOR, SOCIAL_CHAT_TONE_RULES,
};

// =========================================================
// 说话与回复规则子段（角色层内）
// =========================================================

/// 组装说话与回复规则子段：说话方式 + 核心规则 + 回复格式。
///
/// 子段顺序（决定"体裁约束 > 个性化 > 格式契约"的效力层级）:
/// 1. `## 说话方式` — 聊天档体裁约束（`include_social_tone` 控制），
///    对全部 persona 生效；人格规则只在其之上做个性化。
/// 2. `## 核心规则` — persona 风格规则（`chat_style_rules`），缺失时用中性默认。
/// 3. `## 回复格式` — `||` 分条契约与长度兜底（随聊天档开关）。
///
/// 陈述档（`include_social_tone=false`）:
/// - 不注入说话方式与回复格式；
/// - `## 核心规则` 一律回退 [CORE_RULES_DEFAULT] 中性默认（不注入 persona 规则），
///   使可及性轨保持"零字数/零格式表述"口径。
pub(super) fn build_experiment_section(context: &PromptContext, config: &PromptConfig) -> String {
    let mut sections: Vec<String> = Vec::new();

    // 说话方式（全局体裁约束，先于 persona 风格规则；体现"先定体裁、再定个性"）
    if config.include_social_tone {
        sections.push(SOCIAL_CHAT_TONE_RULES.to_string());
    }

    // 核心回复规则：聊天档用 persona 个性化规则（缺失时中性默认）；
    // 陈述档一律回退中性默认（不注入个性化格式/风格规则）
    let persona_rules = context
        .chat_style_rules
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    match persona_rules {
        Some(rules) if config.include_social_tone => {
            sections.push(format!("## 核心规则\n{rules}"));
        }
        _ => sections.push(CORE_RULES_DEFAULT.to_string()),
    }

    // 回复格式（`||` 分条契约与长度兜底；陈述档不注入）
    if config.include_social_tone {
        sections.push(RESPONSE_ANCHOR.to_string());
    }

    format!("\n\n{}", sections.join("\n\n"))
}
