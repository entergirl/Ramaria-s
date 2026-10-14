//! crates/ramaria-memory/src/preset/parse/sections.rs - Ramaria 预设结构化段解析
//!
//! 设计特点:
//! - 自由文本块（保文件序、取值归一）与四类结构化段（traits / facts / examples / rules）
//! - 提示词补丁解析（追加语义，空值按未配置）
//! - 条目级宽容：非法或重复条目拒绝并进跳过明细，不阻断整体解析

use ramaria_core::TraitLayer;
use ramaria_core::behavior::BehaviorParams;
use std::collections::HashSet;
use toml::Value;
use tracing::{debug, warn};

use super::helpers::{
    dedup_strings, merge_params, parse_profile_field_strict, parse_string_array, text_value,
    truncate_chars,
};
use super::{
    CONTENT_MAX_CHARS, MEANING_MAX_CHARS, Parser, SUFFIX_MAX_CHARS, TRAITS_PER_LAYER_MAX, Table,
};
use crate::preset::model::{
    ExampleEntry, FactEntry, PromptPatch, RuleEntry, SkipCategory, SkipReason, TraitEntry,
    TraitsSection,
};

/// 解析 [blocks]：保文件序，值取字符串并归一末尾空白。
pub(super) fn parse_blocks(table: &Table, parser: &mut Parser) -> Vec<(String, String)> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    match table.get("blocks") {
        Some(Value::Table(section)) => {
            for (key, value) in section {
                match value.as_str() {
                    Some(text) => blocks.push((key.clone(), normalize_block_text(text))),
                    None => parser.warn(
                        "blocks_value_ignored",
                        format!("[blocks].{key} 不是字符串，已忽略"),
                    ),
                }
            }
        }
        Some(_) => parser.warn(
            "blocks_section_invalid",
            "[blocks] 节格式非法，已忽略".to_string(),
        ),
        None => {}
    }
    if blocks.is_empty() {
        warn!("persona.toml 的 [blocks] 节为空，将创建无结构化画像的 rama persona");
    }
    blocks
}

/// 自由文本块取值归一：CRLF 折为 LF，末尾空白去除。
fn normalize_block_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_string()
}

/// 解析 [traits] 三层；跨层 label 去重，单层超限拒绝超出部分。
pub(super) fn parse_traits(table: &Table, parser: &mut Parser) -> TraitsSection {
    let mut section = TraitsSection::default();
    let mut seen_labels: HashSet<String> = HashSet::new();

    let Some(layers) = table.get("traits").and_then(Value::as_table) else {
        if table.contains_key("traits") {
            parser.warn(
                "traits_section_invalid",
                "[traits] 节格式非法，已忽略".to_string(),
            );
        }
        return section;
    };

    for (layer_key, layer_value) in layers {
        let layer = match layer_key.as_str() {
            "base" => TraitLayer::Base,
            "primary" => TraitLayer::Primary,
            "accent" => TraitLayer::Accent,
            _ => {
                parser.warn(
                    "unknown_traits_layer",
                    format!("traits 层 {layer_key} 不可识别，已忽略"),
                );
                continue;
            }
        };
        let Some(items) = layer_value.as_array() else {
            parser.warn(
                "traits_layer_invalid",
                format!("traits 层 {layer_key} 不是数组，已忽略"),
            );
            continue;
        };
        let bucket = if layer == TraitLayer::Base {
            &mut section.base
        } else if layer == TraitLayer::Primary {
            &mut section.primary
        } else {
            &mut section.accent
        };

        for item in items {
            let Some(fields) = item.as_table() else {
                parser.skip_entry(
                    SkipCategory::Traits,
                    None,
                    SkipReason::Invalid,
                    "trait_entry_invalid",
                    "traits 条目格式非法，已跳过".to_string(),
                );
                continue;
            };
            let (label, meaning) =
                match (text_value(fields, "label"), text_value(fields, "meaning")) {
                    (Some(label), Some(meaning)) => (label, meaning),
                    (label, _) => {
                        parser.skip_entry(
                            SkipCategory::Traits,
                            label,
                            SkipReason::Invalid,
                            "trait_entry_invalid",
                            format!("traits 层 {layer_key} 的条目缺少 label 或 meaning，已跳过"),
                        );
                        continue;
                    }
                };

            if seen_labels.contains(&label) {
                parser.skip_entry(
                    SkipCategory::Traits,
                    Some(label.clone()),
                    SkipReason::LabelDuplicate,
                    "trait_label_duplicate",
                    format!("label「{label}」跨层重复，已跳过"),
                );
                continue;
            }
            if bucket.len() >= TRAITS_PER_LAYER_MAX {
                parser.skip_entry(
                    SkipCategory::Traits,
                    Some(label.clone()),
                    SkipReason::Overflow,
                    "traits_layer_overflow",
                    format!(
                        "traits 层 {layer_key} 超过 {TRAITS_PER_LAYER_MAX} 条，已跳过「{label}」"
                    ),
                );
                continue;
            }

            let (meaning, truncated) = truncate_chars(meaning, MEANING_MAX_CHARS);
            if truncated {
                parser.warn(
                    "meaning_truncated",
                    format!("traits「{label}」的 meaning 超过 {MEANING_MAX_CHARS} 字符，已截断"),
                );
            }
            seen_labels.insert(label.clone());
            bucket.push(TraitEntry {
                layer,
                label,
                meaning,
                not_meaning: text_value(fields, "not_meaning"),
                trigger: text_value(fields, "trigger"),
                suppress: text_value(fields, "suppress"),
                related: text_value(fields, "related"),
                motives: parse_string_array(fields.get("motives")),
            });
        }
    }

    section
}

