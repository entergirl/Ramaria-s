//! crates/ramaria-memory/src/preset/tests/superset.rs - 超集解析与旧文件兼容用例
//!
//! 设计特点:
//! - 覆盖全字段超集解析、无 [persona] 段旧文件兼容、真实旧文件形态
//! - 覆盖 [blocks] 保序与取值归一、文件级失败与未知节忽略

use super::*;

// =========================================================
// 超集全字段
// =========================================================

/// 完整预设：注册段 + 对话身份 + 自由文本块 + 三类结构化段 + 提示词补丁逐字段断言。
#[test]
fn superset_full_preset_parses_all_sections() {
    let content = r#"[persona]
uid = "char-0005"
kind = "char"
name = "黎杋枫"
avatar = "https://example.com/a.png"
description = "背景描述"
source = "manual"

[identity]
assistant_name = "黎杋枫"
user_name = "用户"
work = "测试作品"
era = "近未来"

[blocks]
A_persona = "人设原文"
speaking_style = "口语化"

[traits]
base = [{ label = "温柔", meaning = "对人和善", motives = ["陪伴", "安心"], not_meaning = "不是无原则" }]
primary = [{ label = "慢热", meaning = "初识时安静", trigger = "陌生人" }]
accent = [{ label = "对昆虫着迷", meaning = "会蹲下观察", suppress = "正式场合", related = "好奇" }]

[[facts]]
field = "BasicInfo"
content = "医学院学生"

[[examples]]
user = "你好"
assistant = "你好呀"

[[behavior_rules]]
keywords = ["加班", "工作"]
reaction = "先安慰"
avoid = ["说教"]
params = { emotion_intensity = -0.3, formality = 0.2 }

[prompt]
role_suffix = "角色补丁"
style_suffix = "风格补丁"
"#;

    let parsed = parse_ok(content, &ctx_default());

    // 注册段
    assert_eq!(parsed.format, PresetFormat::Toml);
    assert_eq!(parsed.persona.uid.as_deref(), Some("char-0005"));
    assert_eq!(parsed.persona.kind, Some(PersonaKind::Char));
    assert_eq!(parsed.persona.kind_source, KindSource::Explicit);
    assert_eq!(parsed.persona.name, "黎杋枫");
    assert_eq!(
        parsed.persona.avatar.as_deref(),
        Some("https://example.com/a.png")
    );
    assert_eq!(parsed.persona.description.as_deref(), Some("背景描述"));
    assert_eq!(parsed.persona.source, "manual");

    // 对话身份
    assert_eq!(parsed.identity.assistant_name, "黎杋枫");
    assert_eq!(parsed.identity.user_name.as_deref(), Some("用户"));
    assert_eq!(parsed.identity.work.as_deref(), Some("测试作品"));
    assert_eq!(parsed.identity.era.as_deref(), Some("近未来"));

    // 自由文本块
    assert_eq!(parsed.blocks.len(), 2);
    assert_eq!(
        parsed.blocks[0],
        ("A_persona".to_string(), "人设原文".to_string())
    );
    assert_eq!(
        parsed.blocks[1],
        ("speaking_style".to_string(), "口语化".to_string())
    );

    // 性格标签三层
    assert_eq!(parsed.traits.base.len(), 1);
    assert_eq!(parsed.traits.primary.len(), 1);
    assert_eq!(parsed.traits.accent.len(), 1);
    let base = &parsed.traits.base[0];
    assert_eq!(base.layer, TraitLayer::Base);
    assert_eq!(base.label, "温柔");
    assert_eq!(base.meaning, "对人和善");
    assert_eq!(base.not_meaning.as_deref(), Some("不是无原则"));
    assert_eq!(base.motives, vec!["陪伴".to_string(), "安心".to_string()]);
    assert_eq!(parsed.traits.primary[0].trigger.as_deref(), Some("陌生人"));
    assert_eq!(
        parsed.traits.accent[0].suppress.as_deref(),
        Some("正式场合")
    );
    assert_eq!(parsed.traits.accent[0].related.as_deref(), Some("好奇"));

    // 事实与示例
    assert_eq!(parsed.facts.len(), 1);
    assert_eq!(parsed.facts[0].field, ProfileField::BasicInfo);
    assert_eq!(parsed.facts[0].content, "医学院学生");
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(parsed.examples[0].user, "你好");
    assert_eq!(parsed.examples[0].assistant, "你好呀");

    // 行为规则（显式键覆盖默认，未给键保持默认）
    assert_eq!(parsed.rules.len(), 1);
    let rule = &parsed.rules[0];
    assert_eq!(rule.keywords, vec!["加班".to_string(), "工作".to_string()]);
    assert_eq!(rule.reaction.as_deref(), Some("先安慰"));
    assert_eq!(rule.avoid, vec!["说教".to_string()]);
    assert_eq!(rule.params.emotional_intensity, -0.3);
    assert_eq!(rule.params.formality, 0.2);
    assert_eq!(rule.params.proactiveness, 0.5);
    assert_eq!(rule.params.detail_level, 0.5);

    // 提示词补丁
    assert_eq!(parsed.prompt.role_suffix.as_deref(), Some("角色补丁"));
    assert_eq!(parsed.prompt.style_suffix.as_deref(), Some("风格补丁"));

    // 干净输入不产生提示与跳过明细
    assert!(parsed.hints.is_empty(), "不应有提示: {:?}", parsed.hints);
    assert!(parsed.skipped.is_empty());
    assert!(parsed.personality_raw.is_none());
}

