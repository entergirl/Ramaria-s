//! crates/ramaria-memory/src/preset/tests/ser.rs - 预设序列化用例
//!
//! 设计特点:
//! - 覆盖超集 TOML round-trip 语义等价（TOML 来源与 ST 来源）
//! - 覆盖 ST personality 原文到 [blocks].A_persona 的降级转化与不覆盖语义
//! - 覆盖空段省略与字节入口再消费

use super::*;
use ramaria_core::behavior::BehaviorParams;

// =========================================================
// 构造辅助
// =========================================================

/// 全字段预设中间表示（TOML 来源形态）。
fn full_preset() -> ParsedPreset {
    ParsedPreset {
        format: PresetFormat::Toml,
        persona: PersonaSection {
            uid: Some("char-0005".to_string()),
            kind: Some(PersonaKind::Char),
            kind_source: KindSource::Explicit,
            name: "黎杋枫".to_string(),
            avatar: Some("https://example.com/a.png".to_string()),
            description: Some("背景描述".to_string()),
            source: "manual".to_string(),
        },
        identity: IdentitySection {
            assistant_name: "黎杋枫".to_string(),
            user_name: Some("用户".to_string()),
            work: Some("测试作品".to_string()),
            era: None,
        },
        blocks: vec![
            ("E_rules".to_string(), "规则文本".to_string()),
            (
                "speaking_style".to_string(),
                "风格第一行\n风格第二行".to_string(),
            ),
        ],
        traits: TraitsSection {
            base: vec![TraitEntry {
                layer: TraitLayer::Base,
                label: "温柔".to_string(),
                meaning: "待人温和".to_string(),
                not_meaning: Some("不是软弱".to_string()),
                trigger: None,
                suppress: None,
                related: Some("耐心".to_string()),
                motives: vec!["陪伴".to_string()],
            }],
            primary: vec![TraitEntry {
                layer: TraitLayer::Primary,
                label: "慢热".to_string(),
                meaning: "初识内敛".to_string(),
                not_meaning: None,
                trigger: Some("陌生人".to_string()),
                suppress: Some("独处".to_string()),
                related: None,
                motives: Vec::new(),
            }],
            accent: Vec::new(),
        },
        facts: vec![
            FactEntry {
                field: ProfileField::BasicInfo,
                content: "医学院学生".to_string(),
            },
            FactEntry {
                field: ProfileField::Interests,
                content: "喜欢昆虫".to_string(),
            },
        ],
        examples: vec![ExampleEntry {
            user: "你好".to_string(),
            assistant: "你好呀".to_string(),
        }],
        rules: vec![
            RuleEntry {
                keywords: vec!["加班".to_string(), "工作".to_string()],
                reaction: Some("别太累".to_string()),
                avoid: vec!["说教".to_string()],
                params: BehaviorParams {
                    emotional_intensity: -0.3,
                    proactiveness: 0.4,
                    detail_level: 0.6,
                    formality: 0.2,
                },
            },
            RuleEntry {
                keywords: vec!["周末".to_string()],
                reaction: None,
                avoid: Vec::new(),
                params: BehaviorParams::default(),
            },
        ],
        prompt: PromptPatch {
            role_suffix: Some("角色后缀".to_string()),
            style_suffix: None,
        },
        personality_raw: None,
        hints: Vec::new(),
        skipped: Vec::new(),
    }
}

/// round-trip 关键字段逐项等价断言（persona 的 kind_source 不比较）。
fn assert_roundtrip_eq(expected: &ParsedPreset, actual: &ParsedPreset) {
    assert_eq!(actual.format, PresetFormat::Toml);
    assert_eq!(actual.persona.uid, expected.persona.uid);
    assert_eq!(actual.persona.kind, expected.persona.kind);
    assert_eq!(actual.persona.name, expected.persona.name);
    assert_eq!(actual.persona.avatar, expected.persona.avatar);
    assert_eq!(actual.persona.description, expected.persona.description);
    assert_eq!(actual.persona.source, expected.persona.source);
    assert_eq!(actual.identity, expected.identity);
    assert_eq!(actual.blocks, expected.blocks);
    assert_eq!(actual.traits, expected.traits);
    assert_eq!(actual.facts, expected.facts);
    assert_eq!(actual.examples, expected.examples);
    assert_eq!(actual.rules, expected.rules);
    assert_eq!(actual.prompt, expected.prompt);
}