/// 解析 [[facts]]；field 值域严格校验（未知值拒绝该条），内容去重与截断。
pub(super) fn parse_facts(table: &Table, parser: &mut Parser) -> Vec<FactEntry> {
    let mut facts: Vec<FactEntry> = Vec::new();
    let Some(items) = table.get("facts").and_then(Value::as_array) else {
        if table.contains_key("facts") {
            parser.warn(
                "facts_section_invalid",
                "[[facts]] 节格式非法，已忽略".to_string(),
            );
        }
        return facts;
    };

    for item in items {
        let Some(fields) = item.as_table() else {
            parser.skip_entry(
                SkipCategory::Facts,
                None,
                SkipReason::Invalid,
                "fact_entry_invalid",
                "facts 条目格式非法，已跳过".to_string(),
            );
            continue;
        };
        let field_text = fields
            .get("field")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let Some(field) = parse_profile_field_strict(field_text) else {
            let shown = if field_text.is_empty() {
                "(缺失)".to_string()
            } else {
                field_text.to_string()
            };
            parser.skip_entry(
                SkipCategory::Facts,
                None,
                SkipReason::Invalid,
                "fact_field_invalid",
                format!("facts 条目 field「{shown}」不可识别，已跳过"),
            );
            continue;
        };
        let Some(content) = text_value(fields, "content") else {
            parser.skip_entry(
                SkipCategory::Facts,
                Some(field.as_str().to_string()),
                SkipReason::Invalid,
                "fact_entry_invalid",
                "facts 条目缺少 content，已跳过".to_string(),
            );
            continue;
        };

        let (content, truncated) = truncate_chars(content, CONTENT_MAX_CHARS);
        if truncated {
            parser.warn(
                "content_truncated",
                format!(
                    "facts（{}）内容超过 {CONTENT_MAX_CHARS} 字符，已截断",
                    field.as_str()
                ),
            );
        }
        if facts
            .iter()
            .any(|f| f.field == field && f.content == content)
        {
            parser.skip_entry(
                SkipCategory::Facts,
                Some(field.as_str().to_string()),
                SkipReason::Duplicate,
                "fact_duplicate",
                format!("facts（{}）内容重复，已跳过", field.as_str()),
            );
            continue;
        }
        facts.push(FactEntry { field, content });
    }

    facts
}

/// 解析 [[examples]]；用户与回复均必填，完全一致者去重。
pub(super) fn parse_examples(table: &Table, parser: &mut Parser) -> Vec<ExampleEntry> {
    let mut examples: Vec<ExampleEntry> = Vec::new();
    let Some(items) = table.get("examples").and_then(Value::as_array) else {
        if table.contains_key("examples") {
            parser.warn(
                "examples_section_invalid",
                "[[examples]] 节格式非法，已忽略".to_string(),
            );
        }
        return examples;
    };

    for item in items {
        let Some(fields) = item.as_table() else {
            parser.skip_entry(
                SkipCategory::Examples,
                None,
                SkipReason::Invalid,
                "example_entry_invalid",
                "examples 条目格式非法，已跳过".to_string(),
            );
            continue;
        };
        let (Some(user), Some(assistant)) =
            (text_value(fields, "user"), text_value(fields, "assistant"))
        else {
            parser.skip_entry(
                SkipCategory::Examples,
                None,
                SkipReason::Invalid,
                "example_entry_invalid",
                "examples 条目缺少 user 或 assistant，已跳过".to_string(),
            );
            continue;
        };
        if examples
            .iter()
            .any(|e| e.user == user && e.assistant == assistant)
        {
            parser.skip_entry(
                SkipCategory::Examples,
                None,
                SkipReason::Duplicate,
                "example_duplicate",
                "examples 条目重复，已跳过".to_string(),
            );
            continue;
        }
        examples.push(ExampleEntry { user, assistant });
    }

    examples
}

