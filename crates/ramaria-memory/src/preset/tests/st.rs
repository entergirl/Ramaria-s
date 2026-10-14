//! crates/ramaria-memory/src/preset/tests/st.rs - ST 角色卡解析用例
//!
//! 设计特点:
//! - 覆盖 ST 卡 v2 / v1 字段映射、忽略字段与世界书提示、名称兜底与截断
//! - 覆盖 mes_example 切块配对规则（大小写、`<START>` 分隔、未配对丢弃、判重）
//! - 覆盖 PNG chara 全链路与字节级入口三分支判定

use super::*;

// =========================================================
// 辅助
// =========================================================

/// 按 ST JSON 解析并断言成功。
fn st_ok(content: &str, ctx: &PresetParseContext) -> ParsedPreset {
    parse_st_json(content, ctx).expect("ST 卡解析应当成功")
}

// =========================================================
// v2 / v1 字段映射
// =========================================================

/// ST 卡 v2：全字段映射、忽略字段不报错、世界书一条 info 提示。
#[test]
fn st_v2_full_card_maps_all_fields() {
    let content = r#"{
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "data": {
            "name": "黎杋枫",
            "description": "背景描述文本",
            "personality": "温柔、慢热",
            "scenario": "咖啡馆",
            "first_mes": "你好呀",
            "mes_example": "{{user}}: 你好\n{{char}}: 你好呀\n<START>\n{{user}}: 再见\n{{char}}: 再见啦",
            "system_prompt": "角色补丁",
            "post_history_instructions": "指令",
            "alternate_greetings": ["早上好"],
            "creator_notes": "备注",
            "tags": ["测试"],
            "creator": "someone",
            "character_version": "1.0",
            "extensions": { "talkativeness": "0.5" },
            "character_book": { "entries": [] }
        }
    }"#;

    let parsed = st_ok(content, &ctx_default());

    // 注册段与对话身份
    assert_eq!(parsed.format, PresetFormat::StJson);
    assert!(parsed.persona.uid.is_none());
    assert_eq!(parsed.persona.kind, Some(PersonaKind::Char));
    assert_eq!(parsed.persona.kind_source, KindSource::Default);
    assert_eq!(parsed.persona.name, "黎杋枫");
    assert!(parsed.persona.avatar.is_none());
    assert_eq!(parsed.persona.description.as_deref(), Some("背景描述文本"));
    assert_eq!(parsed.persona.source, "manual");
    assert_eq!(parsed.identity.assistant_name, "黎杋枫");
    assert!(parsed.identity.user_name.is_none());
    assert!(parsed.identity.work.is_none());
    assert!(parsed.identity.era.is_none());

    // description 双落点：列 + BasicInfo fact
    assert_eq!(parsed.facts.len(), 1);
    assert_eq!(parsed.facts[0].field, ProfileField::BasicInfo);
    assert_eq!(parsed.facts[0].content, "背景描述文本");

    // personality 原文保留、不产生 blocks / traits / rules
    assert_eq!(parsed.personality_raw.as_deref(), Some("温柔、慢热"));
    assert!(parsed.blocks.is_empty());
    assert!(parsed.traits.base.is_empty());
    assert!(parsed.traits.primary.is_empty());
    assert!(parsed.traits.accent.is_empty());
    assert!(parsed.rules.is_empty());

    // mes_example 两对
    assert_eq!(parsed.examples.len(), 2);
    assert_eq!(parsed.examples[0].user, "你好");
    assert_eq!(parsed.examples[0].assistant, "你好呀");
    assert_eq!(parsed.examples[1].user, "再见");
    assert_eq!(parsed.examples[1].assistant, "再见啦");

    // system_prompt → role_suffix
    assert_eq!(parsed.prompt.role_suffix.as_deref(), Some("角色补丁"));
    assert!(parsed.prompt.style_suffix.is_none());

    // 仅世界书一条 info 提示；忽略字段无提示无跳过明细
    assert_eq!(parsed.hints.len(), 1, "提示: {:?}", parsed.hints);
    let lorebook = &parsed.hints[0];
    assert_eq!(lorebook.code, "st_lorebook_ignored");
    assert_eq!(lorebook.level, HintLevel::Info);
    assert!(parsed.skipped.is_empty());
}