/// 查找 blocks 中指定键的值。
fn block_value<'a>(parsed: &'a ParsedPreset, key: &str) -> Option<&'a str> {
    parsed
        .blocks
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, value)| value.as_str())
}

// =========================================================
// round-trip
// =========================================================

/// 全字段 round-trip：序列化文本再解析，关键字段逐项等价。
#[test]
fn superset_toml_roundtrip_keeps_all_fields() {
    let expected = full_preset();
    let text = to_superset_toml(&expected).expect("序列化应当成功");

    let actual = parse_ok(&text, &ctx_default());
    assert_roundtrip_eq(&expected, &actual);
}

/// ST 来源 round-trip：personality 原文转化为 [blocks].A_persona，再解析后 personality_raw 为 None。
#[test]
fn st_personality_raw_becomes_blocks_a_persona() {
    let card = r#"{"name":"黎杋枫","personality":"第一行人设\n第二行人设"}"#;
    let parsed = parse_st_json(card, &ctx_default()).expect("ST 卡解析应当成功");
    assert_eq!(
        parsed.personality_raw.as_deref(),
        Some("第一行人设\n第二行人设")
    );

    let text = to_superset_toml(&parsed).expect("序列化应当成功");
    assert!(text.contains("[blocks]"), "序列化文本应含 [blocks] 节");
    assert!(text.contains("A_persona"), "序列化文本应含 A_persona 键");

    let reparsed = parse_ok(&text, &ctx_default());
    assert!(
        reparsed.personality_raw.is_none(),
        "再解析后 personality_raw 应为 None"
    );
    assert_eq!(
        block_value(&reparsed, "A_persona"),
        Some("第一行人设\n第二行人设")
    );
}

/// blocks 已有 A_persona 时 personality_raw 不覆盖（blocks 优先）。
#[test]
fn existing_persona_block_wins_over_personality_raw() {
    let mut parsed = full_preset();
    parsed.personality_raw = Some("另一版本人设原文".to_string());
    parsed.blocks = vec![
        ("A_persona".to_string(), "已有块内容".to_string()),
        ("E_rules".to_string(), "规则文本".to_string()),
    ];

    let text = to_superset_toml(&parsed).expect("序列化应当成功");
    assert!(
        !text.contains("另一版本人设原文"),
        "personality_raw 不应写出"
    );

    let reparsed = parse_ok(&text, &ctx_default());
    assert_eq!(reparsed.blocks.len(), 2);
    assert_eq!(block_value(&reparsed, "A_persona"), Some("已有块内容"));
}

/// 空段省略：无结构化段与补丁时序列化文本不含对应节，再解析为空。
#[test]
fn empty_sections_are_omitted() {
    let mut parsed = full_preset();
    parsed.blocks.clear();
    parsed.traits = TraitsSection::default();
    parsed.facts.clear();
    parsed.examples.clear();
    parsed.rules.clear();
    parsed.prompt = PromptPatch::default();
    parsed.personality_raw = None;

    let text = to_superset_toml(&parsed).expect("序列化应当成功");
    assert!(!text.contains("[blocks]"));
    assert!(!text.contains("[traits]"));
    assert!(!text.contains("facts"));
    assert!(!text.contains("examples"));
    assert!(!text.contains("behavior_rules"));
    assert!(!text.contains("[prompt]"));

    let reparsed = parse_ok(&text, &ctx_default());
    assert!(reparsed.blocks.is_empty());
    assert_eq!(reparsed.traits, TraitsSection::default());
    assert!(reparsed.facts.is_empty());
    assert!(reparsed.examples.is_empty());
    assert!(reparsed.rules.is_empty());
    assert_eq!(reparsed.prompt, PromptPatch::default());
}

/// 序列化文本可被字节入口 parse_preset 直接消费（UTF-8 文本 → Toml 分支）。
#[test]
fn serialized_text_is_consumable_by_byte_entry() {
    let expected = full_preset();
    let text = to_superset_toml(&expected).expect("序列化应当成功");

    let actual = parse_preset(text.as_bytes(), &ctx_default()).expect("字节入口解析应当成功");
    assert_eq!(actual.format, PresetFormat::Toml);
    assert_roundtrip_eq(&expected, &actual);
}
