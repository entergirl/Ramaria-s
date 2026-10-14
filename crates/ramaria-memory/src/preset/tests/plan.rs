//! crates/ramaria-memory/src/preset/tests/plan.rs - 预设导入计划用例
//!
//! 设计特点:
//! - 覆盖注册决议优先级与 uid 自动生成
//! - 覆盖 Create 全写与 Merge 只增不改（列补空缺 / 四类条目判重 / 配置与补丁跳过）
//! - 覆盖拆解条目追加次序与库侧提示分支
//! - 覆盖预览桥接的计数、跳过明细顺序与计划一致性

use super::*;
use ramaria_core::behavior::BehaviorParams;

// =========================================================
// 构造辅助
// =========================================================

/// 构造单条 trait。
fn trait_entry(label: &str, layer: TraitLayer) -> TraitEntry {
    TraitEntry {
        layer,
        label: label.to_string(),
        meaning: format!("{label}的含义"),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        motives: Vec::new(),
    }
}

/// 构造单条 rule。
fn rule_entry(keyword: &str, reaction: &str) -> RuleEntry {
    RuleEntry {
        keywords: vec![keyword.to_string()],
        reaction: Some(reaction.to_string()),
        avoid: Vec::new(),
        params: BehaviorParams::default(),
    }
}

/// 基础预设（Create 场景全写）。
fn parsed_preset() -> ParsedPreset {
    ParsedPreset {
        format: PresetFormat::Toml,
        persona: PersonaSection {
            uid: Some("char-0005".to_string()),
            kind: Some(PersonaKind::Char),
            kind_source: KindSource::Inferred,
            name: "黎杋枫".to_string(),
            avatar: Some("https://example.com/a.png".to_string()),
            description: Some("背景描述".to_string()),
            source: "manual".to_string(),
        },
        identity: IdentitySection {
            assistant_name: "黎杋枫".to_string(),
            user_name: Some("用户".to_string()),
            work: None,
            era: None,
        },
        blocks: vec![
            ("A_persona".to_string(), "人设文本".to_string()),
            ("E_rules".to_string(), "规则文本".to_string()),
        ],
        traits: TraitsSection {
            base: vec![trait_entry("温柔", TraitLayer::Base)],
            primary: vec![trait_entry("慢热", TraitLayer::Primary)],
            accent: Vec::new(),
        },
        facts: vec![FactEntry {
            field: ProfileField::BasicInfo,
            content: "医学院学生".to_string(),
        }],
        examples: vec![ExampleEntry {
            user: "你好".to_string(),
            assistant: "你好呀".to_string(),
        }],
        rules: vec![rule_entry("加班", "别太累")],
        prompt: PromptPatch {
            role_suffix: Some("角色后缀".to_string()),
            style_suffix: None,
        },
        personality_raw: None,
        hints: Vec::new(),
        skipped: Vec::new(),
    }
}

/// 库侧现状快照（含既有 label / fact / example / rule，各一条）。
fn existing_snapshot() -> ExistingProfile {
    ExistingProfile {
        exists: true,
        kind: Some(PersonaKind::Char),
        name: Some("黎杋枫".to_string()),
        avatar: None,
        description: None,
        has_config: false,
        trait_labels: vec!["温柔".to_string()],
        trait_max_seq: [1, 0, 0],
        fact_pairs: vec![(ProfileField::BasicInfo, "医学院学生".to_string())],
        example_pairs: vec![("你好".to_string(), "你好呀".to_string())],
        rule_keys: vec![(vec!["加班".to_string()], Some("别太累".to_string()))],
        all_uids: vec!["char-0005".to_string()],
    }
}

/// 计划中是否含指定提示码。
fn has_hint(plan: &ImportPlan, code: &str) -> bool {
    plan.hints.iter().any(|hint| hint.code == code)
}

// =========================================================
// resolve_registration
// =========================================================

/// 显式 uid 优先于文件名，merge_target 优先于 uid。
#[test]
fn resolve_registration_prefers_explicit_uid_then_merge_target() {
    let parsed = parsed_preset();
    let resolved = resolve_registration(&parsed, None, &[]);
    assert_eq!(resolved.uid, "char-0005");
    assert_eq!(resolved.kind, PersonaKind::Char);
    assert_eq!(resolved.kind_source, KindSource::Inferred);

    let resolved = resolve_registration(&parsed, Some("char-0009"), &[]);
    assert_eq!(resolved.uid, "char-0009");
    assert_eq!(resolved.kind, PersonaKind::Char);
}

