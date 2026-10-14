//! crates/ramaria-memory/src/preset/parse/mod.rs - Ramaria 预设超集 TOML 解析入口
//!
//! 设计特点:
//! - persona.toml 超集解析入口：注册段 / 对话身份 / 自由文本块 / L3 结构化段 / 提示词补丁
//! - 分层宽容：文件级失败仅限 TOML 语法、uid/kind 主键校验与名称缺失；条目级问题降级
//! - 跳过明细与提示成对记录（同一事件同时进 skipped 与 hints）
//! - 解析产物不含原文全文（提示消息只含键名、条目标识与计数）

use ramaria_core::{RamariaError, RamariaResult};
use toml::Value;
use tracing::debug;

use super::{PresetFormat, PresetParseContext};
use crate::preset::model::{
    HintLevel, IdentitySection, ParsedPreset, PersonaSection, PresetHint, SkipCategory, SkipReason,
    SkippedEntry,
};

mod helpers;
mod registration;
mod sections;
mod st;

pub use st::{parse_st_json, parse_st_png};

use helpers::text_value;
use registration::{resolve_description, resolve_kind, resolve_name, resolve_uid};
use sections::{
    parse_blocks, parse_examples, parse_facts, parse_prompt, parse_rules, parse_traits,
};

/// TOML 表别名。
pub(super) type Table = toml::map::Map<String, Value>;

/// 名称长度上限（字符数）。
pub(super) const NAME_MAX_CHARS: usize = 64;
/// 描述长度上限（字符数）。
pub(super) const DESCRIPTION_MAX_CHARS: usize = 2000;
/// trait meaning 长度上限（字符数）。
pub(super) const MEANING_MAX_CHARS: usize = 200;
/// fact content 长度上限（字符数）。
pub(super) const CONTENT_MAX_CHARS: usize = 500;
/// 提示词后缀长度上限（字符数）。
pub(super) const SUFFIX_MAX_CHARS: usize = 1000;
/// traits 单层条数上限。
pub(super) const TRAITS_PER_LAYER_MAX: usize = 30;

/// uid 格式非法时的统一提示（前缀 2~8 个小写字母，序号 1~4 位数字）。
pub(super) const UID_FORMAT_HINT: &str = "须为 前缀-序号：前缀 2~8 个小写字母，序号 1~4 位数字";
/// kind 非法时的统一提示（合法值枚举）。
pub(super) const KIND_VALUE_HINT: &str = "合法值: user / rama / char / anim / oc / hist";

/// 已知顶层节键（其余顶层键忽略）。
const KNOWN_SECTIONS: [&str; 8] = [
    "persona",
    "identity",
    "blocks",
    "traits",
    "facts",
    "examples",
    "behavior_rules",
    "prompt",
];

// =========================================================
// 入口
// =========================================================

/// 解析预设 TOML 内容为统一中间表示。
///
/// 参数:
/// - `content`: 预设 TOML 原始文本。
/// - `ctx`: 解析上下文（文件名 stem 与 source 缺省值）。
///
/// 返回:
/// - 成功时返回 ParsedPreset；条目级问题以提示与跳过明细记录，不阻断。
/// - 文件级失败（返回 Err）：TOML 语法非法 / 显式 uid 格式非法 / kind 值非法 /
///   uid 前缀与 kind 不一致 / 名称缺失。
pub fn parse_toml(content: &str, ctx: &PresetParseContext) -> RamariaResult<ParsedPreset> {
    let root: Value = content
        .parse()
        .map_err(|e| RamariaError::config(format!("解析 persona.toml 失败: {e}")))?;
    let Some(table) = root.as_table() else {
        return Err(RamariaError::config(
            "解析 persona.toml 失败: 顶层结构不是表",
        ));
    };

    let mut parser = Parser::new();
    for key in table.keys() {
        if !KNOWN_SECTIONS.contains(&key.as_str()) {
            debug!(key = %key, "忽略未知顶层节");
        }
    }

    let persona_table = parser.optional_table(table, "persona");
    let identity_table = parser.optional_table(table, "identity");

    let uid = resolve_uid(persona_table, ctx, &mut parser)?;
    let (kind, kind_source) = resolve_kind(persona_table, uid.as_deref())?;
    let (name, assistant_name) = resolve_name(persona_table, identity_table, ctx, &mut parser)?;
    let description = resolve_description(persona_table, identity_table, &mut parser);
    let source = persona_table
        .and_then(|t| text_value(t, "source"))
        .unwrap_or_else(|| ctx.default_source.clone());

    let persona = PersonaSection {
        uid,
        kind,
        kind_source,
        name,
        avatar: persona_table.and_then(|t| text_value(t, "avatar")),
        description,
        source,
    };
    let identity = IdentitySection {
        assistant_name,
        user_name: identity_table.and_then(|t| text_value(t, "user_name")),
        work: identity_table.and_then(|t| text_value(t, "work")),
        era: identity_table.and_then(|t| text_value(t, "era")),
    };

    Ok(ParsedPreset {
        format: PresetFormat::Toml,
        persona,
        identity,
        blocks: parse_blocks(table, &mut parser),
        traits: parse_traits(table, &mut parser),
        facts: parse_facts(table, &mut parser),
        examples: parse_examples(table, &mut parser),
        rules: parse_rules(table, &mut parser),
        prompt: parse_prompt(table, &mut parser),
        personality_raw: None,
        hints: parser.hints,
        skipped: parser.skipped,
    })
}

// =========================================================
// 提示与跳过明细收集
// =========================================================

/// 解析过程中的提示与跳过明细收集器。
pub(super) struct Parser {
    hints: Vec<PresetHint>,
    skipped: Vec<SkippedEntry>,
}

impl Parser {
    fn new() -> Self {
        Self {
            hints: Vec::new(),
            skipped: Vec::new(),
        }
    }

    /// 记录一条警告提示。
    pub(super) fn warn(&mut self, code: &str, message: String) {
        self.hints.push(PresetHint {
            level: HintLevel::Warn,
            code: code.to_string(),
            message,
        });
    }

    /// 记录一条信息提示。
    pub(super) fn info(&mut self, code: &str, message: String) {
        self.hints.push(PresetHint {
            level: HintLevel::Info,
            code: code.to_string(),
            message,
        });
    }

    /// 记录一次条目跳过：同一事件同时进跳过明细与警告提示。
    pub(super) fn skip_entry(
        &mut self,
        category: SkipCategory,
        label: Option<String>,
        reason: SkipReason,
        code: &str,
        message: String,
    ) {
        self.warn(code, message);
        self.skipped.push(SkippedEntry {
            category,
            label,
            reason,
        });
    }

    /// 读取顶层节表；存在但非表时记一条格式提示。
    fn optional_table<'a>(&mut self, table: &'a Table, key: &str) -> Option<&'a Table> {
        match table.get(key) {
            Some(Value::Table(section)) => Some(section),
            Some(_) => {
                self.warn(
                    &format!("{key}_section_invalid"),
                    format!("[{key}] 节格式非法，已忽略"),
                );
                None
            }
            None => None,
        }
    }
}
