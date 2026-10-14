//! crates/ramaria-memory/src/preset/parse/registration.rs - Ramaria 预设注册段解析
//!
//! 设计特点:
//! - uid 解析与格式校验（显式字段 > 文件名 stem，stem 不做格式校验）
//! - kind 解析与 uid 前缀一致性校验（主键语义，不一致为文件级失败）
//! - 名称兜底链（name 与 assistant_name 互为缺省，文件名 stem 兜底）
//! - 描述优先级（[persona].description 优先，遗留键兜底并提示迁移）

use ramaria_core::types::PersonaKind;
use ramaria_core::{RamariaError, RamariaResult};
use toml::Value;

use super::helpers::{
    is_valid_uid, known_prefix_kind, parse_kind, text_value, truncate_chars, truncate_name,
};
use super::{DESCRIPTION_MAX_CHARS, KIND_VALUE_HINT, Parser, Table, UID_FORMAT_HINT};
use crate::preset::PresetParseContext;
use crate::preset::model::KindSource;

/// 解析 uid：显式字段（过格式校验）优先，否则文件名 stem（不校验格式）。
pub(super) fn resolve_uid(
    persona_table: Option<&Table>,
    ctx: &PresetParseContext,
    parser: &mut Parser,
) -> RamariaResult<Option<String>> {
    let explicit = match persona_table.and_then(|t| t.get("uid")) {
        None => None,
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other) => {
            return Err(RamariaError::validation(format!(
                "预设 uid 格式非法（{UID_FORMAT_HINT}）: {other}"
            )));
        }
    };

    if let Some(uid) = &explicit {
        if !is_valid_uid(uid) {
            return Err(RamariaError::validation(format!(
                "预设 uid 格式非法（{UID_FORMAT_HINT}）: {uid}"
            )));
        }
        let stem = ctx
            .file_stem
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(stem) = stem {
            if stem != uid {
                parser.warn(
                    "uid_stem_mismatch",
                    format!("预设 uid「{uid}」与文件名「{stem}」不一致，以显式 uid 为准"),
                );
            }
        }
        return Ok(Some(uid.clone()));
    }

    Ok(ctx
        .file_stem
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

/// 解析 kind：显式值（校验合法性）优先，否则由 uid 前缀推断。
///
/// 返回:
/// - `(kind, kind_source)`；uid 与显式 kind 同时可得时校验二者一致性。
pub(super) fn resolve_kind(
    persona_table: Option<&Table>,
    uid: Option<&str>,
) -> RamariaResult<(Option<PersonaKind>, KindSource)> {
    let explicit = match persona_table.and_then(|t| t.get("kind")) {
        None => None,
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                match parse_kind(trimmed) {
                    Some(kind) => Some(kind),
                    None => {
                        return Err(RamariaError::validation(format!(
                            "预设 kind 非法（{KIND_VALUE_HINT}）: {trimmed}"
                        )));
                    }
                }
            }
        }
        Some(other) => {
            return Err(RamariaError::validation(format!(
                "预设 kind 非法（{KIND_VALUE_HINT}）: {other}"
            )));
        }
    };

    if let (Some(kind), Some(uid)) = (explicit, uid) {
        let prefix = uid.split('-').next().unwrap_or("");
        let consistent = match known_prefix_kind(prefix) {
            Some(expected) => kind == expected,
            None => kind == PersonaKind::Char,
        };
        if !consistent {
            return Err(RamariaError::validation(format!(
                "预设 uid「{uid}」前缀与 kind「{}」不一致",
                kind.as_str()
            )));
        }
    }

    Ok(match explicit {
        Some(kind) => (Some(kind), KindSource::Explicit),
        None => match uid {
            Some(uid) => (Some(PersonaKind::from_uid(uid)), KindSource::Inferred),
            None => (None, KindSource::Default),
        },
    })
}

/// 解析名称：name 与 assistant_name 互为缺省，文件名 stem 兜底。
///
/// 返回:
/// - `(name, assistant_name)`；二者均无来源时返回文件级错误。
pub(super) fn resolve_name(
    persona_table: Option<&Table>,
    identity_table: Option<&Table>,
    ctx: &PresetParseContext,
    parser: &mut Parser,
) -> RamariaResult<(String, String)> {
    let persona_name = persona_table
        .and_then(|t| text_value(t, "name"))
        .map(|v| truncate_name(v, "[persona].name", parser));
    let identity_name = identity_table
        .and_then(|t| text_value(t, "assistant_name"))
        .map(|v| truncate_name(v, "[identity].assistant_name", parser));
    let stem = ctx
        .file_stem
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let name = persona_name
        .clone()
        .or_else(|| identity_name.clone())
        .or_else(|| stem.clone())
        .ok_or_else(|| {
            RamariaError::validation(
                "预设缺少名称（[persona].name 或 [identity].assistant_name 至少一个）",
            )
        })?;
    let assistant_name = identity_name
        .or(persona_name)
        .or(stem)
        .unwrap_or_else(|| name.clone());
    Ok((name, assistant_name))
}

/// 解析描述：[persona].description 优先；遗留 [identity].description 兜底并提示迁移。
pub(super) fn resolve_description(
    persona_table: Option<&Table>,
    identity_table: Option<&Table>,
    parser: &mut Parser,
) -> Option<String> {
    let persona_desc = persona_table.and_then(|t| text_value(t, "description"));
    let legacy_desc = identity_table.and_then(|t| text_value(t, "description"));

    let picked = match (&persona_desc, &legacy_desc) {
        (Some(current), Some(legacy)) => {
            if current != legacy {
                parser.warn(
                    "legacy_description_key",
                    "[identity].description 为遗留键，已被 [persona].description 覆盖，建议迁移"
                        .to_string(),
                );
            }
            Some(current.clone())
        }
        (Some(current), None) => Some(current.clone()),
        (None, Some(legacy)) => {
            parser.info(
                "legacy_description_key",
                "[identity].description 为遗留键，建议迁移至 [persona].description".to_string(),
            );
            Some(legacy.clone())
        }
        (None, None) => None,
    };

    picked.map(|desc| {
        let (desc, truncated) = truncate_chars(desc, DESCRIPTION_MAX_CHARS);
        if truncated {
            parser.warn(
                "description_truncated",
                format!("description 超过 {DESCRIPTION_MAX_CHARS} 字符，已截断"),
            );
        }
        desc
    })
}