// =========================================================
// 旧文件兼容
// =========================================================

/// 无 [persona] 段的旧文件：uid 取文件名 stem、kind 前缀推断、描述走遗留键并提示迁移。
#[test]
fn legacy_file_without_persona_section() {
    let content = r#"[identity]
assistant_name = "旧助手"
user_name = "用户"
description = "遗留描述"

[blocks]
A_persona = "人设"
"#;

    let parsed = parse_ok(content, &ctx_with_stem("char-0009"));
    assert_eq!(parsed.persona.uid.as_deref(), Some("char-0009"));
    assert_eq!(parsed.persona.kind, Some(PersonaKind::Char));
    assert_eq!(parsed.persona.kind_source, KindSource::Inferred);
    assert_eq!(parsed.persona.name, "旧助手");
    assert_eq!(parsed.identity.assistant_name, "旧助手");
    assert_eq!(parsed.persona.description.as_deref(), Some("遗留描述"));
    assert_eq!(parsed.persona.source, "manual");

    // 遗留描述提示为 info 级迁移建议
    let hint = parsed
        .hints
        .iter()
        .find(|h| h.code == "legacy_description_key")
        .expect("应提示遗留描述键");
    assert_eq!(hint.level, HintLevel::Info);
}

/// 无文件上下文且无名称与 uid：uid = None、kind 待定、名称缺失为文件级失败。
#[test]
fn missing_name_without_stem_is_file_level_error() {
    let content = r#"[identity]
user_name = "用户"
"#;
    let err = parse_toml(content, &ctx_default()).unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("assistant_name"),
        "错误消息应提示名称来源: {message}"
    );
}

/// 无 [persona] 段但有文件 stem：stem 同时兜底 uid 与名称。
#[test]
fn stem_fills_uid_and_name_when_absent() {
    let content = r#"[blocks]
A_persona = "人设"
"#;
    let parsed = parse_ok(content, &ctx_with_stem("char-0007"));
    assert_eq!(parsed.persona.uid.as_deref(), Some("char-0007"));
    assert_eq!(parsed.persona.name, "char-0007");
    assert_eq!(parsed.identity.assistant_name, "char-0007");
}

// =========================================================
// 真实旧文件形态
// =========================================================

/// 真实随包文件形态（[identity] + 多行 [blocks]）：名称与块顺序、内容与旧口径一致。
#[test]
fn real_persona_file_shape_parses() {
    let content = r#"[identity]
assistant_name = "黎杋枫"
user_name = "用户"

[blocks]

A_persona = """
你是黎杋枫。女，生日1月1日。
你是用户的学习伙伴和生活挚友。
始终以黎杋枫身份对话。被问及是否是AI时，温柔回避，不承认，不否认。

性格：知性稳重，情绪稳定，偶有冷幽默。知识面广，不卖弄。关注用户的状态和情绪。

讲解时：用生活化比喻替代专业术语。引导探索，不直接给答案。
"""

E_rules = """
回复格式要求：
1. 分条：分两次说就用「||」隔开，一次最多 3 条。
2. 接话：从自己的内容接起；不重复对方的话。
3. 收尾：说完即停；不追加计划、安排或其他延伸内容。
4. 讲解：使用生活化的比方；引导对方自己得出结论，不直接给出答案。
"""
"#;

    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.identity.assistant_name, "黎杋枫");
    assert_eq!(parsed.identity.user_name.as_deref(), Some("用户"));
    assert_eq!(parsed.blocks.len(), 2);
    assert_eq!(parsed.blocks[0].0, "A_persona");
    assert_eq!(parsed.blocks[1].0, "E_rules");
    assert!(parsed.blocks[0].1.contains("你是黎杋枫。"));
    assert!(parsed.blocks[0].1.contains("性格：知性稳重"));
    assert!(parsed.blocks[0].1.contains("引导探索，不直接给答案。"));
    assert!(parsed.blocks[1].1.contains("用「||」分隔"));
    assert!(parsed.blocks[1].1.starts_with("回复格式要求："));
    // 多行块值不含 CR 且末尾无残留空白
    for (_, value) in &parsed.blocks {
        assert!(!value.contains('\r'), "块值不应含 CR");
        assert_eq!(value, value.trim_end(), "块值末尾空白已归一");
    }
}