/// 无 uid 时按 kind 生成 `{kind}-{序号:04}`，kind 缺省 Char。
#[test]
fn resolve_registration_generates_uid_by_kind() {
    let mut parsed = parsed_preset();
    parsed.persona.uid = None;
    parsed.persona.kind = None;
    parsed.persona.kind_source = KindSource::Default;
    let all_uids = vec!["char-0003".to_string(), "rama-0001".to_string()];

    let resolved = resolve_registration(&parsed, None, &all_uids);
    assert_eq!(resolved.uid, "char-0004");
    assert_eq!(resolved.kind, PersonaKind::Char);
    assert_eq!(resolved.kind_source, KindSource::Default);

    let mut parsed = parsed_preset();
    parsed.persona.uid = None;
    parsed.persona.kind = Some(PersonaKind::Rama);
    parsed.persona.kind_source = KindSource::Explicit;
    let resolved = resolve_registration(&parsed, None, &all_uids);
    assert_eq!(resolved.uid, "rama-0002");
    assert_eq!(resolved.kind, PersonaKind::Rama);
}

// =========================================================
// Create / Merge 注册段
// =========================================================

/// Create：注册列全写、四类条目全写、config 与补丁写入。
#[test]
fn plan_create_writes_all_sections() {
    let parsed = parsed_preset();
    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(
        &parsed,
        &resolved,
        &ExistingProfile::default(),
        &PlanOptions::default(),
    );

    assert_eq!(plan.registration.mode, PreviewMode::Create);
    assert!(!plan.registration.uid_exists);
    assert_eq!(plan.registration.kind, PersonaKind::Char);
    assert!(plan.registration.write_name);
    assert!(plan.registration.write_avatar);
    assert!(plan.registration.write_description);
    assert!(plan.registration.write_config);
    assert_eq!(
        plan.registration.avatar.as_deref(),
        Some("https://example.com/a.png")
    );

    assert_eq!(plan.traits.len(), 2);
    assert!(
        plan.traits
            .iter()
            .all(|item| item.decision == EntryDecision::Write)
    );
    assert_eq!(plan.facts.len(), 1);
    assert_eq!(plan.facts[0].decision, EntryDecision::Write);
    assert_eq!(plan.examples.len(), 1);
    assert_eq!(plan.examples[0].decision, EntryDecision::Write);
    assert_eq!(plan.rules.len(), 1);
    assert_eq!(plan.rules[0].decision, EntryDecision::Write);

    assert!(plan.prompt.write_role_suffix);
    assert!(!plan.prompt.write_style_suffix);
    assert_eq!(plan.blocks_segments, 2);
    assert_eq!(plan.identity.assistant_name, "黎杋枫");
}

/// Merge 判重矩阵：既有条目跳过、新条目写入。
#[test]
fn plan_merge_skips_duplicates_and_keeps_new_entries() {
    let mut parsed = parsed_preset();
    parsed
        .traits
        .base
        .push(trait_entry("从容", TraitLayer::Base));
    parsed.facts.push(FactEntry {
        field: ProfileField::Interests,
        content: "书法".to_string(),
    });
    parsed.examples.push(ExampleEntry {
        user: "早".to_string(),
        assistant: "早呀".to_string(),
    });
    parsed.rules.push(rule_entry("生日", "记得"));

    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(
        &parsed,
        &resolved,
        &existing_snapshot(),
        &PlanOptions::default(),
    );

    assert_eq!(plan.registration.mode, PreviewMode::Merge);
    assert!(plan.registration.uid_exists);

    // traits：温柔 重复、慢热 与 从容 保留
    assert_eq!(
        plan.traits[0].decision,
        EntryDecision::Skip(SkipReason::LabelDuplicate)
    );
    assert_eq!(plan.traits[0].entry.label, "温柔");
    assert_eq!(plan.traits[1].decision, EntryDecision::Write);
    assert_eq!(plan.traits[2].decision, EntryDecision::Write);

    // facts：BasicInfo 重复、Interests 保留
    assert_eq!(
        plan.facts[0].decision,
        EntryDecision::Skip(SkipReason::Duplicate)
    );
    assert_eq!(plan.facts[1].decision, EntryDecision::Write);

    // examples：同对重复、新对保留
    assert_eq!(
        plan.examples[0].decision,
        EntryDecision::Skip(SkipReason::Duplicate)
    );
    assert_eq!(plan.examples[1].decision, EntryDecision::Write);

    // rules：同 keywords + reaction 重复、新规则保留
    assert_eq!(
        plan.rules[0].decision,
        EntryDecision::Skip(SkipReason::Duplicate)
    );
    assert_eq!(plan.rules[1].decision, EntryDecision::Write);

    // Merge 且既有字段空缺时补空缺
    assert!(plan.registration.write_avatar);
    assert!(plan.registration.write_description);
    assert!(plan.registration.write_config);
}

