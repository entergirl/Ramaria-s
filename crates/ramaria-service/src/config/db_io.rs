//! crates/ramaria-service/src/config/db_io.rs - Ramaria 配置 DB 侧读写与同步写回
//!
//! 设计特点:
//! - 读取源：settings 表真实存在的 `config.*` 键集 + backend_config 表
//! - 以文件为准写回：仅回写"文件显式声明"的键，DB 缺失补齐、值不同记 mismatch
//! - 写回 backend_config 表时保留既有 capability / embedding_model_path
//! - 单键 / 单侧写失败记失败明细并告警，降级不阻塞
//! - 统一写入口 DB 部分：settings 全量扁平化 + 后端组合并保留

use std::collections::BTreeMap;

use ramaria_core::config::RamariaConfig;
use ramaria_core::types::BackendConfig;
use serde_json::Value as JsonValue;

use super::ConfigWriter;
use super::ExplicitFileKeys;
use super::MismatchEntry;
use super::SETTINGS_KEY_PREFIX;
use super::backend_map::{backend_config_from_selection, backend_fields_equal};
use super::flatten::{config_to_flat_map, format_value, json_value_to_setting};

// =========================================================
// DB 侧 I/O
// =========================================================

impl ConfigWriter {
    /// 读取 DB 侧配置源：settings 表真实存在的 `config.*` 键集 + backend_config 表。
    ///
    /// 返回:
    /// - `(flat, backend)`：
    ///   - `flat`: settings 表 `config.*` 键（去掉前缀）→ JSON 标量，仅含真实存在的键。
    ///   - `backend`: backend_config 表内容（无记录时为 None）。
    pub(super) async fn read_db_sources(
        &self,
    ) -> (BTreeMap<String, JsonValue>, Option<BackendConfig>) {
        let mut flat: BTreeMap<String, JsonValue> = BTreeMap::new();

        match self.storage.list_settings().await {
            Ok(settings) => {
                for (key, value) in settings {
                    if let Some(rest) = key.strip_prefix(SETTINGS_KEY_PREFIX) {
                        // 值以 JSON 标量文本存储（数字 "30"、bool "true"、字符串 "\"char\""）
                        let parsed = serde_json::from_str::<JsonValue>(&value)
                            .unwrap_or(JsonValue::String(value.clone()));
                        flat.insert(rest.to_string(), parsed);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "读取 settings 表失败，跳过 DB 侧配置");
            }
        }

        let backend = match self.storage.get_backend_config().await {
            Ok(opt) => opt,
            Err(e) => {
                tracing::warn!(error = %e, "读取 backend_config 表失败，跳过后端配置");
                None
            }
        };

        (flat, backend)
    }

    /// 以文件为准写回 DB 侧（backend_config 表 + settings 表 `config.*` 键）。
    ///
    /// 参数:
    /// - `file_cfg`: 文件侧配置（canonical）。
    /// - `explicit_keys`: 文件**显式声明**的键集（缺失键不参与回写）。
    /// - `db_flat`: DB 侧 settings 真实键集。
    /// - `db_backend`: DB 侧 backend_config 表内容。
    ///
    /// 返回:
    /// - `(mismatches, db_write_failures)`：不一致明细与写失败明细。
    pub(super) async fn sync_db_to_file(
        &self,
        file_cfg: &RamariaConfig,
        explicit_keys: &ExplicitFileKeys,
        db_flat: &BTreeMap<String, JsonValue>,
        db_backend: Option<&BackendConfig>,
    ) -> (Vec<MismatchEntry>, Vec<String>) {
        let mut mismatches = Vec::new();
        let mut failures = Vec::new();

        // ---- settings 组（除 backend/paths/version 外）----
        let file_flat = config_to_flat_map(file_cfg);

        // 需要写回的键：值不同（mismatch）或 DB 缺失。
        // 只遍历"文件显式声明"的键：文件缺失的键是反序列化默认值，
        // 不得用于覆盖 DB 中用户显式设置（文件缺键时默认值不得反向覆盖）。
        let mut keys_to_write: BTreeMap<String, JsonValue> = BTreeMap::new();
        for key in &explicit_keys.flat {
            let Some(file_value) = file_flat.get(key) else {
                // 文件里写了但当前 schema 无此键（未知键）→ 不参与同步
                continue;
            };
            match db_flat.get(key) {
                Some(db_value) if db_value == file_value => {
                    // 一致，无需写回
                }
                Some(db_value) => {
                    mismatches.push(MismatchEntry {
                        key: format!("{SETTINGS_KEY_PREFIX}{key}"),
                        file_value: format_value(file_value),
                        db_value: format_value(db_value),
                    });
                    keys_to_write.insert(key.clone(), file_value.clone());
                }
                None => {
                    // DB 缺失 → 补齐
                    keys_to_write.insert(key.clone(), file_value.clone());
                }
            }
        }

        // ---- backend 组（backend_config 表）----
        // 文件未显式声明 [backend] 时后端组不参与回写：否则默认值会覆盖 DB 用户配置。
        let file_backend = if explicit_keys.has_backend_group {
            Some(backend_config_from_selection(&file_cfg.backend, None))
        } else {
            None
        };
        let backend_mismatch = match (&file_backend, db_backend) {
            (Some(fb), Some(db_bc)) => !backend_fields_equal(fb, db_bc),
            (Some(_), None) => true, // DB 无记录 → 补齐
            (None, _) => false,
        };
        if backend_mismatch {
            if let Some(fb) = &file_backend {
                let db_desc = db_backend
                    .map(|b| format!("provider={} model={}", b.provider, b.capability.model_id))
                    .unwrap_or_else(|| "（无记录）".to_string());
                mismatches.push(MismatchEntry {
                    key: "backend".to_string(),
                    file_value: format!(
                        "provider={} model={}",
                        fb.provider, fb.capability.model_id
                    ),
                    db_value: db_desc,
                });
            }
        }

        // 无差异 → 不写回
        if keys_to_write.is_empty() && !backend_mismatch {
            return (mismatches, failures);
        }

        // 写回 settings 表
        if !keys_to_write.is_empty() {
            for (key, value) in &keys_to_write {
                let setting_key = format!("{SETTINGS_KEY_PREFIX}{key}");
                let setting_value = json_value_to_setting(value);
                if let Err(e) = self.storage.set_setting(&setting_key, &setting_value).await {
                    let msg = format!("settings 表写入失败 ({setting_key}): {e}");
                    failures.push(msg.clone());
                    tracing::warn!(key = %setting_key, error = %e, "DB 侧配置写回失败（降级不阻塞）");
                }
            }
        }

        // 写回 backend_config 表（保留既有 capability / embedding_model_path）
        if backend_mismatch {
            if let Some(fb) = &file_backend {
                let existing = match self.storage.get_backend_config().await {
                    Ok(opt) => opt,
                    Err(e) => {
                        let msg = format!("读取 backend_config 表失败，跳过后端写回: {e}");
                        failures.push(msg.clone());
                        tracing::warn!(error = %e, "读取 backend_config 表失败（降级不阻塞）");
                        return (mismatches, failures);
                    }
                };
                let merged = match existing {
                    Some(mut bc) => {
                        bc.provider = fb.provider;
                        bc.base_url = fb.base_url.clone();
                        bc.embedding_model_id = fb.embedding_model_id.clone();
                        bc.temperature = fb.temperature;
                        bc.max_tokens = fb.max_tokens;
                        bc.capability.provider = fb.capability.provider;
                        bc.capability.model_id = fb.capability.model_id.clone();
                        bc.capability.base_url = fb.capability.base_url.clone();
                        bc
                    }
                    None => fb.clone(),
                };
                if let Err(e) = self.storage.save_backend_config(&merged).await {
                    let msg = format!("backend_config 表写入失败: {e}");
                    failures.push(msg.clone());
                    tracing::warn!(error = %e, "backend_config 表写回失败（降级不阻塞）");
                }
            }
        }

        (mismatches, failures)
    }

    /// 将完整配置写入 DB 侧（统一写入口的 DB 部分）。
    ///
    /// 返回:
    /// - `true`：DB 侧全部写入成功。
    pub(super) async fn write_db_config(
        &self,
        cfg: &RamariaConfig,
        failures: &mut Vec<String>,
    ) -> bool {
        let mut all_ok = true;

        // settings 表：扁平化（跳过 version/paths/backend）
        let flat = config_to_flat_map(cfg);
        for (key, value) in &flat {
            let setting_key = format!("{SETTINGS_KEY_PREFIX}{key}");
            let setting_value = json_value_to_setting(value);
            if let Err(e) = self.storage.set_setting(&setting_key, &setting_value).await {
                all_ok = false;
                let msg = format!("settings 表写入失败 ({setting_key}): {e}");
                failures.push(msg.clone());
                tracing::warn!(key = %setting_key, error = %e, "统一写入口 DB 侧写入失败（降级不阻塞）");
            }
        }

        // backend_config 表：合并保留既有 capability / embedding_model_path
        let backend = backend_config_from_selection(&cfg.backend, None);
        let existing = match self.storage.get_backend_config().await {
            Ok(opt) => opt,
            Err(e) => {
                all_ok = false;
                let msg = format!("读取 backend_config 表失败，跳过后端写回: {e}");
                failures.push(msg.clone());
                tracing::warn!(error = %e, "统一写入口读取 backend_config 表失败（降级不阻塞）");
                return all_ok;
            }
        };
        let merged = match existing {
            Some(mut bc) => {
                bc.provider = backend.provider;
                bc.base_url = backend.base_url.clone();
                bc.embedding_model_id = backend.embedding_model_id.clone();
                bc.temperature = backend.temperature;
                bc.max_tokens = backend.max_tokens;
                bc.capability.provider = backend.capability.provider;
                bc.capability.model_id = backend.capability.model_id.clone();
                bc.capability.base_url = backend.capability.base_url.clone();
                bc
            }
            None => backend,
        };
        if let Err(e) = self.storage.save_backend_config(&merged).await {
            all_ok = false;
            let msg = format!("backend_config 表写入失败: {e}");
            failures.push(msg.clone());
            tracing::warn!(error = %e, "统一写入口 backend_config 表写入失败（降级不阻塞）");
        }

        all_ok
    }
}
