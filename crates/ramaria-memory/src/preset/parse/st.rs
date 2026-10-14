//! crates/ramaria-memory/src/preset/parse/st.rs - Ramaria ST 角色卡解析模块
//!
//! 设计特点:
//! - SillyTavern 角色卡（Character Card v1 / v2）JSON 到统一中间表示的字段映射
//! - PNG 角色卡经 tEXt chara 提取后复用 JSON 映射（仅格式标记不同）
//! - 字段级宽容：类型不符与空串按未提供，忽略字段不报错；仅 JSON 非法与名称缺失为文件级失败
//! - mes_example 按 {{user}} / {{char}} 前缀切块并交替配对为 examples
//! - 解析产物不含原文全文（提示消息只含键名、条目标识与计数）

use ramaria_core::types::PersonaKind;
use ramaria_core::{ProfileField, RamariaError, RamariaResult};
use serde_json::Value;

use super::helpers::{truncate_chars, truncate_name};
use super::{CONTENT_MAX_CHARS, DESCRIPTION_MAX_CHARS, Parser, SUFFIX_MAX_CHARS};
use crate::preset::model::{
    ExampleEntry, FactEntry, IdentitySection, KindSource, ParsedPreset, PersonaSection,
    PromptPatch, SkipCategory, SkipReason, TraitsSection,
};
use crate::preset::png::extract_chara_text;
use crate::preset::{PresetFormat, PresetParseContext};

/// ST 卡片对象（字段名到 JSON 值的映射）。
type CardObject = serde_json::Map<String, Value>;

// =========================================================
// 入口
// =========================================================

/// 解析 SillyTavern 角色卡 JSON（Character Card v1 / v2）为统一中间表示。
///
/// 参数:
/// - `content`: 角色卡 JSON 文本；顶层 `data` 为对象时按 v2 取该对象，否则按 v1 读顶层。
/// - `ctx`: 解析上下文（文件名 stem 兜底与 source 缺省值）。
///
/// 返回:
/// - 文件级失败：JSON 语法非法 / 顶层结构不是对象 / 缺少 name 且无文件名兜底。
pub fn parse_st_json(content: &str, ctx: &PresetParseContext) -> RamariaResult<ParsedPreset> {
    let root: Value = serde_json::from_str(content)
        .map_err(|e| RamariaError::config(format!("解析 ST 角色卡 JSON 失败: {e}")))?;
    let Some(root_object) = root.as_object() else {
        return Err(RamariaError::config(
            "解析 ST 角色卡 JSON 失败: 顶层结构不是对象",
        ));
    };
    let card = match root_object.get("data").and_then(Value::as_object) {
        Some(data) => data,
        None => root_object,
    };

    let mut parser = Parser::new();
    let name = resolve_card_name(card, ctx, &mut parser)?;

    // description：列与 BasicInfo fact 两落点各自截断，同一全文来源
    let description_source = card_text(card, "description");
    let mut facts: Vec<FactEntry> = Vec::new();
    let description = description_source.as_ref().map(|raw| {
        let (value, truncated) = truncate_chars(raw.clone(), DESCRIPTION_MAX_CHARS);
        if truncated {
            parser.warn(
                "description_truncated",
                format!("ST 角色卡 description 超过 {DESCRIPTION_MAX_CHARS} 字符，已截断"),
            );
        }
        value
    });
    if let Some(raw) = description_source {
        let (content, truncated) = truncate_chars(raw, CONTENT_MAX_CHARS);
        if truncated {
            parser.warn(
                "content_truncated",
                format!("facts（BasicInfo）内容超过 {CONTENT_MAX_CHARS} 字符，已截断"),
            );
        }
        facts.push(FactEntry {
            field: ProfileField::BasicInfo,
            content,
        });
    }

    let personality_raw = card_text(card, "personality");
    let examples = parse_mes_example(card, &mut parser);
    let role_suffix = card_text(card, "system_prompt").map(|raw| {
        let (value, truncated) = truncate_chars(raw, SUFFIX_MAX_CHARS);
        if truncated {
            parser.warn(
                "suffix_truncated",
                format!("ST 角色卡 system_prompt 超过 {SUFFIX_MAX_CHARS} 字符，已截断"),
            );
        }
        value
    });

    if card
        .get("character_book")
        .is_some_and(|value| !value.is_null())
    {
        parser.info(
            "st_lorebook_ignored",
            "角色卡世界书（character_book）暂不支持，已忽略".to_string(),
        );
    }

    Ok(ParsedPreset {
        format: PresetFormat::StJson,
        persona: PersonaSection {
            uid: None,
            kind: Some(PersonaKind::Char),
            kind_source: KindSource::Default,
            name: name.clone(),
            avatar: None,
            description,
            source: ctx.default_source.clone(),
        },
        identity: IdentitySection {
            assistant_name: name,
            user_name: None,
            work: None,
            era: None,
        },
        blocks: Vec::new(),
        traits: TraitsSection::default(),
        facts,
        examples,
        rules: Vec::new(),
        prompt: PromptPatch {
            role_suffix,
            style_suffix: None,
        },
        personality_raw,
        hints: parser.hints,
        skipped: parser.skipped,
    })
}