/// CRLF 文本的多行块值折为 LF（对齐行级解析口径）。
#[test]
fn crlf_multiline_block_normalizes_to_lf() {
    let content = "[identity]\r\nassistant_name = \"测试\"\r\n\r\n[blocks]\r\nA_persona = \"\"\"\r\n第一行\r\n第二行\r\n\"\"\"\r\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.blocks.len(), 1);
    assert_eq!(parsed.blocks[0].1, "第一行\n第二行");
}

// =========================================================
// [blocks] 取值与保序
// =========================================================

/// 单行与多行混合、保文件序、非字符串值忽略。
#[test]
fn blocks_keeps_file_order_and_ignores_non_string_values() {
    let content = r#"[identity]
assistant_name = "测试"

[blocks]
first = "一"
second = 42
third = """
多行
内容
"""
fourth = "四"
"#;
    let parsed = parse_ok(content, &ctx_default());
    let keys: Vec<&str> = parsed.blocks.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["first", "third", "fourth"]);
    assert_eq!(parsed.blocks[1].1, "多行\n内容");
    assert_hint(&parsed, "blocks_value_ignored");
}

/// 空 blocks 节：正常解析、无块内容。
#[test]
fn empty_blocks_section_parses() {
    let content = r#"[identity]
assistant_name = "测试"

[blocks]
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.blocks.is_empty());
}

/// 空值块保留（值 trim_end 后为空串）。
#[test]
fn blocks_keeps_empty_values() {
    let content = r#"[identity]
assistant_name = "测试"

[blocks]
A_persona = ""
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(
        parsed.blocks,
        vec![("A_persona".to_string(), String::new())]
    );
}

/// [blocks] 非表：提示并视为空。
#[test]
fn blocks_non_table_is_ignored() {
    let content = "blocks = \"不是表\"\n\n[identity]\nassistant_name = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.blocks.is_empty());
    assert_hint(&parsed, "blocks_section_invalid");
}

// =========================================================
// 文件级失败与未知节
// =========================================================

/// TOML 语法非法：文件级失败并带解析错误位置信息。
#[test]
fn toml_syntax_error_is_file_level_failure() {
    let content = "[identity]\nassistant_name = \"未闭合\n";
    let err = parse_toml(content, &ctx_default()).unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("解析 persona.toml 失败"),
        "消息: {message}"
    );
}

/// 未知顶层节忽略且不产生提示（宽容读取）。
#[test]
fn unknown_top_level_section_is_ignored() {
    let content = r#"[identity]
assistant_name = "测试"

[future_section]
key = "value"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.name, "测试");
    assert!(parsed.hints.is_empty());
}

/// 条目级非法不阻断整体解析（仅文件级 5 类才 Err）。
#[test]
fn entry_level_problems_do_not_fail_whole_parse() {
    let content = r#"[persona]
uid = "char-0001"
name = "测试"

[traits]
base = [{ label = "只有标签" }]

[[facts]]
field = "UnknownField"
content = "内容"

[[behavior_rules]]
reaction = "没有关键词"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.traits.base.is_empty());
    assert!(parsed.facts.is_empty());
    assert!(parsed.rules.is_empty());
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Traits, SkipReason::Invalid),
        1
    );
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Facts, SkipReason::Invalid),
        1
    );
    assert_eq!(
        count_skipped(&parsed, SkipCategory::BehaviorRules, SkipReason::Invalid),
        1
    );
}
