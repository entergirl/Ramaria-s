//! crates/ramaria-memory/src/prompt/builder/tests/social.rs - 全局说话方式（体裁约束）
//!
//! 设计特点:
//! - 由 父测试模块 以 mod social; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 全局说话方式（聊天档注入）
// =========================================================

/// 说话方式无条件注入：即使 persona 已有个性化风格规则也必须在场，
/// 且顺序先于 `## 核心规则`（先定体裁、再定个性）。
#[test]
fn social_tone_block_injected_before_persona_rules() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        chat_style_rules: Some("用||分隔短句，模仿社交平台打字节奏".to_string()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(result.contains("## 说话方式"), "全局说话方式必须无条件注入");
    assert!(
        result.contains("用||分隔短句，模仿社交平台打字节奏"),
        "persona 风格规则仍须注入（体裁不替代个性）"
    );
    assert!(result.contains("## 回复格式"), "聊天档应注入回复格式");
    let tone_pos = result.find("## 说话方式").expect("说话方式存在");
    let core_pos = result.find("## 核心规则").expect("核心规则存在");
    let format_pos = result.find("## 回复格式").expect("回复格式存在");
    assert!(tone_pos < core_pos, "说话方式应先于核心规则出现");
    assert!(core_pos < format_pos, "回复格式应位于核心规则之后");
}

/// 说话方式开关关闭（陈述档）时不注入说话方式/回复格式，核心规则回退中性默认。
#[test]
fn social_tone_block_can_be_disabled() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        chat_style_rules: Some("自定义规则".to_string()),
        ..Default::default()
    };
    let config = PromptConfig {
        include_social_tone: false,
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &config);
    assert!(!result.contains("## 说话方式"), "关闭后不得注入说话方式");
    assert!(!result.contains("## 回复格式"), "关闭后不得注入回复格式");
    assert!(
        !result.contains("自定义规则"),
        "陈述档不注入 persona 风格规则"
    );
    assert!(
        result.contains("不确定或不知道的内容直接说明"),
        "陈述档核心规则回退中性默认"
    );
}

/// 说话方式文本锁定关键约束（防后续压缩误删"去助手腔"的语义）。
#[test]
fn social_tone_locks_anti_assistant_constraints() {
    for needle in [
        "不是客户",
        "不是在完成服务任务",
        "一两个字的回复也是正常的",
        "不解释、不总结、不列点、不给建议",
        "不用「还需要别的吗」这类追问",
        "不代替对方说话",
    ] {
        assert!(
            SOCIAL_CHAT_TONE_RULES.contains(needle),
            "说话方式必须保留约束: {needle}"
        );
    }
}

/// 度量函数基本正确性（字符数 + token 估算，复用 estimate_tokens）。
#[test]
fn measure_prompt_volume_counts_chars_and_tokens() {
    let empty = measure_prompt_volume("");
    assert_eq!(empty.chars, 0);
    assert_eq!(empty.tokens, 0);

    let vol = measure_prompt_volume("# 记忆（脉络层）\n你好世界 Hello");
    assert_eq!(
        vol.chars,
        "# 记忆（脉络层）\n你好世界 Hello".chars().count()
    );
    assert_eq!(
        vol.tokens,
        crate::token_budget::estimate_tokens("# 记忆（脉络层）\n你好世界 Hello")
    );
}
