//! crates/ramaria-memory/src/init/tests.rs - //! crates/ramaria-memory/src/init.rs - Ramaria 助手冷启动模块单元测试
//!
//! 设计特点:
//! - 位于 init 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 init.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// =========================================================
// persona.toml 解析测试
// =========================================================

#[test]
fn test_parse_basic_persona_toml() {
    let content = r#"[identity]
assistant_name = "黎杋枫"
user_name = "用户"

[blocks]
A_persona = "你是黎杋枫。性格知性稳重。"
E_rules = "用||分隔回复。"
"#;

    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "黎杋枫");
    assert_eq!(parsed.user_name, "用户");
    assert_eq!(parsed.blocks.len(), 2);
    assert_eq!(parsed.blocks[0].0, "A_persona");
    assert_eq!(parsed.blocks[0].1, "你是黎杋枫。性格知性稳重。");
    assert_eq!(parsed.blocks[1].0, "E_rules");
    assert_eq!(parsed.blocks[1].1, "用||分隔回复。");
}

#[test]
fn test_parse_multiline_toml() {
    let content = r#"[identity]
assistant_name = "测试助手"
user_name = "用户"

[blocks]
A_persona = """
第一行内容
第二行内容
第三行内容
"""
"#;

    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "测试助手");
    assert_eq!(parsed.blocks.len(), 1);
    assert!(parsed.blocks[0].1.contains("第一行内容"));
    assert!(parsed.blocks[0].1.contains("第二行内容"));
    assert!(parsed.blocks[0].1.contains("第三行内容"));
}

#[test]
fn test_parse_empty_blocks() {
    let content = r#"[identity]
assistant_name = "test"
user_name = "user"

[blocks]
"#;

    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "test");
    assert_eq!(parsed.blocks.len(), 0); // 空 blocks 节
}

#[test]
fn test_parse_missing_assistant_name() {
    let content = r#"[identity]
user_name = "user"
"#;

    let err = parse_persona_toml(content).unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("assistant_name"));
}

