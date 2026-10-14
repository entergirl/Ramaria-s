//! crates/ramaria-memory/src/preset/tests/sections.rs - 结构化段（traits / facts / examples / rules / prompt）用例
//!
//! 设计特点:
//! - 覆盖三层 traits 的去重、溢出、截断与未知层键
//! - 覆盖 facts 值域归一与判重、examples 判重、rules 关键词与参数校验
//! - 覆盖提示（hints）与跳过明细（skipped）成对记录

use super::*;

// =========================================================
// traits
// =========================================================

/// 未知层键：提示并忽略，不影响其他层。
#[test]
fn unknown_traits_layer_is_ignored() {
    let content = r#"[persona]
name = "测试"

[traits]
base = [{ label = "温柔", meaning = "对人和善" }]
unknown = [{ label = "忽略", meaning = "不应解析" }]
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.traits.base.len(), 1);
    assert_hint(&parsed, "unknown_traits_layer");
}

/// 层值非数组：提示并跳过该层。
#[test]
fn traits_layer_non_array_is_skipped() {
    let content = r#"[persona]
name = "测试"

[traits]
base = "不是数组"
primary = [{ label = "慢热", meaning = "初识安静" }]
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.traits.base.is_empty());
    assert_eq!(parsed.traits.primary.len(), 1);
    assert_hint(&parsed, "traits_layer_invalid");
}

/// 缺 label / 缺 meaning / 条目非表：逐条拒绝并进跳过明细。
#[test]
fn invalid_trait_entries_are_skipped_with_details() {
    let content = r#"[persona]
name = "测试"

[traits]
base = [
    { meaning = "缺标签" },
    { label = "缺含义" },
    { label = "完整", meaning = "可用" },
]
primary = ["不是表"]
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.traits.base.len(), 1);
    assert_eq!(parsed.traits.base[0].label, "完整");
    assert!(parsed.traits.primary.is_empty());
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Traits, SkipReason::Invalid),
        3
    );
    // 跨层 label 重复不存在时明细 label 保留可读标识
    assert!(
        parsed
            .skipped
            .iter()
            .any(|s| s.label.as_deref() == Some("缺含义"))
    );
}

/// 跨层 label 完全重复：后者跳过，明细带 label 与 LabelDuplicate。
#[test]
fn cross_layer_duplicate_label_is_skipped() {
    let content = r#"[persona]
name = "测试"

[traits]
base = [{ label = "慢热", meaning = "初识安静" }]
primary = [{ label = "慢热", meaning = "重复声明" }]
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.traits.base.len(), 1);
    assert!(parsed.traits.primary.is_empty());
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Traits, SkipReason::LabelDuplicate),
        1
    );
    let entry = parsed
        .skipped
        .iter()
        .find(|s| s.reason == SkipReason::LabelDuplicate)
        .expect("应记录重复明细");
    assert_eq!(entry.label.as_deref(), Some("慢热"));
    assert_hint(&parsed, "trait_label_duplicate");
}

/// 单层第 31 条起拒绝：前 30 条保留，超出者进 Overflow 明细。
#[test]
fn traits_layer_overflow_rejects_excess() {
    let mut items: Vec<String> = Vec::new();
    for index in 1..=32 {
        items.push(format!(
            "{{ label = \"标签{index}\", meaning = \"含义{index}\" }}"
        ));
    }
    let content = format!(
        "[persona]\nname = \"测试\"\n\n[traits]\nbase = [{}]\n",
        items.join(", ")
    );
    let parsed = parse_ok(&content, &ctx_default());
    assert_eq!(parsed.traits.base.len(), 30);
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Traits, SkipReason::Overflow),
        2
    );
    assert_hint(&parsed, "traits_layer_overflow");
}