/// 从 PNG 角色卡字节解析（提取 tEXt `chara` 数据后走 JSON 映射）。
///
/// 参数:
/// - `bytes`: PNG 文件原始字节。
/// - `ctx`: 解析上下文（文件名 stem 兜底与 source 缺省值）。
///
/// 返回:
/// - 文件级失败：PNG 签名非法 / chunk 数据损坏 / 无 chara 数据 / JSON 映射失败。
pub fn parse_st_png(bytes: &[u8], ctx: &PresetParseContext) -> RamariaResult<ParsedPreset> {
    let Some(json) = extract_chara_text(bytes)? else {
        return Err(RamariaError::validation(
            "PNG 角色卡缺少 chara 数据".to_string(),
        ));
    };
    let mut parsed = parse_st_json(&json, ctx)?;
    parsed.format = PresetFormat::StPng;
    Ok(parsed)
}

// =========================================================
// 字段映射
// =========================================================

/// 读取卡片对象键的字符串值（trim 后非空；其他类型与空值按未提供）。
fn card_text(card: &CardObject, key: &str) -> Option<String> {
    card.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 名称解析：卡片 name 优先（超长截断），否则文件名 stem 兜底；均无 → 文件级失败。
fn resolve_card_name(
    card: &CardObject,
    ctx: &PresetParseContext,
    parser: &mut Parser,
) -> RamariaResult<String> {
    if let Some(name) = card_text(card, "name") {
        return Ok(truncate_name(name, "ST 角色卡 name", parser));
    }
    ctx.file_stem
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| RamariaError::validation("ST 角色卡缺少 name 且无文件名兜底"))
}

// =========================================================
// mes_example 切块与配对
// =========================================================

/// 示例块说话者。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExampleSpeaker {
    User,
    Char,
}

/// 解析 mes_example 为交替配对的 examples；重复条目跳过并记明细。
fn parse_mes_example(card: &CardObject, parser: &mut Parser) -> Vec<ExampleEntry> {
    let mut examples: Vec<ExampleEntry> = Vec::new();
    let Some(raw) = card_text(card, "mes_example") else {
        return examples;
    };
    let blocks = split_example_blocks(&raw);

    let mut index = 0;
    while index < blocks.len() {
        let Some(next) = blocks.get(index + 1) else {
            break;
        };
        let (speaker, text) = &blocks[index];
        if *speaker != ExampleSpeaker::User || next.0 != ExampleSpeaker::Char {
            index += 1;
            continue;
        }
        let entry = ExampleEntry {
            user: text.clone(),
            assistant: next.1.clone(),
        };
        if examples
            .iter()
            .any(|e| e.user == entry.user && e.assistant == entry.assistant)
        {
            parser.skip_entry(
                SkipCategory::Examples,
                None,
                SkipReason::Duplicate,
                "example_duplicate",
                "examples 条目重复，已跳过".to_string(),
            );
        } else {
            examples.push(entry);
        }
        index += 2;
    }

    examples
}

/// 按前缀行切分 mes_example 为消息块；`<START>` 行结束当前块，空块丢弃。
fn split_example_blocks(raw: &str) -> Vec<(ExampleSpeaker, String)> {
    let mut blocks: Vec<(ExampleSpeaker, String)> = Vec::new();
    let mut current: Option<(ExampleSpeaker, Vec<String>)> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "<START>" {
            finish_block(&mut current, &mut blocks);
            continue;
        }
        if let Some((speaker, rest)) = match_speaker_prefix(trimmed) {
            finish_block(&mut current, &mut blocks);
            let first = rest.trim_start();
            let first = first.strip_prefix(':').unwrap_or(first).trim();
            let mut lines: Vec<String> = Vec::new();
            if !first.is_empty() {
                lines.push(first.to_string());
            }
            current = Some((speaker, lines));
            continue;
        }
        if let Some((_, lines)) = current.as_mut() {
            lines.push(trimmed.to_string());
        }
    }
    finish_block(&mut current, &mut blocks);

    blocks
}

/// 结束当前块：内容行以换行连接并整体 trim，空块丢弃。
fn finish_block(
    current: &mut Option<(ExampleSpeaker, Vec<String>)>,
    blocks: &mut Vec<(ExampleSpeaker, String)>,
) {
    let Some((speaker, lines)) = current.take() else {
        return;
    };
    let text = lines.join("\n");
    let text = text.trim();
    if !text.is_empty() {
        blocks.push((speaker, text.to_string()));
    }
}

/// 行首说话者前缀匹配（大小写不敏感，前缀后可跟冒号与空白）；无前缀返回 None。
fn match_speaker_prefix(line: &str) -> Option<(ExampleSpeaker, &str)> {
    const PREFIXES: [(&str, ExampleSpeaker); 2] = [
        ("{{user}}", ExampleSpeaker::User),
        ("{{char}}", ExampleSpeaker::Char),
    ];
    for (prefix, speaker) in PREFIXES {
        let Some(head) = line.get(..prefix.len()) else {
            continue;
        };
        if head.eq_ignore_ascii_case(prefix) {
            return Some((speaker, &line[prefix.len()..]));
        }
    }
    None
}
