//! crates/ramaria-memory/src/preset/parse/helpers.rs - Ramaria 预设解析取值与校验辅助
//!
//! 设计特点:
//! - 表取值归一（trim 非空字符串、字符串数组）与超长截断
//! - uid / kind / ProfileField / 行为参数的格式与值域校验
//! - 零 I/O，供解析入口与各解析子模块共用

use ramaria_core::ProfileField;
use ramaria_core::behavior::BehaviorParams;
use ramaria_core::types::PersonaKind;
use toml::Value;
use tracing::debug;

use super::{NAME_MAX_CHARS, Parser, Table};

/// 读取表中键的字符串值（trim 后非空；其他类型与空值按未提供）。
pub(super) fn text_value(table: &Table, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 读取字符串数组（其他类型项忽略，逐项 trim 剔空）。
pub(super) fn parse_string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 字符串数组去重（保留首现序）。
pub(super) fn dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

/// 超长截断（按字符数）；返回 (结果, 是否截断)。
pub(super) fn truncate_chars(value: String, limit: usize) -> (String, bool) {
    if value.chars().count() > limit {
        (value.chars().take(limit).collect(), true)
    } else {
        (value, false)
    }
}

/// 名称截断：超长截断并记提示。
pub(super) fn truncate_name(value: String, key: &str, parser: &mut Parser) -> String {
    let (value, truncated) = truncate_chars(value, NAME_MAX_CHARS);
    if truncated {
        parser.warn(
            "name_truncated",
            format!("{key} 超过 {NAME_MAX_CHARS} 字符，已截断"),
        );
    }
    value
}

/// uid 格式校验：`前缀-序号`（前缀 2~8 个小写字母，序号 1~4 位数字）。
pub(super) fn is_valid_uid(uid: &str) -> bool {
    let Some((prefix, seq)) = uid.split_once('-') else {
        return false;
    };
    let prefix_ok =
        (2..=8).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_lowercase());
    let seq_ok = (1..=4).contains(&seq.len()) && seq.bytes().all(|b| b.is_ascii_digit());
    prefix_ok && seq_ok
}

/// kind 字符串解析（大小写不敏感）。
pub(super) fn parse_kind(raw: &str) -> Option<PersonaKind> {
    match raw.to_ascii_lowercase().as_str() {
        "user" => Some(PersonaKind::User),
        "rama" => Some(PersonaKind::Rama),
        "char" => Some(PersonaKind::Char),
        "anim" => Some(PersonaKind::Anim),
        "oc" => Some(PersonaKind::Oc),
        "hist" => Some(PersonaKind::Hist),
        _ => None,
    }
}

/// uid 前缀到 kind 的已知映射（未知前缀无对应值）。
pub(super) fn known_prefix_kind(prefix: &str) -> Option<PersonaKind> {
    match prefix {
        "user" => Some(PersonaKind::User),
        "rama" => Some(PersonaKind::Rama),
        "char" => Some(PersonaKind::Char),
        "anim" => Some(PersonaKind::Anim),
        "oc" => Some(PersonaKind::Oc),
        "hist" => Some(PersonaKind::Hist),
        _ => None,
    }
}

/// ProfileField 严格解析：大小写不敏感 + 忽略下划线；未知值返回 None。
pub(super) fn parse_profile_field_strict(raw: &str) -> Option<ProfileField> {
    let normalized = raw.trim().to_ascii_lowercase().replace('_', "");
    match normalized.as_str() {
        "basicinfo" => Some(ProfileField::BasicInfo),
        "personalstatus" => Some(ProfileField::PersonalStatus),
        "interests" => Some(ProfileField::Interests),
        "social" => Some(ProfileField::Social),
        "history" => Some(ProfileField::History),
        "recentcontext" => Some(ProfileField::RecentContext),
        "speakingstyle" => Some(ProfileField::SpeakingStyle),
        _ => None,
    }
}

/// 数值取值（整数或浮点均可转为 f64）。
pub(super) fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(number) => Some(*number as f64),
        Value::Float(number) => Some(*number),
        _ => None,
    }
}

/// 行为参数合并：认可 4 键按值域写入，未知键忽略，非法值整体拒绝。
pub(super) fn merge_params(value: &Value, params: &mut BehaviorParams) -> bool {
    let Some(fields) = value.as_table() else {
        return false;
    };
    for (key, raw) in fields {
        let (range, slot): (std::ops::RangeInclusive<f64>, &mut f64) = match key.as_str() {
            "emotion_intensity" => (-1.0..=1.0, &mut params.emotional_intensity),
            "proactiveness" => (0.0..=1.0, &mut params.proactiveness),
            "detail_level" => (0.0..=1.0, &mut params.detail_level),
            "formality" => (0.0..=1.0, &mut params.formality),
            _ => {
                debug!(key = %key, "忽略 behavior_rules params 中的未知键");
                continue;
            }
        };
        let Some(number) = value_as_f64(raw) else {
            return false;
        };
        if !range.contains(&number) {
            return false;
        }
        *slot = number;
    }
    true
}