/// meaning 超 200 字符：截断保留该条并提示。
#[test]
fn overlong_meaning_is_truncated_with_hint() {
    let long_meaning = "义".repeat(250);
    let content = format!(
        "[persona]\nname = \"测试\"\n\n[traits]\nbase = [{{ label = \"标签\", meaning = \"{long_meaning}\" }}]\n"
    );
    let parsed = parse_ok(&content, &ctx_default());
    let entry = &parsed.traits.base[0];
    assert_eq!(entry.meaning.chars().count(), 200);
    assert_hint(&parsed, "meaning_truncated");
}

/// motives 非数组项忽略、空项剔除。
#[test]
fn motives_parse_filters_invalid_items() {
    let content = r#"[persona]
name = "测试"

[traits]
base = [{ label = "温柔", meaning = "对人和善", motives = ["陪伴", "", 42, "安心"] }]
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(
        parsed.traits.base[0].motives,
        vec!["陪伴".to_string(), "安心".to_string()]
    );
}

/// [traits] 节非表：提示并视为空。
#[test]
fn traits_section_non_table_is_ignored() {
    let content = "traits = 3\n\n[persona]\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.traits.base.is_empty());
    assert_hint(&parsed, "traits_section_invalid");
}

// =========================================================
// facts
// =========================================================

/// 七值枚举大小写不敏感 + 下划线归一。
#[test]
fn fact_fields_normalize_case_and_underscore() {
    let cases = [
        ("BasicInfo", ProfileField::BasicInfo),
        ("basic_info", ProfileField::BasicInfo),
        ("BASICINFO", ProfileField::BasicInfo),
        ("PersonalStatus", ProfileField::PersonalStatus),
        ("interests", ProfileField::Interests),
        ("SOCIAL", ProfileField::Social),
        ("History", ProfileField::History),
        ("recent_context", ProfileField::RecentContext),
        ("SPEAKING_STYLE", ProfileField::SpeakingStyle),
    ];
    for (raw, expected) in cases {
        let content = format!(
            "[persona]\nname = \"测试\"\n\n[[facts]]\nfield = \"{raw}\"\ncontent = \"内容\"\n"
        );
        let parsed = parse_ok(&content, &ctx_default());
        assert_eq!(parsed.facts[0].field, expected, "field={raw}");
    }
}

/// 未知 field：拒绝该条（不静默回退），明细与提示成对。
#[test]
fn unknown_fact_field_is_rejected() {
    let content = r#"[persona]
name = "测试"

[[facts]]
field = "FavoriteFood"
content = "火锅"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.facts.is_empty());
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Facts, SkipReason::Invalid),
        1
    );
    assert_hint(&parsed, "fact_field_invalid");
}

/// 缺 content：拒绝该条。
#[test]
fn fact_without_content_is_rejected() {
    let content = "[persona]\nname = \"测试\"\n\n[[facts]]\nfield = \"BasicInfo\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.facts.is_empty());
    assert_hint(&parsed, "fact_entry_invalid");
}

/// 同 field 且内容相同：后者跳过进 Duplicate 明细。
#[test]
fn duplicate_fact_is_skipped() {
    let content = r#"[persona]
name = "测试"

[[facts]]
field = "BasicInfo"
content = "医学院学生"

[[facts]]
field = "BasicInfo"
content = "  医学院学生  "
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.facts.len(), 1);
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Facts, SkipReason::Duplicate),
        1
    );
    assert_hint(&parsed, "fact_duplicate");
}

/// content 超 500 字符：截断保留并提示。
#[test]
fn overlong_fact_content_is_truncated_with_hint() {
    let long_content = "容".repeat(520);
    let content = format!(
        "[persona]\nname = \"测试\"\n\n[[facts]]\nfield = \"BasicInfo\"\ncontent = \"{long_content}\"\n"
    );
    let parsed = parse_ok(&content, &ctx_default());
    assert_eq!(parsed.facts[0].content.chars().count(), 500);
    assert_hint(&parsed, "content_truncated");
}

