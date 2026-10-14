//! crates/ramaria-memory/src/preset/tests/identity.rs - 注册段（uid / kind / 名称 / 描述）用例
//!
//! 设计特点:
//! - 覆盖 uid 格式边界与非法形态（文件级失败）、kind 值与大小写、uid×kind 一致性
//! - 覆盖名称兜底链与截断、描述优先级与遗留键提示

use super::*;

// =========================================================
// uid 格式
// =========================================================

/// uid 合法边界：2 字母 + 1 位数字、8 字母 + 4 位数字。
#[test]
fn uid_valid_boundaries_accepted() {
    for uid in ["aa-1", "abcdefgh-9999"] {
        let content = format!("[persona]\nuid = \"{uid}\"\nname = \"测试\"\n");
        let parsed = parse_ok(&content, &ctx_default());
        assert_eq!(parsed.persona.uid.as_deref(), Some(uid), "uid={uid}");
    }
}

/// uid 非法形态（前缀/序号越界、缺短横、大写、段为空）均为文件级失败。
#[test]
fn uid_invalid_shapes_are_file_level_errors() {
    let cases = [
        "a-1",         // 前缀 1 字母
        "abcdefghi-1", // 前缀 9 字母
        "ab-12345",    // 序号 5 位
        "abc123",      // 无短横
        "AB-1",        // 大写
        "ab-",         // 序号段为空
        "-0001",       // 前缀段为空
        "ab 1",        // 空白替代短横
    ];
    for uid in cases {
        let content = format!("[persona]\nuid = \"{uid}\"\nname = \"测试\"\n");
        let err = parse_toml(&content, &ctx_default()).unwrap_err();
        let message = format!("{err}");
        assert!(
            message.contains("uid 格式非法"),
            "uid={uid} 应文件级失败，消息: {message}"
        );
    }
}

/// 显式 uid 空白串按未提供处理，回退文件名 stem。
#[test]
fn blank_explicit_uid_falls_back_to_stem() {
    let content = "[persona]\nuid = \"   \"\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_with_stem("char-0002"));
    assert_eq!(parsed.persona.uid.as_deref(), Some("char-0002"));
}

/// 显式 uid 与文件名 stem 不一致：提示并以显式 uid 为准。
#[test]
fn explicit_uid_overrides_different_stem_with_hint() {
    let content = "[persona]\nuid = \"char-0001\"\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_with_stem("char-0099"));
    assert_eq!(parsed.persona.uid.as_deref(), Some("char-0001"));
    assert_hint(&parsed, "uid_stem_mismatch");
}

/// 显式 uid 与文件名 stem 一致：无提示。
#[test]
fn matching_explicit_uid_and_stem_have_no_hint() {
    let content = "[persona]\nuid = \"char-0001\"\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_with_stem("char-0001"));
    assert!(parsed.hints.is_empty());
}

// =========================================================
// kind
// =========================================================

/// kind 六值全部接受（大小写不敏感），与各自前缀一致的 uid 组合。
#[test]
fn kind_six_values_case_insensitive() {
    let cases = [
        ("user", PersonaKind::User),
        ("RAMA", PersonaKind::Rama),
        ("Char", PersonaKind::Char),
        ("aNiM", PersonaKind::Anim),
        ("OC", PersonaKind::Oc),
        ("hist", PersonaKind::Hist),
    ];
    for (raw, expected) in cases {
        let content =
            format!("[persona]\nuid = \"char-0001\"\nkind = \"{raw}\"\nname = \"测试\"\n");
        // char 之外的 kind 需换成与 kind 前缀一致的 uid 才合法
        let content = if matches!(expected, PersonaKind::Char) {
            content
        } else {
            let prefix = expected.as_str();
            content.replace("char-0001", &format!("{prefix}-0001"))
        };
        let parsed = parse_ok(&content, &ctx_default());
        assert_eq!(parsed.persona.kind, Some(expected), "kind={raw}");
        assert_eq!(parsed.persona.kind_source, KindSource::Explicit);
    }
}

/// kind 非法值：文件级失败，消息列出合法值。
#[test]
fn kind_invalid_value_is_file_level_error() {
    let content = "[persona]\nkind = \"robot\"\nname = \"测试\"\n";
    let err = parse_toml(content, &ctx_default()).unwrap_err();
    let message = format!("{err}");
    assert!(message.contains("kind 非法"), "消息: {message}");
    assert!(message.contains("char"), "消息应列合法值: {message}");
}