/// ST 卡 v1：字段在顶层，与 v2 同映射。
#[test]
fn st_v1_top_level_fields_map_same() {
    let content = r#"{
        "name": "旧版角色",
        "description": "顶层描述",
        "personality": "外向",
        "mes_example": "{{user}}: 嗨\n{{char}}: 嗨呀",
        "system_prompt": "旧版补丁"
    }"#;

    let parsed = st_ok(content, &ctx_with_stem("st-card-v1"));
    assert_eq!(parsed.format, PresetFormat::StJson);
    assert_eq!(parsed.persona.name, "旧版角色");
    assert_eq!(parsed.identity.assistant_name, "旧版角色");
    assert_eq!(parsed.persona.description.as_deref(), Some("顶层描述"));
    assert_eq!(parsed.facts.len(), 1);
    assert_eq!(parsed.personality_raw.as_deref(), Some("外向"));
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(parsed.prompt.role_suffix.as_deref(), Some("旧版补丁"));
    assert_eq!(parsed.persona.kind, Some(PersonaKind::Char));
    assert_eq!(parsed.persona.kind_source, KindSource::Default);
    assert!(parsed.hints.is_empty(), "提示: {:?}", parsed.hints);
}

// =========================================================
// 缺失与类型容错
// =========================================================

/// description 缺失：无列值也无 fact；personality 缺失为 None。
#[test]
fn missing_description_and_personality_yield_none() {
    let parsed = st_ok(r#"{"name": "测试"}"#, &ctx_default());
    assert!(parsed.persona.description.is_none());
    assert!(parsed.facts.is_empty());
    assert!(parsed.personality_raw.is_none());
    assert!(parsed.prompt.role_suffix.is_none());
}

/// description 空串或空白串：按未提供（无列值无 fact）。
#[test]
fn empty_description_is_treated_as_absent() {
    for content in [
        r#"{"name": "测试", "description": ""}"#,
        r#"{"name": "测试", "description": "   "}"#,
    ] {
        let parsed = st_ok(content, &ctx_default());
        assert!(parsed.persona.description.is_none(), "input={content}");
        assert!(parsed.facts.is_empty(), "input={content}");
    }
}

/// 值类型不符：按未提供，不报错。
#[test]
fn wrong_typed_fields_are_treated_as_absent() {
    let content = r#"{
        "name": "测试",
        "description": 42,
        "personality": ["外向"],
        "mes_example": { "a": "b" },
        "system_prompt": true,
        "character_book": null
    }"#;
    let parsed = st_ok(content, &ctx_default());
    assert!(parsed.persona.description.is_none());
    assert!(parsed.facts.is_empty());
    assert!(parsed.personality_raw.is_none());
    assert!(parsed.examples.is_empty());
    assert!(parsed.prompt.role_suffix.is_none());
    assert!(parsed.hints.is_empty(), "提示: {:?}", parsed.hints);
}

// =========================================================
// mes_example 切块配对
// =========================================================

/// 标准交替对：多对 + `<START>` 分隔 + 尾部单侧丢弃。
#[test]
fn mes_example_pairs_with_start_separator() {
    let content = r#"{
        "name": "测试",
        "mes_example": "{{user}}: 问一\n{{char}}: 答一\n<START>\n{{user}}: 问二\n{{char}}: 答二\n{{user}}: 落单"
    }"#;
    let parsed = st_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 2);
    assert_eq!(parsed.examples[0].user, "问一");
    assert_eq!(parsed.examples[0].assistant, "答一");
    assert_eq!(parsed.examples[1].user, "问二");
    assert_eq!(parsed.examples[1].assistant, "答二");
}

/// 多行块：块内多行按换行连接并整体 trim。
#[test]
fn mes_example_multiline_blocks_join_with_newline() {
    let content = r#"{
        "name": "测试",
        "mes_example": "{{user}}: 第一行\n第二行\n\n{{char}}: 回复一\n回复二"
    }"#;
    let parsed = st_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(parsed.examples[0].user, "第一行\n第二行");
    assert_eq!(parsed.examples[0].assistant, "回复一\n回复二");
}

/// 前缀大小写不敏感。
#[test]
fn mes_example_prefix_is_case_insensitive() {
    let content = r#"{"name": "测试", "mes_example": "{{USER}}: 嗨\n{{Char}}: 嗨呀"}"#;
    let parsed = st_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(parsed.examples[0].user, "嗨");
    assert_eq!(parsed.examples[0].assistant, "嗨呀");
}

/// 全部不成对（开头 char 与孤立 user）：0 对且无跳过明细。
#[test]
fn mes_example_all_unpaired_yields_zero_pairs() {
    let content = r#"{"name": "测试", "mes_example": "{{char}}: 只有角色\n{{user}}: 只有用户"}"#;
    let parsed = st_ok(content, &ctx_default());
    assert!(parsed.examples.is_empty());
    assert!(parsed.skipped.is_empty());
}