#[test]
fn test_parse_unclosed_multiline() {
    let content = r#"[identity]
assistant_name = "test"

[blocks]
A_persona = """
未闭合的多行字符串
"#;

    let err = parse_persona_toml(content).unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("未闭合"));
}

#[test]
fn test_parse_with_comments() {
    let content = r#"# 这是注释
[identity]
assistant_name = "name"
# 这也是注释
user_name = "user"
"#;

    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "name");
    assert_eq!(parsed.user_name, "user");
}

#[test]
fn test_parse_with_escaped_quotes() {
    let content = r#"[identity]
assistant_name = "name"
user_name = "user"

[blocks]
A_persona = '你是黎杋枫。被问及"是否是AI"时温柔回避。'
"#;

    // 注意：这里值使用了单引号括起来的普通字符串，
    // 解析器将其当作不带引号的原始值
    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "name");
    assert_eq!(parsed.blocks.len(), 1);
    // 单引号的值会被当作字面量（包括引号本身）
    let block_value = &parsed.blocks[0].1;
    assert!(block_value.contains("黎杋枫"));
}

// =========================================================
// 辅助函数测试
// =========================================================

/// parse_profile_field 中英文与未知字段参数化验证。
#[test]
fn test_parse_profile_field_cases() {
    let cases = [
        ("BasicInfo", ProfileField::BasicInfo),
        ("PersonalStatus", ProfileField::PersonalStatus),
        ("Interests", ProfileField::Interests),
        ("Social", ProfileField::Social),
        ("History", ProfileField::History),
        ("RecentContext", ProfileField::RecentContext),
        ("SpeakingStyle", ProfileField::SpeakingStyle),
        // 中文映射
        ("身份信息", ProfileField::BasicInfo),
        ("性格描述", ProfileField::PersonalStatus),
        ("兴趣爱好", ProfileField::Interests),
        ("社交关系", ProfileField::Social),
        ("背景", ProfileField::History),
        ("近期背景", ProfileField::RecentContext),
        ("说话风格", ProfileField::SpeakingStyle),
        // 未知字段回退为 BasicInfo
        ("UnknownField", ProfileField::BasicInfo),
    ];
    for (input, expected) in cases {
        assert_eq!(parse_profile_field(input), expected, "input={input:?}");
    }
}

/// parse_trait_layer 中英文与未知字段参数化验证。
#[test]
fn test_parse_trait_layer_cases() {
    let cases = [
        ("Base", TraitLayer::Base),
        ("Primary", TraitLayer::Primary),
        ("Accent", TraitLayer::Accent),
        ("底色", TraitLayer::Base),
        ("主色调", TraitLayer::Primary),
        ("点缀", TraitLayer::Accent),
        // 未知字段回退为 Accent
        ("UnknownLayer", TraitLayer::Accent),
    ];
    for (input, expected) in cases {
        assert_eq!(parse_trait_layer(input), expected, "input={input:?}");
    }
}

#[test]
fn test_format_blocks_for_prompt() {
    let blocks = vec![
        ("A_persona".to_string(), "你是助手".to_string()),
        ("E_rules".to_string(), "用||分隔".to_string()),
    ];
    let formatted = format_blocks_for_prompt(&blocks);
    assert!(formatted.contains("## A_persona"));
    assert!(formatted.contains("你是助手"));
    assert!(formatted.contains("## E_rules"));
    assert!(formatted.contains("用||分隔"));
}

// =========================================================
// ColdStartConfig 测试
// =========================================================

#[test]
fn test_cold_start_config_default() {
    let cfg = ColdStartConfig::default();
    assert_eq!(cfg.temperature, 0.3);
    assert_eq!(cfg.max_tokens, 4096);
}

// =========================================================
// ColdStartResponse JSON 反序列化测试
// =========================================================

#[test]
fn test_deserialize_full_response() {
    let json = r#"{
            "facts": [
                {"field": "BasicInfo", "content": "她的名字是黎杋枫", "source": "Manual"},
                {"field": "PersonalStatus", "content": "她性格知性稳重"}
            ],
            "traits": [
                {
                    "layer": "Base",
                    "trait_label": "知性稳重",
                    "meaning": "以理性克制的态度交流",
                    "not_meaning": null,
                    "trigger": "日常对话",
                    "suppress": null,
                    "related": "理性,情绪稳定"
                }
            ]
        }"#;

    let resp: ColdStartResponse = serde_json::from_str(json).expect("反序列化失败");
    assert_eq!(resp.facts.len(), 2);
    assert_eq!(resp.facts[0].field, "BasicInfo");
    assert_eq!(resp.facts[0].content, "她的名字是黎杋枫");
    assert_eq!(resp.traits.len(), 1);
    assert_eq!(resp.traits[0].layer, "Base");
    assert_eq!(resp.traits[0].trait_label, "知性稳重");
}

#[test]
fn test_deserialize_minimal_response() {
    let json = r#"{"facts": [], "traits": []}"#;
    let resp: ColdStartResponse = serde_json::from_str(json).expect("反序列化失败");
    assert!(resp.facts.is_empty());
    assert!(resp.traits.is_empty());
}

#[test]
fn test_deserialize_missing_source() {
    // source 字段可选，缺失时应为 None
    let json = r#"{
            "facts": [{"field": "BasicInfo", "content": "测试"}],
            "traits": []
        }"#;
    let resp: ColdStartResponse = serde_json::from_str(json).expect("反序列化失败");
    assert_eq!(resp.facts[0].source, None);
}

// =========================================================
// ColdStartResult 测试
// =========================================================

#[test]
fn test_cold_start_result_new() {
    let result = ColdStartResult {
        persona_uid: "rama-0001".to_string(),
        is_new: true,
        fact_count: 15,
        trait_count: 8,
    };
    assert_eq!(result.persona_uid, "rama-0001");
    assert!(result.is_new);
    assert_eq!(result.fact_count, 15);
    assert_eq!(result.trait_count, 8);
}

#[test]
fn test_cold_start_result_existing() {
    let result = ColdStartResult {
        persona_uid: "rama-0001".to_string(),
        is_new: false,
        fact_count: 20,
        trait_count: 10,
    };
    assert!(!result.is_new);
}

// =========================================================
// 共享聊天口吻示例脱敏与形状回归测试
// =========================================================

/// 共享聊天口吻的"正确示例"不含具体地点/场所等可被当作真实处境的信息。
#[test]
fn shared_style_examples_are_generic() {
    for keyword in ["扬州", "瘦西湖", "宿舍", "四月的"] {
        assert!(
            !SHARED_CHAT_STYLE_RULES.contains(keyword),
            "示例不应包含具体地点/场所字样: {keyword}"
        );
    }
}

/// 共享聊天口吻注入形状不回退：分条契约与通用闲聊示例仍在。
#[test]
fn shared_style_keeps_structured_shape() {
    let rules = SHARED_CHAT_STYLE_RULES;
    // 分条契约（前端按 || 拆分为多条气泡）
    assert!(rules.contains("需要分条时用「||」分隔"), "缺少分条契约");
    // 两组通用闲聊示例（不涉具体地点），保证示例段落可建立 || 短句直觉
    for line in ["草||先去吃饭", "怎么这样||换谁都得烦"] {
        assert!(rules.contains(line), "缺少示例行: {line}");
    }
    // 示例以「对方 / 你」对写呈现
    assert!(rules.contains("对方："));
    assert!(rules.contains("你："));
}

// =========================================================
// 聊天风格规则解析测试
// =========================================================

/// 显式非空 E_rules 优先于共享规则。
#[test]
fn resolve_chat_style_rules_prefers_explicit_e_rules() {
    let config = r#"[identity]
assistant_name = "测试"
user_name = "用户"

[blocks]
E_rules = "自定义规则内容"
"#;
    assert_eq!(resolve_chat_style_rules(Some(config)), "自定义规则内容");
}

/// 无 E_rules 块时回退共享规则。
#[test]
fn resolve_chat_style_rules_falls_back_without_e_rules() {
    let config = r#"[identity]
assistant_name = "测试"
user_name = "用户"

[blocks]
A_persona = "人设内容"
"#;
    assert_eq!(
        resolve_chat_style_rules(Some(config)),
        SHARED_CHAT_STYLE_RULES
    );
}

/// 无配置、非法配置与空白 E_rules 均回退共享规则。
#[test]
fn resolve_chat_style_rules_falls_back_on_missing_or_invalid() {
    let cases = [
        None,
        Some(""),
        Some("这不是合法的 persona 配置"),
        Some(
            r#"[identity]
assistant_name = "测试"

[blocks]
E_rules = "   "
"#,
        ),
    ];
    for case in cases {
        assert_eq!(
            resolve_chat_style_rules(case),
            SHARED_CHAT_STYLE_RULES,
            "case={case:?}"
        );
    }
}

// =========================================================
// 集成测试：完整的 persona.toml 解析 + Prompt 构建
// =========================================================

#[test]
fn test_real_persona_toml_parse_and_prompt() {
    // 使用真实的 config/persona.toml 简化版
    let content = r#"[identity]
assistant_name = "黎杋枫"
user_name = "用户"

[blocks]

A_persona = """
你是黎杋枫。女，生日3月21日。
你是用户的学习伙伴和生活挚友。
性格：知性稳重，情绪稳定，偶有冷幽默。
"""

E_rules = """
用||分隔成多条短句发送。
不反问。不重复对方的词开头。
"""
"#;

    let parsed = parse_persona_toml(content).expect("真实配置解析失败");
    assert_eq!(parsed.assistant_name, "黎杋枫");
    assert_eq!(parsed.user_name, "用户");
    assert!(parsed.blocks.len() >= 2);

    let prompt = COLD_START_PROMPT.replace(
        "{persona_config}",
        &format_blocks_for_prompt(&parsed.blocks),
    );
    assert!(prompt.contains("黎杋枫"));
    assert!(prompt.contains("知性稳重"));
    assert!(prompt.contains("用||分隔"));
    assert!(!prompt.contains("{persona_config}")); // 占位符已被替换
}