// =========================================================
// examples
// =========================================================

/// 正常示例解析。
#[test]
fn examples_parse_pairs() {
    let content = r#"[persona]
name = "测试"

[[examples]]
user = "你好"
assistant = "你好呀"

[[examples]]
user = "在吗"
assistant = "在的"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 2);
    assert_eq!(parsed.examples[1].assistant, "在的");
}

/// 缺 user 或 assistant：拒绝该条。
#[test]
fn incomplete_example_is_rejected() {
    let content = r#"[persona]
name = "测试"

[[examples]]
user = "你好"

[[examples]]
assistant = "只有回复"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.examples.is_empty());
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Examples, SkipReason::Invalid),
        2
    );
    assert_hint(&parsed, "example_entry_invalid");
}

/// 完全一致的示例：后者跳过。
#[test]
fn duplicate_example_is_skipped() {
    let content = r#"[persona]
name = "测试"

[[examples]]
user = "你好"
assistant = "你好呀"

[[examples]]
user = "你好"
assistant = "你好呀"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Examples, SkipReason::Duplicate),
        1
    );
    assert_hint(&parsed, "example_duplicate");
}

// =========================================================
// behavior_rules
// =========================================================

/// 关键词去重保首现、空项剔除。
#[test]
fn rule_keywords_dedup_and_trim() {
    let content = r#"[persona]
name = "测试"

[[behavior_rules]]
keywords = ["加班", "加班", "工作", "   "]
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(
        parsed.rules[0].keywords,
        vec!["加班".to_string(), "工作".to_string()]
    );
}

/// 缺 keywords / 全空：拒绝整条。
#[test]
fn rule_without_keywords_is_rejected() {
    let content = r#"[persona]
name = "测试"

[[behavior_rules]]
reaction = "没有关键词"

[[behavior_rules]]
keywords = []
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert!(parsed.rules.is_empty());
    assert_eq!(
        count_skipped(&parsed, SkipCategory::BehaviorRules, SkipReason::Invalid),
        2
    );
    assert_hint(&parsed, "rule_keywords_invalid");
}

/// reaction 空串按未配置（None = 候选规则）。
#[test]
fn blank_reaction_is_candidate_rule() {
    let content = "[persona]\nname = \"测试\"\n\n[[behavior_rules]]\nkeywords = [\"加班\"]\nreaction = \"\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.rules.len(), 1);
    assert_eq!(parsed.rules[0].reaction, None);
}

/// params 缺省各用默认值，显式键覆盖（部分覆盖）。
#[test]
fn rule_params_default_with_partial_override() {
    let content = r#"[persona]
name = "测试"

[[behavior_rules]]
keywords = ["加班"]
params = { proactiveness = 0.8 }
"#;
    let parsed = parse_ok(content, &ctx_default());
    let params = parsed.rules[0].params;
    assert_eq!(params.proactiveness, 0.8);
    assert_eq!(params.emotional_intensity, 0.0);
    assert_eq!(params.detail_level, 0.5);
    assert_eq!(params.formality, 0.5);
}

/// params 越界 / 非数值 / 非表：拒绝整条。
#[test]
fn invalid_rule_params_rejects_whole_entry() {
    let cases = [
        "params = { proactiveness = 1.5 }",
        "params = { emotion_intensity = -1.2 }",
        "params = { detail_level = \"high\" }",
        "params = 3",
    ];
    for params in cases {
        let content = format!(
            "[persona]\nname = \"测试\"\n\n[[behavior_rules]]\nkeywords = [\"加班\"]\n{params}\n"
        );
        let parsed = parse_ok(&content, &ctx_default());
        assert!(parsed.rules.is_empty(), "params={params}");
        assert_hint(&parsed, "rule_params_invalid");
    }
}

