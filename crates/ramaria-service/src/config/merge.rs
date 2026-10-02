//! crates/ramaria-service/src/config/merge.rs - Ramaria 配置合并保留与显式键集提取
//!
//! 设计特点:
//! - DB 侧真实键集合并到文件侧配置（DB 优先，仅覆盖 DB 实际存在的键）
//! - "文件显式声明的键集"提取：同步只认文件里真实写了的键（缺键不反向覆盖 DB）
//! - TOML 保留合并：文件头注释块与当前 schema 未知键（含未知分组）原样保留
//! - `[backend]` 组单独标记：未声明时后端组不参与回写
//! - 空文件 / 解析失败返回空键集，不误回写

use std::collections::{BTreeMap, BTreeSet};

use ramaria_core::config::RamariaConfig;
use ramaria_core::types::BackendConfig;
use serde_json::Value as JsonValue;

use super::ExplicitFileKeys;
use super::SKIP_FLAT_KEYS;
use super::backend_map::backend_selection_from_backend_config;
use super::flatten::flat_map_to_config;

// =========================================================
// DB 侧键集合并（DB 优先）
// =========================================================

/// 将 DB 侧真实键集合并到文件侧配置上（DB 优先，仅覆盖 DB 实际存在的键）。
///
/// 用途:
/// - 首启（文件缺失）与文件损坏时：以 DB 为准生成生效配置，防止覆盖用户数据。
/// - 只读回显（`load_config_only`）：反映运行时实际生效值。
///
/// 说明:
/// - settings 键经 `flat_map_to_config` 覆盖；backend 组由 backend_config 表覆盖
///   （保留文件侧 `online_memory_injection` 等 DB 无对应字段的值）。
pub(super) fn merge_db_into_file(
    file: &RamariaConfig,
    db_flat: &BTreeMap<String, JsonValue>,
    db_backend: Option<&BackendConfig>,
) -> RamariaConfig {
    // settings 键：仅覆盖 DB 真实存在的键；合并失败时保持文件侧（不降级整个配置）
    let merged = match flat_map_to_config(db_flat, file) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(error = %e, "DB 侧配置键合并失败，保持文件侧配置");
            file.clone()
        }
    };
    // backend 组：DB 有记录则覆盖（保留文件侧 online_memory_injection）
    match db_backend {
        Some(bc) => {
            let mut cfg = merged;
            cfg.backend = backend_selection_from_backend_config(bc, &file.backend);
            cfg
        }
        None => merged,
    }
}

// =========================================================
// 文件显式键集与 TOML 保留合并
// =========================================================

/// 从原始 TOML 文本提取"文件显式声明的键集"。
///
/// 说明:
/// - 空文件/解析失败 → 返回空键集（调用方在解析失败路径已回退默认配置，不会误回写）；
/// - 跳过 version / schema_version / paths 组（与 `SKIP_FLAT_KEYS` 一致）；
/// - `[backend]` 组单独标记（键集里不含点分键，回写策略见 `sync_db_to_file`）。
pub(super) fn parse_explicit_file_keys(text: &str) -> ExplicitFileKeys {
    let mut keys = ExplicitFileKeys::default();

    let Ok(value) = text.parse::<toml::Value>() else {
        return keys;
    };
    let Some(table) = value.as_table() else {
        return keys;
    };

    keys.has_backend_group = table.contains_key("backend");
    collect_explicit_keys(table, "", &mut keys.flat);
    keys.flat.retain(|key| {
        !SKIP_FLAT_KEYS
            .iter()
            .any(|skip| key == skip || key.starts_with(&format!("{skip}.")))
    });
    keys
}

/// 递归收集叶子键为点分键（数组/标量均视为叶子）。
fn collect_explicit_keys(table: &toml::value::Table, prefix: &str, out: &mut BTreeSet<String>) {
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(inner) if !inner.is_empty() => {
                collect_explicit_keys(inner, &full_key, out);
            }
            _ => {
                out.insert(full_key);
            }
        }
    }
}

/// 取"文件头注释块"（文件开头的注释与空行；遇到首个非注释内容即停止）。
///
/// 用途:
/// - 全量序列化会丢注释；此函数把用户写在文件头部的说明原样保留。
/// - 非空返回值以换行结尾，便于与序列化正文直接拼接。
pub(super) fn leading_comment_block(text: &str) -> String {
    let mut header = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            header.push_str(line);
            header.push('\n');
            continue;
        }
        break;
    }
    header
}

/// 把旧表中"当前 schema 未知"的键合并回新表，避免全量序列化丢数据。
///
/// 说明:
/// - 双方同为表时递归合并（未知键可落在已知分组内）；
/// - 新表已有的键以新表为准（配置值不反向覆盖）。
pub(super) fn merge_unknown_keys(
    new_table: &mut toml::value::Table,
    old_table: &toml::value::Table,
) {
    for (key, old_value) in old_table {
        match new_table.get_mut(key) {
            Some(toml::Value::Table(new_inner)) => {
                if let toml::Value::Table(old_inner) = old_value {
                    merge_unknown_keys(new_inner, old_inner);
                }
            }
            Some(_) => {}
            None => {
                new_table.insert(key.clone(), old_value.clone());
            }
        }
    }
}