/// 判重归一：fact 内容去空白、rule keywords 排序去重。
#[test]
fn plan_merge_dedup_normalizes_content() {
    let mut parsed = parsed_preset();
    parsed.facts[0].content = "  医学院学生  ".to_string();
    parsed.rules[0] = RuleEntry {
        keywords: vec!["加班".to_string(), "工作".to_string(), "加班".to_string()],
        reaction: Some("  别太累 ".to_string()),
        avoid: Vec::new(),
        params: BehaviorParams::default(),
    };

    let mut existing = existing_snapshot();
    existing.rule_keys = vec![(
        vec!["工作".to_string(), "加班".to_string()],
        Some("别太累".to_string()),
    )];

    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(&parsed, &resolved, &existing, &PlanOptions::default());

    assert_eq!(
        plan.facts[0].decision,
        EntryDecision::Skip(SkipReason::Duplicate)
    );
    assert_eq!(
        plan.rules[0].decision,
        EntryDecision::Skip(SkipReason::Duplicate)
    );
}

/// Merge 列只补空缺：已有值不覆盖，空串 / 空白视同空缺。
#[test]
fn plan_merge_fills_missing_columns_only() {
    let parsed = parsed_preset();
    let resolved = resolve_registration(&parsed, None, &[]);
    let mut existing = existing_snapshot();
    existing.name = Some("".to_string());
    existing.avatar = Some("https://example.com/existing.png".to_string());
    existing.description = Some("   ".to_string());

    let plan = plan_import(&parsed, &resolved, &existing, &PlanOptions::default());
    assert!(plan.registration.write_name);
    assert!(!plan.registration.write_avatar);
    assert!(plan.registration.write_description);
}

/// Merge 且目标已有配置：config 与提示词补丁一并跳过。
#[test]
fn plan_merge_skips_config_and_prompt_patch() {
    let parsed = parsed_preset();
    let resolved = resolve_registration(&parsed, None, &[]);
    let mut existing = existing_snapshot();
    existing.has_config = true;

    let plan = plan_import(&parsed, &resolved, &existing, &PlanOptions::default());
    assert!(!plan.registration.write_config);
    assert!(!plan.prompt.write_role_suffix);
    assert!(!plan.prompt.write_style_suffix);
    assert!(has_hint(&plan, "config_skipped"));
    assert!(has_hint(&plan, "prompt_skipped"));
}

// =========================================================
// 拆解条目
// =========================================================

/// 拆解条目追加在直写之后，同 label / 同内容跳过。
#[test]
fn plan_appends_decomposed_entries_after_direct() {
    let parsed = parsed_preset();
    let resolved = resolve_registration(&parsed, None, &[]);
    let options = PlanOptions {
        merge_target: None,
        decomposed: DecomposedEntries {
            traits: vec![
                trait_entry("温柔", TraitLayer::Primary),
                trait_entry("活泼", TraitLayer::Accent),
            ],
            facts: vec![
                FactEntry {
                    field: ProfileField::BasicInfo,
                    content: "医学院学生".to_string(),
                },
                FactEntry {
                    field: ProfileField::History,
                    content: "童年海边".to_string(),
                },
            ],
        },
    };

    let plan = plan_import(&parsed, &resolved, &ExistingProfile::default(), &options);

    // 顺序：直写 base / primary、拆解 base / primary / accent
    let labels: Vec<&str> = plan
        .traits
        .iter()
        .map(|item| item.entry.label.as_str())
        .collect();
    assert_eq!(labels, vec!["温柔", "慢热", "温柔", "活泼"]);
    assert_eq!(
        plan.traits[2].decision,
        EntryDecision::Skip(SkipReason::LabelDuplicate)
    );
    assert_eq!(plan.traits[3].decision, EntryDecision::Write);

    assert_eq!(plan.facts.len(), 3);
    assert_eq!(
        plan.facts[1].decision,
        EntryDecision::Skip(SkipReason::Duplicate)
    );
    assert_eq!(plan.facts[2].decision, EntryDecision::Write);
}