/// 整数参数值可接受（转 f64）。
#[test]
fn integer_rule_params_accepted() {
    let content = "[persona]\nname = \"测试\"\n\n[[behavior_rules]]\nkeywords = [\"加班\"]\nparams = { formality = 1 }\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.rules[0].params.formality, 1.0);
}

/// 关键词集合与 reaction 一致（顺序无关）：后者跳过。
#[test]
fn duplicate_rule_is_skipped() {
    let content = r#"[persona]
name = "测试"

[[behavior_rules]]
keywords = ["加班", "工作"]
reaction = "先安慰"

[[behavior_rules]]
keywords = ["工作", "加班"]
reaction = "先安慰"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.rules.len(), 1);
    assert_eq!(
        count_skipped(&parsed, SkipCategory::BehaviorRules, SkipReason::Duplicate),
        1
    );
    assert_hint(&parsed, "rule_duplicate");
}

/// reaction 不同时不算重复。
#[test]
fn rules_with_different_reaction_are_kept() {
    let content = r#"[persona]
name = "测试"

[[behavior_rules]]
keywords = ["加班"]
reaction = "先安慰"

[[behavior_rules]]
keywords = ["加班"]
reaction = "转移话题"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.rules.len(), 2);
}

/// avoid 逐项 trim 剔空。
#[test]
fn rule_avoid_is_trimmed() {
    let content = "[persona]\nname = \"测试\"\n\n[[behavior_rules]]\nkeywords = [\"加班\"]\navoid = [\" 说教 \", \"\", \"比较\"]\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(
        parsed.rules[0].avoid,
        vec!["说教".to_string(), "比较".to_string()]
    );
}

// =========================================================
// prompt
// =========================================================

/// 空串后缀按未配置。
#[test]
fn blank_prompt_suffixes_are_none() {
    let content =
        "[persona]\nname = \"测试\"\n\n[prompt]\nrole_suffix = \"\"\nstyle_suffix = \"   \"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.prompt.role_suffix, None);
    assert_eq!(parsed.prompt.style_suffix, None);
}

/// 超 1000 字符后缀：截断并提示。
#[test]
fn overlong_suffix_is_truncated_with_hint() {
    let long_suffix = "补".repeat(1100);
    let content =
        format!("[persona]\nname = \"测试\"\n\n[prompt]\nrole_suffix = \"{long_suffix}\"\n");
    let parsed = parse_ok(&content, &ctx_default());
    let suffix = parsed.prompt.role_suffix.as_deref().expect("后缀应保留");
    assert_eq!(suffix.chars().count(), 1000);
    assert_hint(&parsed, "suffix_truncated");
}

/// [prompt] 节非表：提示并视为空。
#[test]
fn prompt_section_non_table_is_ignored() {
    let content = "prompt = \"不是表\"\n\n[persona]\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.prompt, PromptPatch::default());
    assert_hint(&parsed, "prompt_section_invalid");
}

// =========================================================
// hints 与 skipped 组合
// =========================================================

/// 同一跳过事件两处记录：被跳条目的 label 与提示码同时可见。
#[test]
fn skip_events_recorded_in_both_hints_and_skipped() {
    let content = r#"[persona]
name = "测试"

[traits]
base = [{ label = "慢热", meaning = "初识安静" }]
accent = [{ label = "慢热", meaning = "重复声明" }]

[[facts]]
field = "BasicInfo"
content = "内容"

[[facts]]
field = "BasicInfo"
content = "内容"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.skipped.len(), 2);
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Traits, SkipReason::LabelDuplicate),
        1
    );
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Facts, SkipReason::Duplicate),
        1
    );
    // hint 侧：两条跳过事件各有对应提示码
    assert_hint(&parsed, "trait_label_duplicate");
    assert_hint(&parsed, "fact_duplicate");
    // 提示消息不含条目内容全文（只含标识与计数）
    for hint in &parsed.hints {
        assert!(
            !hint.message.contains("重复声明"),
            "提示不应含条目原文: {}",
            hint.message
        );
    }
}