/// 解析 [[behavior_rules]]；关键词必填，参数按值域校验，重复规则去重。
pub(super) fn parse_rules(table: &Table, parser: &mut Parser) -> Vec<RuleEntry> {
    let mut rules: Vec<RuleEntry> = Vec::new();
    let Some(items) = table.get("behavior_rules").and_then(Value::as_array) else {
        if table.contains_key("behavior_rules") {
            parser.warn(
                "behavior_rules_section_invalid",
                "[[behavior_rules]] 节格式非法，已忽略".to_string(),
            );
        }
        return rules;
    };

    for item in items {
        let Some(fields) = item.as_table() else {
            parser.skip_entry(
                SkipCategory::BehaviorRules,
                None,
                SkipReason::Invalid,
                "rule_entry_invalid",
                "behavior_rules 条目格式非法，已跳过".to_string(),
            );
            continue;
        };
        let keywords = dedup_strings(parse_string_array(fields.get("keywords")));
        if keywords.is_empty() {
            parser.skip_entry(
                SkipCategory::BehaviorRules,
                None,
                SkipReason::Invalid,
                "rule_keywords_invalid",
                "behavior_rules 条目缺少有效 keywords，已跳过".to_string(),
            );
            continue;
        }
        let label = keywords.first().cloned();

        let mut params = BehaviorParams::default();
        if let Some(params_value) = fields.get("params") {
            if !merge_params(params_value, &mut params) {
                parser.skip_entry(
                    SkipCategory::BehaviorRules,
                    label,
                    SkipReason::Invalid,
                    "rule_params_invalid",
                    "behavior_rules 条目 params 非法，已跳过".to_string(),
                );
                continue;
            }
        }

        let reaction = text_value(fields, "reaction");
        let mut sorted = keywords.clone();
        sorted.sort();
        let duplicated = rules.iter().any(|rule| {
            let mut other = rule.keywords.clone();
            other.sort();
            other == sorted && rule.reaction == reaction
        });
        if duplicated {
            parser.skip_entry(
                SkipCategory::BehaviorRules,
                label,
                SkipReason::Duplicate,
                "rule_duplicate",
                "behavior_rules 条目重复，已跳过".to_string(),
            );
            continue;
        }

        rules.push(RuleEntry {
            keywords,
            reaction,
            avoid: parse_string_array(fields.get("avoid")),
            params,
        });
    }

    rules
}

/// 解析 [prompt] 补丁；空值按未配置，超长截断。
pub(super) fn parse_prompt(table: &Table, parser: &mut Parser) -> PromptPatch {
    let Some(fields) = table.get("prompt").and_then(Value::as_table) else {
        if table.contains_key("prompt") {
            parser.warn(
                "prompt_section_invalid",
                "[prompt] 节格式非法，已忽略".to_string(),
            );
        }
        return PromptPatch::default();
    };
    for key in fields.keys() {
        if key != "role_suffix" && key != "style_suffix" {
            debug!(key = %key, "忽略 [prompt] 中的未知键");
        }
    }
    PromptPatch {
        role_suffix: normalize_suffix(fields, "role_suffix", parser),
        style_suffix: normalize_suffix(fields, "style_suffix", parser),
    }
}

/// 后缀键归一：空值按未配置，超长截断。
fn normalize_suffix(table: &Table, key: &str, parser: &mut Parser) -> Option<String> {
    let value = text_value(table, key)?;
    let (value, truncated) = truncate_chars(value, SUFFIX_MAX_CHARS);
    if truncated {
        parser.warn(
            "suffix_truncated",
            format!("[prompt].{key} 超过 {SUFFIX_MAX_CHARS} 字符，已截断"),
        );
    }
    Some(value)
}