/// uid 与 kind 同时可得时一致性校验：矛盾组合拒绝，未知前缀须为 char。
#[test]
fn uid_kind_consistency_enforced() {
    // char-0001 + rama → 拒绝
    let content = "[persona]\nuid = \"char-0001\"\nkind = \"rama\"\nname = \"测试\"\n";
    let err = parse_toml(content, &ctx_default()).unwrap_err();
    assert!(format!("{err}").contains("不一致"));

    // anim-0002 + Anim → 接受
    let content = "[persona]\nuid = \"anim-0002\"\nkind = \"Anim\"\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.kind, Some(PersonaKind::Anim));

    // xx-0001 + char → 接受（未知前缀保守回退 char）
    let content = "[persona]\nuid = \"xx-0001\"\nkind = \"char\"\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.kind, Some(PersonaKind::Char));

    // xx-0001 + anim → 拒绝
    let content = "[persona]\nuid = \"xx-0001\"\nkind = \"anim\"\nname = \"测试\"\n";
    let err = parse_toml(content, &ctx_default()).unwrap_err();
    assert!(format!("{err}").contains("不一致"));
}

/// 无 uid 且无显式 kind：kind 待定（Default），不推断。
#[test]
fn kind_defaults_when_no_uid_and_no_explicit() {
    let content = "[persona]\nname = \"测试\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.uid, None);
    assert_eq!(parsed.persona.kind, None);
    assert_eq!(parsed.persona.kind_source, KindSource::Default);
}

// =========================================================
// 名称兜底链
// =========================================================

/// [persona].name 显式优先；assistant_name 反向取用 name。
#[test]
fn persona_name_is_preferred_for_both_fields() {
    let content = r#"[persona]
name = "注册名"

[identity]
assistant_name = "自称名"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.name, "注册名");
    assert_eq!(parsed.identity.assistant_name, "自称名");
}

/// 仅 [persona].name：assistant_name 回退 name。
#[test]
fn assistant_name_falls_back_to_persona_name() {
    let content = "[persona]\nname = \"注册名\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.name, "注册名");
    assert_eq!(parsed.identity.assistant_name, "注册名");
}

/// 仅 [identity].assistant_name：name 回退 assistant_name。
#[test]
fn persona_name_falls_back_to_assistant_name() {
    let content = "[identity]\nassistant_name = \"自称名\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.name, "自称名");
    assert_eq!(parsed.identity.assistant_name, "自称名");
}

/// 双缺 + 文件 stem：name 与 assistant_name 均为 stem。
#[test]
fn both_name_fields_missing_use_stem() {
    let content = "[blocks]\nA_persona = \"人设\"\n";
    let parsed = parse_ok(content, &ctx_with_stem("char-0100"));
    assert_eq!(parsed.persona.name, "char-0100");
    assert_eq!(parsed.identity.assistant_name, "char-0100");
}

/// name 超 64 字符：截断并提示。
#[test]
fn overlong_name_is_truncated_with_hint() {
    let long_name = "名".repeat(70);
    let content = format!("[persona]\nname = \"{long_name}\"\n");
    let parsed = parse_ok(&content, &ctx_default());
    assert_eq!(parsed.persona.name.chars().count(), 64);
    assert_eq!(parsed.identity.assistant_name.chars().count(), 64);
    assert_hint(&parsed, "name_truncated");
}

// =========================================================
// 描述
// =========================================================

/// [persona].description 优先；遗留键不同值时 warn 提示被覆盖。
#[test]
fn persona_description_wins_over_legacy_with_warn() {
    let content = r#"[persona]
name = "测试"
description = "新描述"

[identity]
description = "旧描述"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.description.as_deref(), Some("新描述"));
    let hint = parsed
        .hints
        .iter()
        .find(|h| h.code == "legacy_description_key")
        .expect("应提示遗留键被覆盖");
    assert_eq!(hint.level, HintLevel::Warn);
}

/// 两处描述相同：以 [persona] 为准且不提示。
#[test]
fn identical_descriptions_produce_no_hint() {
    let content = r#"[persona]
name = "测试"
description = "同一描述"

[identity]
description = "同一描述"
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.description.as_deref(), Some("同一描述"));
    assert!(
        !parsed
            .hints
            .iter()
            .any(|h| h.code == "legacy_description_key")
    );
}

/// 描述超 2000 字符：截断并提示。
#[test]
fn overlong_description_is_truncated_with_hint() {
    let long_desc = "描".repeat(2100);
    let content = format!("[persona]\nname = \"测试\"\ndescription = \"{long_desc}\"\n");
    let parsed = parse_ok(&content, &ctx_default());
    let desc = parsed.persona.description.as_deref().expect("描述应保留");
    assert_eq!(desc.chars().count(), 2000);
    assert_hint(&parsed, "description_truncated");
}

/// source 显式值优先于上下文缺省值。
#[test]
fn explicit_source_overrides_context_default() {
    let content = "[persona]\nname = \"测试\"\nsource = \"file\"\n";
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.persona.source, "file");
}

/// [identity] 可选字段空串按未设置（None）。
#[test]
fn blank_optional_identity_fields_are_none() {
    let content = r#"[identity]
assistant_name = "测试"
user_name = ""
work = "   "
era = ""
"#;
    let parsed = parse_ok(content, &ctx_default());
    assert_eq!(parsed.identity.user_name, None);
    assert_eq!(parsed.identity.work, None);
    assert_eq!(parsed.identity.era, None);
}