// =========================================================
// 库侧提示
// =========================================================

/// kind 语义提示：rama 库内已有助手人格 / user 提示。
#[test]
fn plan_hints_cover_kind_semantics() {
    let mut parsed = parsed_preset();
    parsed.persona.uid = Some("rama-0002".to_string());
    parsed.persona.kind = Some(PersonaKind::Rama);
    parsed.persona.kind_source = KindSource::Explicit;
    let resolved = resolve_registration(&parsed, None, &[]);
    let mut existing = ExistingProfile {
        all_uids: vec!["rama-0001".to_string()],
        ..Default::default()
    };
    existing.all_uids.push("char-0005".to_string());
    let plan = plan_import(&parsed, &resolved, &existing, &PlanOptions::default());
    assert!(has_hint(&plan, "kind_rama_exists"));

    let mut parsed = parsed_preset();
    parsed.persona.uid = Some("user-0002".to_string());
    parsed.persona.kind = Some(PersonaKind::User);
    parsed.persona.kind_source = KindSource::Explicit;
    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(
        &parsed,
        &resolved,
        &ExistingProfile::default(),
        &PlanOptions::default(),
    );
    assert!(has_hint(&plan, "kind_user_note"));
}

/// 合并与目标提示：merge_mode / kind_immutable / merge_target_missing / into_overrides_uid。
#[test]
fn plan_hints_cover_merge_and_target_paths() {
    let parsed = parsed_preset();
    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(
        &parsed,
        &resolved,
        &existing_snapshot(),
        &PlanOptions::default(),
    );
    assert!(has_hint(&plan, "merge_mode"));
    assert!(has_hint(&plan, "kind_immutable"));

    let options = PlanOptions {
        merge_target: Some("char-9999".to_string()),
        ..Default::default()
    };
    let resolved = resolve_registration(&parsed, Some("char-9999"), &[]);
    let plan = plan_import(&parsed, &resolved, &ExistingProfile::default(), &options);
    assert_eq!(plan.registration.uid, "char-9999");
    assert_eq!(plan.registration.mode, PreviewMode::Create);
    assert!(has_hint(&plan, "merge_target_missing"));
    assert!(has_hint(&plan, "into_overrides_uid"));

    // 提示消息不含描述原文
    assert!(
        !plan
            .hints
            .iter()
            .any(|hint| hint.message.contains("背景描述"))
    );
}

// =========================================================
// 预览桥接
// =========================================================

