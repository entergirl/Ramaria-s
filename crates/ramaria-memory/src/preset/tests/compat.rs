//! crates/ramaria-memory/src/preset/tests/compat.rs - 旧解析器兼容包装用例
//!
//! 设计特点:
//! - 覆盖 parse_persona_toml 对旧文本的既有行为（identity 空值归一、blocks 保序）
//! - 覆盖超集文本经包装的字段提取方向与解析失败语义
//! - 覆盖 resolve_chat_style_rules 的显式优先与回退行为

use crate::init::{SHARED_CHAT_STYLE_RULES, parse_persona_toml, resolve_chat_style_rules};

/// 旧文本兼容：identity 空值归一为默认值、blocks 保序。
#[test]
fn legacy_text_wrapper_keeps_old_semantics() {
    let content = r#"[identity]
assistant_name = "黎杋枫"
user_name = ""
description = "背景"

[blocks]
A_persona = "人设"
E_rules = "规则"
"#;
    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "黎杋枫");
    assert_eq!(parsed.user_name, "");
    assert_eq!(parsed.description.as_deref(), Some("背景"));
    assert_eq!(parsed.blocks.len(), 2);
    assert_eq!(parsed.blocks[0].0, "A_persona");
    assert_eq!(parsed.blocks[1].0, "E_rules");
}

/// 超集文本经包装：description 取 [persona] 优先，work / era 仍来自 [identity]。
#[test]
fn wrapper_reads_superset_fields() {
    let content = r#"[persona]
name = "注册名"
description = "新描述"

[identity]
assistant_name = "自称"
work = "作品"
era = "时代"
"#;
    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "自称");
    assert_eq!(parsed.description.as_deref(), Some("新描述"));
    assert_eq!(parsed.work.as_deref(), Some("作品"));
    assert_eq!(parsed.era.as_deref(), Some("时代"));
}

/// [persona].name 可兜底 assistant_name（[identity] 侧缺失时不再报错）。
#[test]
fn wrapper_name_falls_back_to_persona_section() {
    let content = "[persona]\nname = \"注册名\"\n\n[blocks]\nA_persona = \"人设\"\n";
    let parsed = parse_persona_toml(content).expect("解析失败");
    assert_eq!(parsed.assistant_name, "注册名");
}

/// 包装的解析失败与旧行为同为错误（语法非法、名称缺失）。
#[test]
fn wrapper_reports_parse_failures() {
    assert!(parse_persona_toml("[identity]\nassistant_name = \"未闭合\n").is_err());
    assert!(parse_persona_toml("[identity]\nuser_name = \"x\"\n").is_err());
}

/// resolve_chat_style_rules 既有行为保持：显式 E_rules 优先，缺省回退共享规则。
#[test]
fn resolve_chat_style_rules_behavior_preserved() {
    let with_rules = r#"[identity]
assistant_name = "测试"

[blocks]
E_rules = "自定义"
"#;
    assert_eq!(resolve_chat_style_rules(Some(with_rules)), "自定义");

    let without_rules = r#"[identity]
assistant_name = "测试"

[blocks]
A_persona = "人设"
"#;
    assert_eq!(
        resolve_chat_style_rules(Some(without_rules)),
        SHARED_CHAT_STYLE_RULES
    );
    assert_eq!(resolve_chat_style_rules(None), SHARED_CHAT_STYLE_RULES);
}
