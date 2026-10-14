//! crates/ramaria-memory/src/preset/tests/preview.rs - 预设 dry-run 预览用例
//!
//! 设计特点:
//! - 覆盖预览构建的 Create / Merge 两模式与跳过明细按类别分拣
//! - 覆盖文本渲染要素齐全与原文（blocks / description）不泄漏
//! - 覆盖 JSON 形态字段命名与跳过项 label 省略

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

/// 预览用例预设（含三类跳过明细、一条提示与原文标记）。
fn preview_preset() -> ParsedPreset {
    ParsedPreset {
        format: PresetFormat::Toml,
        persona: PersonaSection {
            uid: Some("char-0005".to_string()),
            kind: Some(PersonaKind::Char),
            kind_source: KindSource::Inferred,
            name: "黎杋枫".to_string(),
            avatar: Some("https://example.com/a.png".to_string()),
            description: Some("DESC_RAW_MARKER 背景描述".to_string()),
            source: "manual".to_string(),
        },
        identity: IdentitySection {
            assistant_name: "黎杋枫".to_string(),
            user_name: Some("用户".to_string()),
            work: None,
            era: None,
        },
        blocks: vec![
            (
                "A_persona".to_string(),
                "BLOCK_RAW_MARKER 人设文本".to_string(),
            ),
            (
                "E_rules".to_string(),
                "BLOCK_RAW_MARKER 规则文本".to_string(),
            ),
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
        rules: vec![RuleEntry {
            keywords: vec!["加班".to_string()],
            reaction: Some("别太累".to_string()),
            avoid: Vec::new(),
            params: BehaviorParams::default(),
        }],
        prompt: PromptPatch {
            role_suffix: Some("角色后缀".to_string()),
            style_suffix: None,
        },
        personality_raw: None,
        hints: vec![PresetHint {
            level: HintLevel::Info,
            code: "kind_rama_exists".to_string(),
            message: "库中已有活跃助手人格（rama-0001）".to_string(),
        }],
        skipped: vec![
            SkippedEntry {
                category: SkipCategory::Traits,
                label: Some("慢热".to_string()),
                reason: SkipReason::LabelDuplicate,
            },
            SkippedEntry {
                category: SkipCategory::Facts,
                label: None,
                reason: SkipReason::Duplicate,
            },
            SkippedEntry {
                category: SkipCategory::BehaviorRules,
                label: Some("加班".to_string()),
                reason: SkipReason::Duplicate,
            },
        ],
    }
}

// =========================================================
// build_preview
// =========================================================

/// Create 模式：注册信息、计数与跳过分拣逐项断言。
#[test]
fn build_preview_create_mode_collects_all_sections() {
    let parsed = preview_preset();
    let options = PreviewOptions {
        file_name: Some("lifanfeng.toml".to_string()),
        merge_target: None,
        decompose_enabled: true,
        privacy_confirmed: false,
    };
    let preview = build_preview(&parsed, &options);

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

    // 跳过明细按类别分拣
    assert_eq!(preview.will_write.traits.skipped.len(), 1);
    assert_eq!(
        preview.will_write.traits.skipped[0].label.as_deref(),
        Some("慢热")
    );
    assert_eq!(
        preview.will_write.traits.skipped[0].reason,
        SkipReason::LabelDuplicate
    );
    assert_eq!(preview.will_write.facts.skipped.len(), 1);
    assert_eq!(preview.will_write.facts.skipped[0].label, None);
    assert_eq!(
        preview.will_write.facts.skipped[0].reason,
        SkipReason::Duplicate
    );
    assert!(preview.will_write.examples.skipped.is_empty());
    assert_eq!(preview.will_write.behavior_rules.skipped.len(), 1);
    assert_eq!(
        preview.will_write.behavior_rules.skipped[0]
            .label
            .as_deref(),
        Some("加班")
    );

    // 拆解选项与提示透传
    assert!(preview.decompose.enabled);
    assert!(!preview.decompose.privacy_confirmed);
    assert_eq!(preview.hints.len(), 1);
    assert_eq!(preview.hints[0].code, "kind_rama_exists");
}

/// Merge 模式：merge_target 覆盖 uid 并切换模式。
#[test]
fn build_preview_merge_mode_overrides_uid() {
    let parsed = preview_preset();
    let options = PreviewOptions {
        merge_target: Some("char-0009".to_string()),
        ..Default::default()
    };
    let preview = build_preview(&parsed, &options);

    assert_eq!(preview.registration.uid.as_deref(), Some("char-0009"));
    assert_eq!(preview.registration.mode, PreviewMode::Merge);
}

/// 无 avatar / description 时布尔为 false；默认选项透传。
#[test]
fn build_preview_reports_missing_avatar_and_description() {
    let mut parsed = preview_preset();
    parsed.persona.avatar = None;
    parsed.persona.description = None;

    let preview = build_preview(&parsed, &PreviewOptions::default());
    assert!(!preview.registration.avatar);
    assert!(!preview.registration.description);
    assert_eq!(preview.registration.uid.as_deref(), Some("char-0005"));
    assert_eq!(preview.registration.mode, PreviewMode::Create);
    assert!(preview.file.is_none());
    assert!(!preview.decompose.enabled);
    assert!(!preview.decompose.privacy_confirmed);
}

// =========================================================
// render_text
// =========================================================

/// 文本渲染：全要素齐备且不含 blocks / description 原文。
#[test]
fn render_text_contains_all_elements_without_raw_text() {
    let parsed = preview_preset();
    let options = PreviewOptions {
        file_name: Some("lifanfeng.toml".to_string()),
        merge_target: None,
        decompose_enabled: true,
        privacy_confirmed: false,
    };
    let preview = build_preview(&parsed, &options);
    let text = preview.render_text();

    // 文件名与格式行
    assert!(text.contains("预设预览：lifanfeng.toml（格式：toml）"));
    // 注册行
    assert!(text.contains("注册：uid char-0005（新建）"));
    assert!(text.contains("kind char（由 uid 推断）"));
    assert!(text.contains("name「黎杋枫」"));
    assert!(text.contains("source manual"));
    assert!(text.contains("description 有"));
    assert!(text.contains("avatar 有"));
    // 人格行
    assert!(text.contains("人格：char-0005（新建）"));
    // 将写入各段计数与跳过汇总
    assert!(text.contains("将写入："));
    assert!(text.contains("base 1 / primary 1 / accent 0"));
    assert!(text.contains("跳过 1：label 重复「慢热」"));
    assert!(text.contains("1 条（跳过 1：重复）"));
    assert!(text.contains("1 对（跳过 0）"));
    assert!(text.contains("behavior_rules"));
    assert!(text.contains("1 条（跳过 1：重复「加班」）"));
    // prompt 补丁行
    assert!(text.contains("prompt 补丁"));
    assert!(text.contains("role_suffix 有 / style_suffix 无"));
    // blocks 与 identity 行
    assert!(text.contains("2 段 → personas.config"));
    assert!(text.contains("assistant_name「黎杋枫」/ user_name「用户」"));
    // 拆解行
    assert!(text.contains("可选拆解："));
    assert!(text.contains("隐私未确认"));
    // 提示节
    assert!(text.contains("提示："));
    assert!(text.contains("库中已有活跃助手人格（rama-0001）"));

    // 不泄漏 blocks / description 原文
    assert!(!text.contains("BLOCK_RAW_MARKER"));
    assert!(!text.contains("DESC_RAW_MARKER"));
}

/// 拆解三态文本：未启用 / 已确认将调用 / 未确认跳过。
#[test]
fn render_text_covers_decompose_states() {
    let parsed = preview_preset();

    let confirmed = build_preview(
        &parsed,
        &PreviewOptions {
            decompose_enabled: true,
            privacy_confirmed: true,
            ..Default::default()
        },
    );
    assert!(
        confirmed
            .render_text()
            .contains("将调用 LLM 拆解 blocks → traits/facts")
    );

    let disabled = build_preview(
        &parsed,
        &PreviewOptions {
            decompose_enabled: false,
            privacy_confirmed: true,
            ..Default::default()
        },
    );
    assert!(disabled.render_text().contains("未启用（--no-ai）"));
}

/// Merge 模式文本：人格行显示合并目标。
#[test]
fn render_text_merge_mode_shows_target() {
    let parsed = preview_preset();
    let preview = build_preview(
        &parsed,
        &PreviewOptions {
            merge_target: Some("char-0009".to_string()),
            ..Default::default()
        },
    );
    let text = preview.render_text();
    assert!(text.contains("注册：uid char-0009（合并到 char-0009）"));
    assert!(text.contains("人格：char-0009（合并到 char-0009）"));
}

// =========================================================
// JSON 形态
// =========================================================

/// JSON 键路径与取值断言（字段命名 snake_case）。
#[test]
fn preview_json_shape_matches_contract() {
    let parsed = preview_preset();
    let options = PreviewOptions {
        file_name: Some("lifanfeng.toml".to_string()),
        merge_target: None,
        decompose_enabled: true,
        privacy_confirmed: true,
    };
    let preview = build_preview(&parsed, &options);
    let value = serde_json::to_value(&preview).expect("预览应可序列化为 JSON");

    assert_eq!(value["file"], "lifanfeng.toml");
    assert_eq!(value["format"], "toml");
    assert_eq!(value["registration"]["uid"], "char-0005");
    assert_eq!(value["registration"]["mode"], "create");
    assert_eq!(value["registration"]["kind"], "char");
    assert_eq!(value["registration"]["kind_source"], "inferred");
    assert_eq!(value["registration"]["name"], "黎杋枫");
    assert_eq!(value["registration"]["source"], "manual");
    assert_eq!(value["registration"]["avatar"], true);
    assert_eq!(value["registration"]["description"], true);
    assert_eq!(value["will_write"]["traits"]["base"], 1);
    assert_eq!(value["will_write"]["traits"]["primary"], 1);
    assert_eq!(value["will_write"]["traits"]["accent"], 0);
    assert_eq!(value["will_write"]["traits"]["skipped"][0]["label"], "慢热");
    assert_eq!(
        value["will_write"]["traits"]["skipped"][0]["reason"],
        "label_duplicate"
    );
    assert_eq!(value["will_write"]["behavior_rules"]["count"], 1);
    assert_eq!(value["will_write"]["prompt"]["role_suffix"], true);
    assert_eq!(value["will_write"]["prompt"]["style_suffix"], false);
    assert_eq!(value["will_write"]["blocks"]["segments"], 2);
    assert_eq!(value["will_write"]["identity"]["assistant_name"], "黎杋枫");
    assert_eq!(value["will_write"]["identity"]["user_name"], "用户");
    assert_eq!(value["decompose"]["enabled"], true);
    assert_eq!(value["decompose"]["privacy_confirmed"], true);
    assert_eq!(value["hints"][0]["level"], "info");
    assert_eq!(value["hints"][0]["code"], "kind_rama_exists");

    // label 为 None 时不出现（skip_serializing_if）
    let facts_skip = &value["will_write"]["facts"]["skipped"][0];
    assert!(facts_skip.get("label").is_none());
    assert_eq!(facts_skip["reason"], "duplicate");
}