/// 开头孤立 char 丢弃，后续对正常成立。
#[test]
fn mes_example_leading_char_block_is_dropped() {
    let content = r#"{
        "name": "测试",
        "mes_example": "{{char}}: 开头\n{{user}}: 问\n{{char}}: 答"
    }"#;
    let parsed = st_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(parsed.examples[0].user, "问");
    assert_eq!(parsed.examples[0].assistant, "答");
}

/// 连续 char 块：中间孤立 char 丢弃，后续对正常成立。
#[test]
fn mes_example_consecutive_char_blocks_drop_middle() {
    let content = r#"{
        "name": "测试",
        "mes_example": "{{user}}: 一\n{{char}}: 二\n{{char}}: 三\n{{user}}: 四\n{{char}}: 五"
    }"#;
    let parsed = st_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 2);
    assert_eq!(parsed.examples[0].user, "一");
    assert_eq!(parsed.examples[0].assistant, "二");
    assert_eq!(parsed.examples[1].user, "四");
    assert_eq!(parsed.examples[1].assistant, "五");
}

/// 无当前块的内容行丢弃。
#[test]
fn mes_example_content_lines_before_any_block_are_dropped() {
    let content = r#"{"name": "测试", "mes_example": "游离行\n{{user}}: 问\n{{char}}: 答"}"#;
    let parsed = st_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(parsed.examples[0].user, "问");
}

/// mes_example 内重复对：跳过并记明细。
#[test]
fn duplicated_example_pair_is_skipped() {
    let content = r#"{
        "name": "测试",
        "mes_example": "{{user}}: 问\n{{char}}: 答\n<START>\n{{user}}: 问\n{{char}}: 答"
    }"#;
    let parsed = st_ok(content, &ctx_default());
    assert_eq!(parsed.examples.len(), 1);
    assert_eq!(
        count_skipped(&parsed, SkipCategory::Examples, SkipReason::Duplicate),
        1
    );
    assert_hint(&parsed, "example_duplicate");
}

// =========================================================
// 名称兜底与截断
// =========================================================

/// name 缺失：文件名 stem 兜底并双写同值。
#[test]
fn missing_name_falls_back_to_file_stem() {
    let parsed = st_ok(
        r#"{"description": "无名字"}"#,
        &ctx_with_stem("char-fallback"),
    );
    assert_eq!(parsed.persona.name, "char-fallback");
    assert_eq!(parsed.identity.assistant_name, "char-fallback");
}