/// 预览各字段与计划一致；跳过明细顺序 = 解析期在前、裁决期在后。
#[test]
fn preview_from_plan_matches_plan() {
    let mut parsed = parsed_preset();
    parsed.skipped = vec![SkippedEntry {
        category: SkipCategory::Traits,
        label: Some("解析期跳过".to_string()),
        reason: SkipReason::Overflow,
    }];
    parsed.hints = vec![PresetHint {
        level: HintLevel::Warn,
        code: "parse_warn".to_string(),
        message: "解析期提示".to_string(),
    }];
    parsed
        .traits
        .base
        .push(trait_entry("温柔", TraitLayer::Accent));

    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(
        &parsed,
        &resolved,
        &ExistingProfile::default(),
        &PlanOptions::default(),
    );

    let preview = build_preview_from_plan(
        &plan,
        &PreviewOptions {
            file_name: Some("lifanfeng.toml".to_string()),
            merge_target: None,
            decompose_enabled: false,
            privacy_confirmed: false,
        },
    );

    assert_eq!(preview.file.as_deref(), Some("lifanfeng.toml"));
    assert_eq!(preview.format, PresetFormat::Toml);
    assert_eq!(preview.registration.uid.as_deref(), Some("char-0005"));
    assert_eq!(preview.registration.mode, PreviewMode::Create);
    assert_eq!(preview.registration.kind, Some(PersonaKind::Char));
    assert_eq!(preview.registration.kind_source, KindSource::Inferred);
    assert_eq!(preview.registration.name, "黎杋枫");
    assert_eq!(preview.registration.source, "manual");
    assert!(preview.registration.avatar);
    assert!(preview.registration.description);

    assert_eq!(preview.will_write.traits.base, 1);
    assert_eq!(preview.will_write.traits.primary, 1);
    assert_eq!(preview.will_write.traits.accent, 0);
    assert_eq!(preview.will_write.traits.skipped.len(), 2);
    assert_eq!(
        preview.will_write.traits.skipped[0].label.as_deref(),
        Some("解析期跳过")
    );
    assert_eq!(
        preview.will_write.traits.skipped[0].reason,
        SkipReason::Overflow
    );
    assert_eq!(
        preview.will_write.traits.skipped[1].label.as_deref(),
        Some("温柔")
    );
    assert_eq!(
        preview.will_write.traits.skipped[1].reason,
        SkipReason::LabelDuplicate
    );

    assert_eq!(preview.will_write.facts.count, 1);
    assert_eq!(preview.will_write.examples.count, 1);
    assert_eq!(preview.will_write.behavior_rules.count, 1);
    assert!(preview.will_write.prompt.role_suffix);
    assert!(!preview.will_write.prompt.style_suffix);
    assert_eq!(preview.will_write.blocks.segments, 2);
    assert_eq!(preview.will_write.identity.assistant_name, "黎杋枫");
    assert_eq!(
        preview.will_write.identity.user_name.as_deref(),
        Some("用户")
    );

    assert!(!preview.decompose.enabled);
    assert!(!preview.decompose.privacy_confirmed);
    assert_eq!(preview.hints.len(), 1);
    assert_eq!(preview.hints[0].code, "parse_warn");
}

/// 规则跳过项的预览标识取首个 keyword。
#[test]
fn preview_from_plan_reports_rule_skip_label() {
    let mut parsed = parsed_preset();
    parsed.rules.push(rule_entry("加班", "别太累"));

    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(
        &parsed,
        &resolved,
        &ExistingProfile::default(),
        &PlanOptions::default(),
    );
    let preview = build_preview_from_plan(&plan, &PreviewOptions::default());

    assert_eq!(preview.will_write.behavior_rules.count, 1);
    assert_eq!(preview.will_write.behavior_rules.skipped.len(), 1);
    assert_eq!(
        preview.will_write.behavior_rules.skipped[0]
            .label
            .as_deref(),
        Some("加班")
    );
    assert_eq!(
        preview.will_write.behavior_rules.skipped[0].reason,
        SkipReason::Duplicate
    );
}

/// 一致性冒烟：预览计数与计划写入数逐项相等。
#[test]
fn preview_counts_consistent_with_plan_writes() {
    let parsed = parsed_preset();
    let resolved = resolve_registration(&parsed, None, &[]);
    let plan = plan_import(
        &parsed,
        &resolved,
        &ExistingProfile::default(),
        &PlanOptions::default(),
    );
    let preview = build_preview_from_plan(&plan, &PreviewOptions::default());

    let traits_writes = plan
        .traits
        .iter()
        .filter(|item| item.decision == EntryDecision::Write)
        .count();
    assert_eq!(
        preview.will_write.traits.base
            + preview.will_write.traits.primary
            + preview.will_write.traits.accent,
        traits_writes
    );
    assert_eq!(
        preview.will_write.facts.count,
        plan.facts
            .iter()
            .filter(|item| item.decision == EntryDecision::Write)
            .count()
    );
    assert_eq!(
        preview.will_write.examples.count,
        plan.examples
            .iter()
            .filter(|item| item.decision == EntryDecision::Write)
            .count()
    );
    assert_eq!(
        preview.will_write.behavior_rules.count,
        plan.rules
            .iter()
            .filter(|item| item.decision == EntryDecision::Write)
            .count()
    );
}
