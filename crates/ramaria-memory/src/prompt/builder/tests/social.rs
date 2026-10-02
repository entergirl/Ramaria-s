//! crates/ramaria-memory/src/prompt/builder/tests/social.rs - 全局社交对话基调
//!
//! 设计特点:
//! - 由 父测试模块 以 mod social; 收纳，经 use super::* 取用共享夹具与被测项。
//! - 用例为确定性断言，可离线运行。

use super::*;

// =========================================================
// 全局社交对话基调（无条件注入）
// =========================================================

/// 社交对话基调无条件注入：即使 persona 已有个性化风格规则也必须在场，
/// 且顺序先于 `### 核心规则`（先定体裁、再定个性）。
#[test]
fn social_tone_block_injected_before_persona_rules() {
    let ctx = PromptContext {
        persona: Some(make_test_persona()),
        chat_style_rules: Some("用||分隔短句，模仿社交平台打字节奏".to_string()),
        ..Default::default()
    };
    let result = assemble_prompt(&ctx, &PromptConfig::default());
    assert!(
        result.contains("### 社交对话基调"),
        "全局基调必须无条件注入"
    );
    assert!(
        result.contains("用||分隔短句，模仿社交平台打字节奏"),
        "persona 风格规则仍须注入（基调不替代个性）"
    );
    assert!(result.contains("### 说话锚点"), "聊天档应注入说话锚点");
    let tone_pos = result.find("### 社交对话基调").expect("基调存在");
    let core_pos = result.find("### 核心规则").expect("核心规则存在");
    let anchor_pos = result.find("### 说话锚点").expect("说话锚点存在");
    let memory_pos = result.find("### 记忆引用规则").expect("记忆引用规则存在");
    assert!(tone_pos < core_pos, "基调应先于核心规则出现");
    assert!(
        core_pos < anchor_pos && anchor_pos < memory_pos,
        "说话锚点应位于核心规则之后、记忆引用规则之前"
    );
}

/// 基调开关关闭（陈述档）时不注入基调/说话锚点，核心规则回退中性默认。
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
    assert!(!result.contains("### 社交对话基调"), "关闭后不得注入基调");
    assert!(!result.contains("### 说话锚点"), "关闭后不得注入说话锚点");
    assert!(
        !result.contains("自定义规则"),
        "陈述档不注入 persona 风格规则"
    );
    assert!(
        result.contains("不确定或不知道的内容直接说明"),
        "陈述档核心规则回退中性默认"
    );
}

/// 基调文本锁定关键约束（防后续压缩误删"杜绝助手腔"的语义）。
#[test]
fn social_tone_locks_anti_assistant_constraints() {
    for needle in [
        "不是服务对象",
        "不超过 30 字",
        "解释、总结、列点、给出方案",
        "结尾反问「需要我帮你…吗」",
        "括号内动作或神态描写",
    ] {
        assert!(
            SOCIAL_CHAT_TONE_RULES.contains(needle),
            "基调必须保留约束: {needle}"
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
