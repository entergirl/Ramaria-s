//! crates/ramaria-service/src/config/flatten.rs - Ramaria 配置扁平键值编解码
//!
//! 设计特点:
//! - 展平：配置序列化为点分键 → JSON 标量（跳过 version/schema_version/paths/backend 组）
//! - 反展平：仅覆盖基础配置中已存在的路径，未知键忽略（向前兼容）
//! - 数组（如 persona_kind_whitelist）保留为 JSON 数组标量
//! - settings 表文本口径：数字 "30" / bool "true" / 字符串保留 JSON 引号
//! - 纯函数无 I/O，不做日志；调用方决定失败降级口径

use std::collections::BTreeMap;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use serde_json::Value as JsonValue;

use super::SKIP_FLAT_KEYS;

// =========================================================
// 扁平化工具（RamariaConfig ↔ 扁平键值）
// =========================================================

/// 将配置扁平化为点分键 → JSON 标量（跳过 version/schema_version/paths/backend）。
///
/// 说明:
/// - 数组（如 persona_kind_whitelist）保留为 JSON 数组标量。
/// - 返回的 map 键即 settings 表 `config.*` 后缀。
pub(super) fn config_to_flat_map(cfg: &RamariaConfig) -> BTreeMap<String, JsonValue> {
    let mut out = BTreeMap::new();
    let Ok(root) = serde_json::to_value(cfg) else {
        return out;
    };
    let Some(obj) = root.as_object() else {
        return out;
    };
    for (group, value) in obj {
        if SKIP_FLAT_KEYS.contains(&group.as_str()) {
            continue;
        }
        flatten_value(group, value, &mut out);
    }
    out
}

/// 递归展开嵌套对象为点分键。
fn flatten_value(prefix: &str, value: &JsonValue, out: &mut BTreeMap<String, JsonValue>) {
    match value {
        JsonValue::Object(map) => {
            for (k, v) in map {
                let key = format!("{prefix}.{k}");
                flatten_value(&key, v, out);
            }
        }
        _ => {
            out.insert(prefix.to_string(), value.clone());
        }
    }
}

/// 将扁平键值 map 合并到基础配置（默认配置）上。
///
/// 说明:
/// - 仅覆盖基础配置中已存在的路径；未知键忽略（向前兼容）。
/// - 值类型由目标字段决定（serde 自动转换 JSON 标量）。
pub(super) fn flat_map_to_config(
    flat: &BTreeMap<String, JsonValue>,
    base: &RamariaConfig,
) -> RamariaResult<RamariaConfig> {
    let mut root = serde_json::to_value(base).map_err(|e| {
        ramaria_core::error::RamariaError::serialization(format!("配置序列化失败: {e}"))
    })?;

    for (key, value) in flat {
        // 逐段下钻到目标路径（不存在则跳过——未知键不覆盖）
        let mut current = &mut root;
        let segments: Vec<&str> = key.split('.').collect();
        let mut reached = true;
        for seg in &segments[..segments.len() - 1] {
            match current.get_mut(*seg) {
                Some(JsonValue::Object(_)) => current = current.get_mut(*seg).expect("已确认存在"),
                _ => {
                    reached = false;
                    break;
                }
            }
        }
        if !reached {
            continue;
        }
        let last = segments[segments.len() - 1];
        if current.get(last).is_some() {
            current[last] = value.clone();
        }
    }

    serde_json::from_value(root).map_err(|e| {
        ramaria_core::error::RamariaError::serialization(format!("配置反序列化失败: {e}"))
    })
}

/// 将 JSON 标量转为 settings 表存储文本（数字 "30"、bool "true"、字符串 "\"char\""）。
pub(super) fn json_value_to_setting(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()),
        other => other.to_string(),
    }
}

/// 格式化 JSON 标量为展示文本。
pub(super) fn format_value(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => s.clone(),
        other => other.to_string(),
    }
}