/// name 与 stem 均无（含空白 name）：文件级失败且消息含 name。
#[test]
fn missing_name_without_stem_is_error() {
    let err = parse_st_json(r#"{"description": "无名字"}"#, &ctx_default()).unwrap_err();
    assert!(format!("{err}").contains("name"));

    let err = parse_st_json(r#"{"name": "   "}"#, &ctx_default()).unwrap_err();
    assert!(format!("{err}").contains("name"));
}

/// name 超 64 字符：截断 + 提示，双写同值。
#[test]
fn long_name_is_truncated_and_double_written() {
    let name = "名".repeat(70);
    let content = format!(r#"{{"name": "{name}"}}"#);
    let parsed = st_ok(&content, &ctx_default());
    assert_eq!(parsed.persona.name.chars().count(), 64);
    assert_eq!(parsed.identity.assistant_name, parsed.persona.name);
    assert_hint(&parsed, "name_truncated");
}

/// system_prompt 超 1000 字符：截断 + 提示。
#[test]
fn long_system_prompt_is_truncated() {
    let prompt = "补".repeat(1001);
    let content = format!(r#"{{"name": "测试", "system_prompt": "{prompt}"}}"#);
    let parsed = st_ok(&content, &ctx_default());
    let suffix = parsed
        .prompt
        .role_suffix
        .as_deref()
        .expect("应保留截断后的补丁");
    assert_eq!(suffix.chars().count(), 1000);
    assert_hint(&parsed, "suffix_truncated");
}

/// description 超 2000 字符：列与 fact 两落点各自截断。
#[test]
fn long_description_truncates_both_landings() {
    let description = "描".repeat(2100);
    let content = format!(r#"{{"name": "测试", "description": "{description}"}}"#);
    let parsed = st_ok(&content, &ctx_default());
    let column = parsed.persona.description.as_deref().expect("应有描述列值");
    assert_eq!(column.chars().count(), 2000);
    assert_eq!(parsed.facts.len(), 1);
    assert_eq!(parsed.facts[0].content.chars().count(), 500);
    assert_hint(&parsed, "description_truncated");
    assert_hint(&parsed, "content_truncated");
}

/// description 在 500~2000 之间：列不截断，fact 独自截断。
#[test]
fn mid_length_description_truncates_fact_only() {
    let description = "描".repeat(600);
    let content = format!(r#"{{"name": "测试", "description": "{description}"}}"#);
    let parsed = st_ok(&content, &ctx_default());
    let column = parsed.persona.description.as_deref().expect("应有描述列值");
    assert_eq!(column.chars().count(), 600);
    assert_eq!(parsed.facts[0].content.chars().count(), 500);
    assert_hint(&parsed, "content_truncated");
    assert!(
        !parsed
            .hints
            .iter()
            .any(|h| h.code == "description_truncated")
    );
}

// =========================================================
// 文件级失败
// =========================================================

/// JSON 语法非法：文件级失败。
#[test]
fn invalid_json_is_file_level_failure() {
    let err = parse_st_json("{ not json", &ctx_default()).unwrap_err();
    assert!(format!("{err}").contains("解析 ST 角色卡 JSON 失败"));
}

/// 顶层非对象：文件级失败。
#[test]
fn non_object_json_is_file_level_failure() {
    for content in ["[1, 2]", "\"文本\"", "42"] {
        let err = parse_st_json(content, &ctx_default()).unwrap_err();
        let message = format!("{err}");
        assert!(
            message.contains("解析 ST 角色卡 JSON 失败"),
            "input={content} 消息: {message}"
        );
    }
}

// =========================================================
// PNG 全链路
// =========================================================

/// PNG 全链路：chara（base64 JSON）提取并完成映射。
#[test]
fn png_card_maps_via_chara() {
    let json = r#"{"data": {"name": "PNG 角色", "description": "PNG 描述"}}"#;
    let png = build_png_with_chara(json);
    let parsed = parse_st_png(&png, &ctx_default()).expect("PNG 解析应成功");
    assert_eq!(parsed.format, PresetFormat::StPng);
    assert_eq!(parsed.persona.name, "PNG 角色");
    assert_eq!(parsed.persona.description.as_deref(), Some("PNG 描述"));
    assert_eq!(parsed.facts.len(), 1);
}

/// PNG 无 chara 数据：文件级失败。
#[test]
fn png_without_chara_is_error() {
    let png = build_png_without_chara();
    assert!(parse_st_png(&png, &ctx_default()).is_err());
}

/// chara 数据 base64 非法：文件级失败。
#[test]
fn png_with_invalid_base64_is_error() {
    let mut png = png_signature();
    png.extend_from_slice(&png_chunk(b"IHDR", &[0u8; 13]));
    let mut text = b"chara".to_vec();
    text.push(0);
    text.extend_from_slice(b"not*valid*base64");
    png.extend_from_slice(&png_chunk(b"tEXt", &text));
    png.extend_from_slice(&png_chunk(b"IEND", &[]));
    assert!(parse_st_png(&png, &ctx_default()).is_err());
}

// =========================================================
// 字节级统一入口
// =========================================================

/// 字节级入口：按内容判定三分支。
#[test]
fn parse_preset_dispatches_by_content() {
    let toml = "[identity]\nassistant_name = \"超集角色\"\n";
    let parsed = parse_preset(toml.as_bytes(), &ctx_default()).expect("TOML 分支应成功");
    assert_eq!(parsed.format, PresetFormat::Toml);

    let json = "  {\"name\": \"JSON 角色\"}";
    let parsed = parse_preset(json.as_bytes(), &ctx_default()).expect("JSON 分支应成功");
    assert_eq!(parsed.format, PresetFormat::StJson);
    assert_eq!(parsed.persona.name, "JSON 角色");

    let png = build_png_with_chara(r#"{"name": "PNG 角色"}"#);
    let parsed = parse_preset(&png, &ctx_default()).expect("PNG 分支应成功");
    assert_eq!(parsed.format, PresetFormat::StPng);
    assert_eq!(parsed.persona.name, "PNG 角色");
}

/// 非 UTF-8 字节：文件级失败。
#[test]
fn parse_preset_rejects_non_utf8_bytes() {
    let bytes = [0xFFu8, 0xFE, 0x00, 0x7B];
    let err = parse_preset(&bytes, &ctx_default()).unwrap_err();
    assert!(format!("{err}").contains("预设文件不是合法 UTF-8"));
}
