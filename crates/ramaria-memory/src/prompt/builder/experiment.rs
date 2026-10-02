//! crates/ramaria-memory/src/prompt/builder/experiment.rs - 回复规范子段（角色层内）
//!
//! 设计特点:
//! - 组装社交对话基调 + 核心规则 + 说话锚点 + 记忆引用规则四子段
//! - 子段顺序体现"体裁约束 > 个性化 > 约束复述 > 记忆引用边界"的效力层级
//! - 陈述档（`include_social_tone=false`）不注入基调与锚点，核心规则回退中性默认
//! - 纯字符串拼接，无 I/O 与 LLM 依赖

use super::{
    CORE_RULES_DEFAULT, MEMORY_CITATION_RULES, PromptConfig, PromptContext, RESPONSE_ANCHOR,
    SOCIAL_CHAT_TONE_RULES,
};

// =========================================================
// 回复规范子段（角色层内）
// =========================================================

/// 组装回复规范子段（`## 回复规范`）：社交对话基调 + 核心规则 + 说话锚点 + 记忆引用规则。
///
/// 子段顺序（决定"体裁约束 > 个性化 > 约束复述 > 记忆引用边界"的效力层级）:
/// 1. `### 社交对话基调` — 聊天档体裁约束（`include_social_tone` 控制），
///    对全部 persona 生效；人格规则只在其之上做个性化。
/// 2. `### 核心规则` — persona 风格规则（`chat_style_rules`），缺失时用中性默认。
/// 3. `### 说话锚点` — 长度/格式约束贴近生成位置的复述（随聊天档开关）。
/// 4. `### 记忆引用规则` — 引用时机/表达方式/对方询问时的边界（随知识边界开关）。
///
/// 陈述档（`include_social_tone=false`）:
/// - 不注入基调与说话锚点；
/// - `### 核心规则` 一律回退 [CORE_RULES_DEFAULT] 中性默认（不注入 persona 规则），
///   使可及性轨保持"零字数/零格式表述"口径。
pub(super) fn build_experiment_section(context: &PromptContext, config: &PromptConfig) -> String {
    let mut parts: Vec<String> = vec!["\n\n## 回复规范".to_string()];

    // 全局社交对话基调（聊天档注入，先于 persona 风格规则；体现"先定体裁、再定个性"）
    if config.include_social_tone {
        parts.push(SOCIAL_CHAT_TONE_RULES.to_string());
    }

    // 核心回复规则：聊天档用 persona 个性化规则（缺失时中性默认）；
    // 陈述档一律回退中性默认（不注入个性化格式/风格规则）
    let persona_rules = context
        .chat_style_rules
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    match persona_rules {
        Some(rules) if config.include_social_tone => {
            parts.push(format!("\n### 核心规则\n{rules}"));
        }
        _ => parts.push(CORE_RULES_DEFAULT.to_string()),
    }

    // 说话锚点（关键约束贴近生成位置复述；陈述档不注入）
    if config.include_social_tone {
        parts.push(RESPONSE_ANCHOR.to_string());
    }

    // 记忆引用规则（引用时机/表达方式/对方询问时的边界）
    if config.include_knowledge_boundary {
        parts.push(MEMORY_CITATION_RULES.to_string());
    }

    parts.join("")
}
