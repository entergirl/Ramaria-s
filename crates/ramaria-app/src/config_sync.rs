//! rust/crates/ramaria-app/src/config_sync.rs - 配置双写同步服务（v1.4 D-V14-006）
//!
//! 设计特点:
//! - 统一配置服务：config.toml（canonical）↔ `backend_config` 表 / `settings` 表 双写同步
//! - 启动加载：读取两处并做一致性校验，不一致以文件为准并告警（写回 DB 侧）
//! - config.toml 缺失时生成含全部默认值的模板文件（打包 `config/default.toml`）
//! - 设置页修改经统一写入口（`save_config`）同时落文件与表，单侧写失败降级不阻塞
//! - settings 表使用 `config.*` 前缀的扁平键；`backend` 组映射 `backend_config` 表
//! - paths/version/schema_version 为环境相关元数据，不参与双写（仅文件侧）
//! - API key 始终由 OS keychain 管理，不进入本服务的任何写入路径
//!
//! 安全约束:
//! - 本模块不记录完整配置内容，日志仅记录差异键名与失败上下文
//! - 后端配置写回时保留既有 capability / embedding_model_path，避免覆盖丢失

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::BackendConfig;
use serde_json::Value as JsonValue;

/// settings 表受管键前缀（与既有 `profile_mode` 等键无冲突）。
const SETTINGS_KEY_PREFIX: &str = "config.";

/// 不参与双写的顶级键：环境相关元数据 / 运行时路径 / LLM 连接（后者走 backend_config 表）。
const SKIP_FLAT_KEYS: &[&str] = &["version", "schema_version", "paths", "backend"];

/// 默认配置模板（打包 `config/default.toml`，含完整注释说明）。
const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../../config/default.toml");

// =========================================================
// 结果类型
// =========================================================

/// 单条不一致记录（启动校验时以文件为准）。
#[derive(Debug, Clone)]
pub struct MismatchEntry {
    /// 配置键（如 `utt.theta_gap_minutes`、`backend.provider`）
    pub key: String,
    /// 文件侧值（canonical）
    pub file_value: String,
    /// DB 侧值
    pub db_value: String,
}

/// 启动加载 + 一致性校验结果。
#[derive(Debug)]
pub struct SyncOutcome {
    /// 合并后的生效配置（正常路径以文件为准；首启/损坏路径以 DB 为准）
    pub config: RamariaConfig,
    /// 配置文件是否存在（false 表示本次自动生成了文件：DB 非空时为 merged，DB 空时为模板）
    pub file_existed: bool,
    /// 文件解析错误（解析失败时回退默认值，不阻塞启动）
    pub file_parse_errors: Vec<String>,
    /// 不一致项（已按文件为准写回 DB 侧）
    pub mismatches: Vec<MismatchEntry>,
    /// DB 侧写回失败（降级不阻塞，启动日志告警）
    pub db_write_failures: Vec<String>,
}

/// 统一写入口（`save_config`）的结果。
#[derive(Debug, Default)]
pub struct SyncWriteResult {
    /// config.toml 写入是否成功
    pub file_ok: bool,
    /// DB 侧（backend_config + settings）写入是否全部成功
    pub db_ok: bool,
    /// 失败明细（供 UI 提示与日志）
    pub failures: Vec<String>,
}

impl SyncWriteResult {
    /// 是否完全成功（文件与 DB 双侧均无失败）。
    pub fn is_ok(&self) -> bool {
        self.file_ok && self.db_ok
    }
}

// =========================================================
// 配置双写同步服务
// =========================================================

/// 配置双写同步服务。
///
/// 职责:
/// - 启动时加载 config.toml + DB 两侧配置，一致性校验（文件为准）并回写 DB。
/// - 提供统一写入口，使文件与表永不单侧漂移。
///
/// 用法:
/// ```ignore
/// let service = ConfigSyncService::new(storage, config_dir.join("config.toml"));
/// let outcome = service.load().await?;   // 启动链路
/// service.save_config(&new_cfg).await;   // 设置页修改（统一写入口）
/// ```
///
/// 降级语义:
/// - 文件缺失 → 生成默认模板（不视为错误）。
/// - 文件解析失败 → 回退默认值并以 DB 侧继续，记 warn。
/// - 单侧写失败 → 另一侧仍生效，记 warn 不抛错。
pub struct ConfigSyncService {
    storage: Arc<dyn StorageBackend>,
    config_path: PathBuf,
}

impl ConfigSyncService {
    /// 创建配置同步服务。
    ///
    /// 参数:
    /// - `storage`: 存储后端（backend_config / settings 表读写）。
    /// - `config_path`: config.toml 文件路径（通常为 `{config_dir}/config.toml`）。
    pub fn new(storage: Arc<dyn StorageBackend>, config_path: PathBuf) -> Self {
        Self {
            storage,
            config_path,
        }
    }

    /// 启动加载：读取两侧配置 → 一致性校验（文件为准）→ 回写 DB 侧。
    ///
    /// 流程:
    /// 1. 读取 config.toml（缺失则生成含全部默认值的模板；解析失败回退默认值）。
    /// 2. 读取 backend_config 表与 settings 表（`config.*` 键）作为 DB 侧配置。
    /// 3. 逐键对比，不一致记录 mismatch（以文件为准）。
    /// 4. 将文件侧配置写回 DB（backend_config 表 + settings 表），单侧失败降级。
    ///
    /// 首启/损坏语义（防止升级覆盖用户数据）:
    /// - 文件缺失（v1.3 升级首启无 config.toml）：以 **DB 为准** 合并生效配置，
    ///   并把 DB 侧值写入生成的文件，**不向 DB 回写**。
    /// - 文件解析失败：回退默认值 + DB 侧合并（DB 为准），**不向 DB 回写**。
    ///
    /// 返回:
    /// - `SyncOutcome`：生效配置与校验结果（调用方记录启动日志）。
    pub async fn load(&self) -> RamariaResult<SyncOutcome> {
        // ---- Step 1: 读取文件侧 ----
        let mut file_parse_errors = Vec::new();
        let mut file_existed = true;
        let file_config: RamariaConfig = if !self.config_path.exists() {
            // 文件缺失：不在此处写模板（由下方首启分支统一决策写入，
            // 避免"模板写成功 + merged 写失败"留下合法模板导致下次启动
            // 以默认值覆盖 DB 用户配置的中间态）。
            file_existed = false;
            RamariaConfig::default()
        } else {
            match self.read_file_config() {
                Ok(cfg) => cfg,
                Err(e) => {
                    file_parse_errors.push(e.to_string());
                    tracing::warn!(
                        path = %self.config_path.display(),
                        error = %e,
                        "config.toml 解析失败，回退默认值并以 DB 侧配置继续"
                    );
                    RamariaConfig::default()
                }
            }
        };

        // ---- Step 2: 读取 DB 侧（真实存在的键集 + backend 表）----
        let (db_flat, db_backend) = self.read_db_sources().await;

        // ---- 首启 / 文件损坏：以 DB 为准，不向 DB 回写 ----
        if !file_existed || !file_parse_errors.is_empty() {
            let merged = merge_db_into_file(&file_config, &db_flat, db_backend.as_ref());
            // 首启（文件缺失）时生成文件：
            // - DB 侧有真实配置 → 直接写 merged（含 DB 值），避免先写模板再覆盖的中间态；
            // - DB 为空 → 写带注释的默认模板（等价默认值）。
            // 文件损坏时不覆盖用户文件（保留现场，仅告警）。
            if !file_existed {
                let write_result = if !db_flat.is_empty() || db_backend.is_some() {
                    self.write_file_config(&merged)
                } else {
                    self.write_template_file()
                };
                if let Err(e) = write_result {
                    tracing::warn!(
                        path = %self.config_path.display(),
                        error = %e,
                        "首启生成 config.toml 失败（下次启动将重新同步）"
                    );
                }
            }
            return Ok(SyncOutcome {
                config: merged,
                file_existed,
                file_parse_errors,
                mismatches: Vec::new(),
                db_write_failures: Vec::new(),
            });
        }

        // ---- Step 3+4: 一致性校验 + 以文件为准回写 ----
        let (mismatches, db_write_failures) = self
            .sync_db_to_file(&file_config, &db_flat, db_backend.as_ref())
            .await;

        Ok(SyncOutcome {
            config: file_config,
            file_existed,
            file_parse_errors,
            mismatches,
            db_write_failures,
        })
    }

    /// 只读加载：读取两侧配置并合并（DB 侧优先），**不写回任何一侧**。
    ///
    /// 用途:
    /// - 设置页回显（`get_full_config`）：反映运行时实际生效值且无写副作用。
    /// - 正常双写同步后文件与 DB 一致，此方法返回与 `load` 相同的生效配置。
    ///
    /// 返回:
    /// - 合并后的 RamariaConfig（文件缺失/损坏时以 DB 为准）。
    pub async fn load_config_only(&self) -> RamariaResult<RamariaConfig> {
        let file_config = if self.config_path.exists() {
            self.read_file_config().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "load_config_only 读取配置失败，使用默认值");
                RamariaConfig::default()
            })
        } else {
            RamariaConfig::default()
        };

        let (db_flat, db_backend) = self.read_db_sources().await;
        Ok(merge_db_into_file(
            &file_config,
            &db_flat,
            db_backend.as_ref(),
        ))
    }

    /// 统一写入口：同时写 config.toml 与 DB 两侧（backend_config + settings）。
    ///
    /// 说明:
    /// - 单侧失败降级不阻塞：文件失败时 DB 仍生效；DB 失败时文件仍生效。
    /// - 调用方（设置页 / 配置命令）应展示 `SyncWriteResult.failures` 提示同步失败。
    ///
    /// 返回:
    /// - `SyncWriteResult`：双侧写入结果（不因单侧失败返回 Err）。
    pub async fn save_config(&self, cfg: &RamariaConfig) -> SyncWriteResult {
        let mut result = SyncWriteResult::default();

        // ---- 文件侧 ----
        match self.write_file_config(cfg) {
            Ok(()) => result.file_ok = true,
            Err(e) => {
                result.file_ok = false;
                let msg = format!("config.toml 写入失败: {e}");
                result.failures.push(msg.clone());
                tracing::warn!(path = %self.config_path.display(), error = %e, "配置文件写入失败");
            }
        }

        // ---- DB 侧 ----
        result.db_ok = self.write_db_config(cfg, &mut result.failures).await;

        result
    }

    /// 后端配置同步（供既有 `update_backend_config` 通道复用，保持三处一致）。
    ///
    /// 参数:
    /// - `backend`: 已写入 backend_config 表的新后端配置。
    ///
    /// 说明:
    /// - 读取当前 config.toml（缺失则生成模板），仅更新 `[backend]` 组后写回文件。
    /// - 保留文件侧 `online_memory_injection` 等 DB 无对应字段的值。
    /// - `db_ok` 恒为 true：本方法只同步文件侧，DB 侧已由调用方写入
    ///   （`update_backend_config` 先写表再调用本方法）。
    pub async fn sync_backend_config(&self, backend: &BackendConfig) -> SyncWriteResult {
        let mut result = SyncWriteResult::default();

        // 当前文件配置
        let mut cfg = if self.config_path.exists() {
            match self.read_file_config() {
                Ok(cfg) => cfg,
                Err(e) => {
                    // 解析失败：不覆盖用户文件（保留现场），仅告警并返回失败
                    let msg = format!("读取 config.toml 失败，跳过文件同步: {e}");
                    result.failures.push(msg.clone());
                    tracing::warn!(error = %e, "sync_backend_config 读取配置失败，跳过文件同步");
                    return result;
                }
            }
        } else {
            RamariaConfig::default()
        };

        // 仅更新 backend 组（保留 online_memory_injection）
        cfg.backend = backend_selection_from_backend_config(backend, &cfg.backend);

        // 写回文件
        match self.write_file_config(&cfg) {
            Ok(()) => result.file_ok = true,
            Err(e) => {
                result.failures.push(format!("config.toml 写入失败: {e}"));
                tracing::warn!(error = %e, "sync_backend_config 文件写入失败");
            }
        }
        result.db_ok = true;
        result
    }

    /// 获取 config.toml 路径（诊断/展示用）。
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    // =========================================================
    // 文件侧 I/O
    // =========================================================

    /// 读取并解析 config.toml。
    fn read_file_config(&self) -> RamariaResult<RamariaConfig> {
        let text = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ramaria_core::error::RamariaError::io(
                format!("读取 config.toml 失败: {}", self.config_path.display()),
                Some(e),
            )
        })?;
        toml::from_str(&text).map_err(|e| {
            ramaria_core::error::RamariaError::config(format!("解析 config.toml 失败: {e}"))
        })
    }

    /// 将配置写为 config.toml（含版本头注释）。
    fn write_file_config(&self, cfg: &RamariaConfig) -> RamariaResult<()> {
        let text = toml::to_string_pretty(cfg).map_err(|e| {
            ramaria_core::error::RamariaError::config(format!("序列化配置为 TOML 失败: {e}"))
        })?;
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ramaria_core::error::RamariaError::io(
                    format!("创建配置目录失败: {}", parent.display()),
                    Some(e),
                )
            })?;
        }
        std::fs::write(&self.config_path, text).map_err(|e| {
            ramaria_core::error::RamariaError::io(
                format!("写入 config.toml 失败: {}", self.config_path.display()),
                Some(e),
            )
        })
    }

    /// 生成默认模板文件（config.toml 缺失时调用）。
    fn write_template_file(&self) -> RamariaResult<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ramaria_core::error::RamariaError::io(
                    format!("创建配置目录失败: {}", parent.display()),
                    Some(e),
                )
            })?;
        }
        std::fs::write(&self.config_path, DEFAULT_CONFIG_TEMPLATE).map_err(|e| {
            ramaria_core::error::RamariaError::io(
                format!("生成配置模板失败: {}", self.config_path.display()),
                Some(e),
            )
        })
    }

    // =========================================================
    // DB 侧 I/O
    // =========================================================

    /// 读取 DB 侧配置源：settings 表真实存在的 `config.*` 键集 + backend_config 表。
    ///
    /// 返回:
    /// - `(flat, backend)`：
    ///   - `flat`: settings 表 `config.*` 键（去掉前缀）→ JSON 标量，仅含真实存在的键。
    ///   - `backend`: backend_config 表内容（无记录时为 None）。
    async fn read_db_sources(&self) -> (BTreeMap<String, JsonValue>, Option<BackendConfig>) {
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
    /// - `db_flat`: DB 侧 settings 真实键集。
    /// - `db_backend`: DB 侧 backend_config 表内容。
    ///
    /// 返回:
    /// - `(mismatches, db_write_failures)`：不一致明细与写失败明细。
    async fn sync_db_to_file(
        &self,
        file_cfg: &RamariaConfig,
        db_flat: &BTreeMap<String, JsonValue>,
        db_backend: Option<&BackendConfig>,
    ) -> (Vec<MismatchEntry>, Vec<String>) {
        let mut mismatches = Vec::new();
        let mut failures = Vec::new();

        // ---- settings 组（除 backend/paths/version 外）----
        let file_flat = config_to_flat_map(file_cfg);

        // 需要写回的键：值不同（mismatch）或 DB 缺失
        let mut keys_to_write: BTreeMap<String, JsonValue> = BTreeMap::new();
        for (key, file_value) in &file_flat {
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
        let file_backend = backend_config_from_selection(&file_cfg.backend, None);
        let backend_mismatch = match db_backend {
            Some(db_bc) => !backend_fields_equal(&file_backend, db_bc),
            None => true, // DB 无记录 → 补齐
        };
        if backend_mismatch {
            let db_desc = db_backend
                .map(|b| format!("provider={} model={}", b.provider, b.capability.model_id))
                .unwrap_or_else(|| "（无记录）".to_string());
            mismatches.push(MismatchEntry {
                key: "backend".to_string(),
                file_value: format!(
                    "provider={} model={}",
                    file_backend.provider, file_backend.capability.model_id
                ),
                db_value: db_desc,
            });
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
                    bc.provider = file_backend.provider;
                    bc.base_url = file_backend.base_url.clone();
                    bc.embedding_model_id = file_backend.embedding_model_id.clone();
                    bc.temperature = file_backend.temperature;
                    bc.max_tokens = file_backend.max_tokens;
                    bc.capability.provider = file_backend.capability.provider;
                    bc.capability.model_id = file_backend.capability.model_id.clone();
                    bc.capability.base_url = file_backend.capability.base_url.clone();
                    bc
                }
                None => file_backend.clone(),
            };
            if let Err(e) = self.storage.save_backend_config(&merged).await {
                let msg = format!("backend_config 表写入失败: {e}");
                failures.push(msg.clone());
                tracing::warn!(error = %e, "backend_config 表写回失败（降级不阻塞）");
            }
        }

        (mismatches, failures)
    }

    /// 将完整配置写入 DB 侧（统一写入口的 DB 部分）。
    ///
    /// 返回:
    /// - `true`：DB 侧全部写入成功。
    async fn write_db_config(&self, cfg: &RamariaConfig, failures: &mut Vec<String>) -> bool {
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

// =========================================================
// 扁平化工具（RamariaConfig ↔ 扁平键值）
// =========================================================

/// 将配置扁平化为点分键 → JSON 标量（跳过 version/schema_version/paths/backend）。
///
/// 说明:
/// - 数组（如 persona_kind_whitelist）保留为 JSON 数组标量。
/// - 返回的 map 键即 settings 表 `config.*` 后缀。
fn config_to_flat_map(cfg: &RamariaConfig) -> BTreeMap<String, JsonValue> {
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
fn flat_map_to_config(
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
fn json_value_to_setting(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()),
        other => other.to_string(),
    }
}

/// 将 DB 侧真实键集合并到文件侧配置上（DB 优先，仅覆盖 DB 实际存在的键）。
///
/// 用途:
/// - 首启（文件缺失）与文件损坏时：以 DB 为准生成生效配置，防止覆盖用户数据。
/// - 只读回显（`load_config_only`）：反映运行时实际生效值。
///
/// 说明:
/// - settings 键经 `flat_map_to_config` 覆盖；backend 组由 backend_config 表覆盖
///   （保留文件侧 `online_memory_injection` 等 DB 无对应字段的值）。
fn merge_db_into_file(
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

/// 格式化 JSON 标量为展示文本。
fn format_value(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// =========================================================
// BackendSelection ↔ BackendConfig 映射
// =========================================================

/// 将 `RamariaConfig.backend`（BackendSelection）映射为 BackendConfig。
///
/// 说明:
/// - `existing` 提供 capability / embedding_model_path 基线（None 时按 provider 默认构造）。
/// - 业务字段（provider/base_url/model_id/embedding/temperature/max_tokens）以文件为准覆盖。
fn backend_config_from_selection(
    sel: &ramaria_core::config::BackendSelection,
    existing: Option<&BackendConfig>,
) -> BackendConfig {
    let mut bc = existing.cloned().unwrap_or_else(|| {
        BackendConfig::new_with_defaults(sel.provider, sel.base_url.clone(), sel.model_id.clone())
    });
    bc.provider = sel.provider;
    bc.base_url = sel.base_url.clone();
    bc.embedding_model_id = sel.embedding_model_id.clone();
    bc.temperature = sel.temperature;
    bc.max_tokens = sel.max_tokens;
    bc.capability.provider = sel.provider;
    bc.capability.model_id = sel.model_id.clone();
    bc.capability.base_url = sel.base_url.clone();
    bc
}

/// 将 BackendConfig 映射为 `RamariaConfig.backend`（BackendSelection）。
///
/// 说明:
/// - `online_memory_injection` 仅存在于文件侧（DB 无对应字段），保留 fallback 值。
fn backend_selection_from_backend_config(
    bc: &BackendConfig,
    fallback: &ramaria_core::config::BackendSelection,
) -> ramaria_core::config::BackendSelection {
    ramaria_core::config::BackendSelection {
        provider: bc.provider,
        model_id: bc.capability.model_id.clone(),
        base_url: bc.base_url.clone(),
        embedding_model_id: bc.embedding_model_id.clone(),
        temperature: bc.temperature,
        max_tokens: bc.max_tokens,
        online_memory_injection: fallback.online_memory_injection,
    }
}

/// 比较两个 BackendConfig 的业务字段（忽略 capability 的派生字段与 embedding_model_path）。
fn backend_fields_equal(a: &BackendConfig, b: &BackendConfig) -> bool {
    a.provider == b.provider
        && a.base_url == b.base_url
        && a.embedding_model_id == b.embedding_model_id
        && (a.temperature - b.temperature).abs() < f64::EPSILON
        && a.max_tokens == b.max_tokens
        && a.capability.model_id == b.capability.model_id
}

// =========================================================
// 单元测试（mock storage，确定性断言）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{LlmProvider, PersonaKind};
    use std::sync::Mutex;

    /// 最小 mock storage：仅实现本模块用到的 settings / backend_config 方法。
    #[derive(Default)]
    struct MockStorage {
        settings: Mutex<BTreeMap<String, String>>,
        backend: Mutex<Option<BackendConfig>>,
    }

    #[async_trait::async_trait]
    impl StorageBackend for MockStorage {
        async fn create_session(
            &self,
            _p: Option<&str>,
        ) -> RamariaResult<ramaria_core::types::Session> {
            Err(ramaria_core::error::RamariaError::unsupported("mock"))
        }
        async fn close_session(&self, _id: uuid::Uuid) -> RamariaResult<()> {
            Err(ramaria_core::error::RamariaError::unsupported("mock"))
        }
        async fn get_session(
            &self,
            _id: uuid::Uuid,
        ) -> RamariaResult<Option<ramaria_core::types::Session>> {
            Ok(None)
        }
        async fn list_active_sessions(&self) -> RamariaResult<Vec<ramaria_core::types::Session>> {
            Ok(vec![])
        }
        async fn list_sessions(&self) -> RamariaResult<Vec<ramaria_core::types::Session>> {
            Ok(vec![])
        }
        async fn delete_session(&self, _id: uuid::Uuid) -> RamariaResult<()> {
            Ok(())
        }
        async fn save_message(&self, _m: &ramaria_core::types::Message) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_messages(
            &self,
            _id: uuid::Uuid,
        ) -> RamariaResult<Vec<ramaria_core::types::Message>> {
            Ok(vec![])
        }
        async fn list_messages_by_persona(
            &self,
            _p: &str,
        ) -> RamariaResult<Vec<ramaria_core::types::Message>> {
            Ok(vec![])
        }
        async fn find_message_by_fingerprint(
            &self,
            _f: &str,
        ) -> RamariaResult<Option<ramaria_core::types::Message>> {
            Ok(None)
        }
        async fn save_memory_l1(&self, _m: &ramaria_core::types::MemoryL1) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_memory_l1(
            &self,
            _id: uuid::Uuid,
        ) -> RamariaResult<Vec<ramaria_core::types::MemoryL1>> {
            Ok(vec![])
        }
        async fn get_memory_l1(
            &self,
            _id: uuid::Uuid,
        ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
            Ok(None)
        }
        async fn mark_l1_absorbed(&self, _ids: &[uuid::Uuid]) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_unabsorbed_l1(
            &self,
            _p: &str,
        ) -> RamariaResult<Vec<ramaria_core::types::MemoryL1>> {
            Ok(vec![])
        }
        async fn create_persona(&self, _p: &ramaria_core::types::Persona) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn get_persona_by_uid(
            &self,
            _u: &str,
        ) -> RamariaResult<Option<ramaria_core::types::Persona>> {
            Ok(None)
        }
        async fn list_personas(&self) -> RamariaResult<Vec<ramaria_core::types::Persona>> {
            Ok(vec![])
        }
        async fn update_persona(
            &self,
            _u: &str,
            _n: &str,
            _a: Option<&str>,
            _c: Option<&str>,
            _d: Option<&str>,
        ) -> RamariaResult<()> {
            Ok(())
        }
        async fn save_event(&self, _e: &ramaria_core::types::MemoryEvent) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_events_by_persona(
            &self,
            _p: &str,
            _o: i64,
            _l: i64,
        ) -> RamariaResult<Vec<ramaria_core::types::MemoryEvent>> {
            Ok(vec![])
        }
        async fn list_unabsorbed_events(
            &self,
            _p: &str,
        ) -> RamariaResult<Vec<ramaria_core::types::MemoryEvent>> {
            Ok(vec![])
        }
        async fn mark_events_absorbed(&self, _ids: &[i64]) -> RamariaResult<()> {
            Ok(())
        }
        async fn save_event_relation(
            &self,
            _r: &ramaria_core::types::EventRelation,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn save_event_source(&self, _e: i64, _l: uuid::Uuid, _w: f64) -> RamariaResult<()> {
            Ok(())
        }
        async fn save_fact(&self, _f: &ramaria_core::types::PersonaFact) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_facts_by_persona(
            &self,
            _p: &str,
            _f: ramaria_core::types::ProfileField,
        ) -> RamariaResult<Vec<ramaria_core::types::PersonaFact>> {
            Ok(vec![])
        }
        async fn save_trait(
            &self,
            _t: &ramaria_core::types::PersonalityTrait,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_traits_by_persona(
            &self,
            _p: &str,
        ) -> RamariaResult<Vec<ramaria_core::types::PersonalityTrait>> {
            Ok(vec![])
        }
        async fn update_trait_confidence(
            &self,
            _id: i64,
            _c: f64,
            _e: f64,
            _s: f64,
        ) -> RamariaResult<()> {
            Ok(())
        }
        async fn update_trait_status(
            &self,
            _id: i64,
            _s: ramaria_core::types::TraitStatus,
        ) -> RamariaResult<()> {
            Ok(())
        }
        async fn save_evidence(
            &self,
            _e: &ramaria_core::types::TraitEvidence,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_evidence_by_trait(
            &self,
            _t: i64,
        ) -> RamariaResult<Vec<ramaria_core::types::TraitEvidence>> {
            Ok(vec![])
        }
        async fn save_example(
            &self,
            _e: &ramaria_core::types::PersonaExample,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_selected_examples(
            &self,
            _p: &str,
        ) -> RamariaResult<Vec<ramaria_core::types::PersonaExample>> {
            Ok(vec![])
        }
        async fn save_cluster_snapshot(
            &self,
            _s: &ramaria_core::types::ClusterSnapshot,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn get_current_snapshots(
            &self,
            _p: &str,
            _c: &str,
        ) -> RamariaResult<Vec<ramaria_core::types::ClusterSnapshot>> {
            Ok(vec![])
        }
        async fn upsert_keyword(&self, _k: &str) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
            Ok(vec![])
        }
        async fn insert_keyword_ref(
            &self,
            _k: &str,
            _d: &str,
            _i: &str,
            _p: &str,
            _w: f64,
        ) -> RamariaResult<()> {
            Ok(())
        }
        async fn find_refs_by_keyword(
            &self,
            _k: &str,
        ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
            Ok(vec![])
        }
        async fn find_refs_by_doc(
            &self,
            _d: &str,
            _i: &str,
        ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
            Ok(vec![])
        }
        async fn save_privacy_consent(
            &self,
            _c: &ramaria_core::types::PrivacyConsent,
        ) -> RamariaResult<()> {
            Ok(())
        }
        async fn get_privacy_consent(
            &self,
            _p: &str,
            _b: &str,
        ) -> RamariaResult<Option<ramaria_core::types::PrivacyConsent>> {
            Ok(None)
        }
        async fn save_backend_config(&self, c: &BackendConfig) -> RamariaResult<()> {
            *self.backend.lock().unwrap() = Some(c.clone());
            Ok(())
        }
        async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
            Ok(self.backend.lock().unwrap().clone())
        }
        async fn get_schema_version(&self) -> RamariaResult<i32> {
            Ok(1)
        }
        async fn get_index_version(&self) -> RamariaResult<i32> {
            Ok(1)
        }
        async fn set_index_version(&self, _v: i32) -> RamariaResult<()> {
            Ok(())
        }
        async fn create_background_job(&self, _t: &str, _p: Option<&str>) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn update_job_status(
            &self,
            _i: i64,
            _s: &str,
            _e: Option<&str>,
        ) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
            Ok(vec![])
        }
        async fn create_conflict(
            &self,
            _f: &str,
            _t: &str,
            _o: Option<&str>,
            _n: Option<&str>,
            _d: Option<&str>,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_pending_conflicts(
            &self,
        ) -> RamariaResult<Vec<(i64, String, String, String)>> {
            Ok(vec![])
        }
        async fn resolve_conflict(&self, _i: i64) -> RamariaResult<()> {
            Ok(())
        }
        async fn create_push(&self, _c: &str) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_pending_pushes(&self) -> RamariaResult<Vec<(i64, String)>> {
            Ok(vec![])
        }
        async fn mark_push_sent(&self, _i: i64) -> RamariaResult<()> {
            Ok(())
        }
        async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> {
            Ok(self.settings.lock().unwrap().get(key).cloned())
        }
        async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> {
            self.settings
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
            Ok(self
                .settings
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
        async fn save_bm25(&self, _d: i64, _l: &str, _t: &str) -> RamariaResult<()> {
            Ok(())
        }
        async fn list_bm25_by_doc(&self, _d: i64) -> RamariaResult<Vec<(String, String)>> {
            Ok(vec![])
        }
        async fn delete_bm25_by_doc(&self, _d: i64) -> RamariaResult<()> {
            Ok(())
        }
        async fn insert_graph_node(
            &self,
            _e: &str,
            _t: &str,
            _l: Option<uuid::Uuid>,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn get_graph_node(&self, _e: &str) -> RamariaResult<Option<(i64, String, String)>> {
            Ok(None)
        }
        async fn insert_graph_edge(
            &self,
            _s: i64,
            _t: i64,
            _r: &str,
            _d: Option<&str>,
            _l: Option<uuid::Uuid>,
        ) -> RamariaResult<i64> {
            Ok(1)
        }
        async fn list_graph_edges(&self, _s: i64) -> RamariaResult<Vec<(i64, i64, i64, String)>> {
            Ok(vec![])
        }
    }

    /// 创建临时目录 + service。
    fn temp_service(storage: Arc<dyn StorageBackend>) -> (ConfigSyncService, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ramaria-config-sync-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        (ConfigSyncService::new(storage, path.clone()), dir)
    }

    #[tokio::test]
    async fn load_missing_file_generates_template() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);
        let outcome = service.load().await.expect("加载不应失败");

        // 文件缺失 → 生成模板 + 默认配置
        assert!(!outcome.file_existed);
        assert!(outcome.file_parse_errors.is_empty());
        assert!(service.config_path().exists(), "应生成模板文件");
        assert!(outcome.config.utt.enabled);
        assert_eq!(outcome.config.utt.theta_gap_minutes, 30);
        assert_eq!(outcome.config.examples.max_examples, 5);
        assert!(outcome.config.bridge.enabled);
        // DB 无差异（均为默认）→ 无 mismatch
        assert!(outcome.mismatches.is_empty());

        // 生成的模板应能反序列化回默认配置
        let text = std::fs::read_to_string(service.config_path()).unwrap();
        let parsed: RamariaConfig = toml::from_str(&text).expect("模板应为合法 TOML");
        assert!(parsed.utt.enabled);
        assert_eq!(parsed.utt.persona_kind_whitelist.len(), 4);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn first_startup_preserves_existing_db_backend() {
        // 关键回归：v1.3 升级首启（无 config.toml）时，DB 中已有用户后端配置，
        // 加载必须以 DB 为准，绝不能以默认值覆盖用户配置。
        let storage = Arc::new(MockStorage::default());
        let bc = BackendConfig::deepseek_default();
        storage.save_backend_config(&bc).await.unwrap();
        // DB 侧还有自定义设置
        storage
            .set_setting("config.utt.theta_gap_minutes", "45")
            .await
            .unwrap();

        let (service, dir) = temp_service(storage.clone());
        let outcome = service.load().await.unwrap();

        // 生效配置 = DB 侧值（不被默认值覆盖）
        assert_eq!(outcome.config.backend.provider, LlmProvider::DeepSeek);
        assert_eq!(outcome.config.backend.model_id, "deepseek-chat");
        assert_eq!(outcome.config.utt.theta_gap_minutes, 45);
        // 首启无 mismatch（不以文件为准回写 DB）
        assert!(outcome.mismatches.is_empty(), "首启不应产生 mismatch");

        // DB 侧未被覆盖（仍为 DeepSeek）
        let db_backend = storage.get_backend_config().await.unwrap().unwrap();
        assert_eq!(db_backend.provider, LlmProvider::DeepSeek);
        assert_eq!(db_backend.capability.model_id, "deepseek-chat");
        // settings 键未被覆盖
        let v = storage
            .get_setting("config.utt.theta_gap_minutes")
            .await
            .unwrap();
        assert_eq!(v.as_deref(), Some("45"));

        // 生成的文件应包含 DB 侧值（下次启动以文件为准时不会漂移）
        let text = std::fs::read_to_string(service.config_path()).unwrap();
        let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
        assert_eq!(file_cfg.backend.provider, LlmProvider::DeepSeek);
        assert_eq!(file_cfg.utt.theta_gap_minutes, 45);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn corrupted_file_does_not_overwrite_db() {
        // 文件损坏 → 以 DB 为准合并，不向 DB 回写默认值（防止覆盖用户配置）
        let storage = Arc::new(MockStorage::default());
        let bc = BackendConfig::openai_default();
        storage.save_backend_config(&bc).await.unwrap();

        let (service, dir) = temp_service(storage.clone());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&service.config_path, "损坏的 [[[ 配置").unwrap();

        let outcome = service.load().await.unwrap();
        assert_eq!(outcome.file_parse_errors.len(), 1);
        // 生效配置以 DB 为准
        assert_eq!(outcome.config.backend.provider, LlmProvider::OpenAI);
        // 不写回 DB（DB 仍是 OpenAI）
        let db_backend = storage.get_backend_config().await.unwrap().unwrap();
        assert_eq!(db_backend.provider, LlmProvider::OpenAI);
        // 损坏文件不被覆盖（保留现场）
        let text = std::fs::read_to_string(service.config_path()).unwrap();
        assert!(text.contains("损坏"), "损坏文件应原样保留");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn load_file_wins_over_db_mismatch() {
        // 文件已存在且 DB 侧值不同 → 检测 mismatch 并回写 DB（以文件为准）
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());

        // 先首启生成文件（默认值 theta=30）
        service.load().await.unwrap();

        // 模拟外部修改 DB 侧值（如旧版本直写 settings 表）
        storage
            .set_setting("config.utt.theta_gap_minutes", "60")
            .await
            .unwrap();

        // 再次加载：文件（30）vs DB（60）→ mismatch → 以文件为准写回 DB
        let outcome = service.load().await.unwrap();
        assert!(
            outcome
                .mismatches
                .iter()
                .any(|m| m.key == "config.utt.theta_gap_minutes"),
            "应检出 theta_gap_minutes 不一致: {:?}",
            outcome.mismatches
        );

        // DB 被回写为文件值 30
        let db_val = storage
            .get_setting("config.utt.theta_gap_minutes")
            .await
            .unwrap();
        assert_eq!(db_val.as_deref(), Some("30"), "DB 应以文件为准回写");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn load_merges_db_backend_config() {
        // 首启（无 config.toml）且 backend_config 表有值 → 以 DB 为准合并
        let storage = Arc::new(MockStorage::default());
        let bc = BackendConfig::deepseek_default();
        storage.save_backend_config(&bc).await.unwrap();
        let (service, dir) = temp_service(storage);

        let outcome = service.load().await.unwrap();
        assert_eq!(outcome.config.backend.provider, LlmProvider::DeepSeek);
        assert_eq!(outcome.config.backend.model_id, "deepseek-chat");
        // 首启以 DB 为准：不产生 mismatch、不覆盖 DB
        assert!(
            outcome.mismatches.is_empty(),
            "首启场景不应检出 mismatch: {:?}",
            outcome.mismatches
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn load_parse_error_falls_back_to_defaults() {
        // 文件损坏 → 解析失败回退默认值，不阻塞
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&service.config_path, "这不是合法的 TOML [[[").unwrap();

        let outcome = service.load().await.unwrap();
        assert_eq!(outcome.file_parse_errors.len(), 1, "应记录解析错误");
        assert!(outcome.config.utt.enabled, "应回退默认配置");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn save_config_writes_both_sides() {
        // 统一写入口：文件 + settings + backend_config 三处一致
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());

        let mut cfg = RamariaConfig::default();
        cfg.utt.theta_gap_minutes = 45;
        cfg.utt.enabled = false;
        cfg.examples.max_examples = 3;
        cfg.backend.provider = LlmProvider::DeepSeek;
        cfg.backend.model_id = "deepseek-chat".to_string();

        let result = service.save_config(&cfg).await;
        assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

        // 文件侧
        let text = std::fs::read_to_string(service.config_path()).unwrap();
        let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
        assert_eq!(file_cfg.utt.theta_gap_minutes, 45);
        assert!(!file_cfg.utt.enabled);
        assert_eq!(file_cfg.examples.max_examples, 3);
        assert_eq!(file_cfg.backend.provider, LlmProvider::DeepSeek);

        // settings 表侧
        let v = storage
            .get_setting("config.utt.theta_gap_minutes")
            .await
            .unwrap();
        assert_eq!(v.as_deref(), Some("45"));
        let v = storage
            .get_setting("config.examples.max_examples")
            .await
            .unwrap();
        assert_eq!(v.as_deref(), Some("3"));
        // 数组（白名单）以 JSON 存储
        let v = storage
            .get_setting("config.utt.persona_kind_whitelist")
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&v.unwrap()).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 4);

        // backend_config 表侧
        let bc = storage.get_backend_config().await.unwrap().unwrap();
        assert_eq!(bc.provider, LlmProvider::DeepSeek);
        assert_eq!(bc.capability.model_id, "deepseek-chat");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn save_then_load_roundtrip() {
        // 热更新语义：save 后 load 应读到相同值
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);

        let mut cfg = RamariaConfig::default();
        cfg.utt.theta_gap_minutes = 55;
        cfg.bridge.enabled = false;
        cfg.session.l1_idle_minutes = 25;
        service.save_config(&cfg).await;

        let outcome = service.load().await.unwrap();
        assert_eq!(outcome.config.utt.theta_gap_minutes, 55);
        assert!(!outcome.config.bridge.enabled);
        assert_eq!(outcome.config.session.l1_idle_minutes, 25);
        assert!(
            outcome.mismatches.is_empty(),
            "save 后 load 不应再有 mismatch: {:?}",
            outcome.mismatches
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn save_preserves_existing_backend_embedding_path() {
        // 写回 backend_config 表时保留既有 embedding_model_path / capability
        let storage = Arc::new(MockStorage::default());
        let mut existing = BackendConfig::lm_studio_default();
        existing.embedding_model_path = Some("D:/models/bge".to_string());
        storage.save_backend_config(&existing).await.unwrap();
        let (service, dir) = temp_service(storage.clone());

        let cfg = RamariaConfig::default(); // 文件侧 LM Studio
        let result = service.save_config(&cfg).await;
        assert!(result.is_ok());

        let bc = storage.get_backend_config().await.unwrap().unwrap();
        assert_eq!(
            bc.embedding_model_path.as_deref(),
            Some("D:/models/bge"),
            "embedding_model_path 不应被覆盖丢失"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn flat_map_roundtrip_preserves_types() {
        // 扁平化 → 合并回配置：类型保持一致（数字/布尔/数组/字符串）
        let cfg = RamariaConfig::default();
        let flat = config_to_flat_map(&cfg);

        // 关键键存在
        assert!(flat.contains_key("utt.theta_gap_minutes"));
        assert!(flat.contains_key("utt.enabled"));
        assert!(flat.contains_key("utt.persona_kind_whitelist"));
        assert!(flat.contains_key("session.l1_idle_minutes"));
        // 跳过组不在扁平 map 中
        assert!(!flat.contains_key("version"));
        assert!(!flat.contains_key("paths.data_dir"));
        assert!(!flat.contains_key("backend.provider"));

        let back = flat_map_to_config(&flat, &RamariaConfig::default()).unwrap();
        assert_eq!(back.utt.theta_gap_minutes, cfg.utt.theta_gap_minutes);
        assert_eq!(back.utt.enabled, cfg.utt.enabled);
        assert_eq!(
            back.utt.persona_kind_whitelist,
            cfg.utt.persona_kind_whitelist
        );
        assert_eq!(back.session.l1_idle_minutes, cfg.session.l1_idle_minutes);
        // backend 组未被扁平 map 覆盖 → 保持基础值
        assert_eq!(back.backend.provider, cfg.backend.provider);
    }

    #[tokio::test]
    async fn flat_map_ignores_unknown_keys() {
        // 未知键（未来版本）合并时忽略，不报错
        let mut flat: BTreeMap<String, JsonValue> = BTreeMap::new();
        flat.insert("utt.theta_gap_minutes".to_string(), JsonValue::from(42));
        flat.insert("future.key".to_string(), JsonValue::from("x"));

        let back = flat_map_to_config(&flat, &RamariaConfig::default()).unwrap();
        assert_eq!(back.utt.theta_gap_minutes, 42);
        assert!(back.utt.enabled, "未涉及的键保持默认");
    }

    #[test]
    fn whitelist_serializes_to_settings() {
        // 白名单数组 → settings 文本 → 读回可还原
        let cfg = RamariaConfig::default();
        let flat = config_to_flat_map(&cfg);
        let v = flat.get("utt.persona_kind_whitelist").unwrap();
        let text = json_value_to_setting(v);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let kinds: Vec<PersonaKind> = serde_json::from_value(parsed).unwrap();
        assert_eq!(kinds.len(), 4);
        assert!(kinds.contains(&PersonaKind::Char));
        assert!(!kinds.contains(&PersonaKind::Rama));
    }

    #[tokio::test]
    async fn sync_backend_config_updates_file_only() {
        // 既有 update_backend_config 通道：写表后同步文件
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());

        // 先有文件配置（默认）
        service.load().await.unwrap();

        let bc = BackendConfig::deepseek_default();
        let result = service.sync_backend_config(&bc).await;
        assert!(result.is_ok());

        let text = std::fs::read_to_string(service.config_path()).unwrap();
        let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
        assert_eq!(file_cfg.backend.provider, LlmProvider::DeepSeek);
        assert_eq!(file_cfg.backend.model_id, "deepseek-chat");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn sync_backend_config_corrupted_file_is_not_overwritten() {
        // 文件损坏时 sync_backend_config 必须拒绝覆盖（保留现场，防止毁掉用户文件）
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&service.config_path, "损坏的 [[[ 配置").unwrap();

        let bc = BackendConfig::deepseek_default();
        let result = service.sync_backend_config(&bc).await;
        assert!(!result.file_ok, "损坏文件应拒绝同步");
        assert!(
            !result.failures.is_empty(),
            "应返回失败明细: {:?}",
            result.failures
        );

        // 文件原样保留（未被默认值或 merged 覆盖）
        let text = std::fs::read_to_string(service.config_path()).unwrap();
        assert_eq!(text, "损坏的 [[[ 配置", "损坏文件应原样保留");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn default_template_contains_v14_groups() {
        // 模板文件包含 v1.4 配置组
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);
        service.load().await.unwrap();

        let text = std::fs::read_to_string(service.config_path()).unwrap();
        assert!(text.contains("[utt]"), "模板应含 [utt]");
        assert!(text.contains("theta_gap_minutes"));
        assert!(text.contains("[examples]"));
        assert!(text.contains("max_examples"));
        assert!(text.contains("[bridge]"));
        assert!(text.contains("persona_kind_whitelist"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn first_startup_empty_db_keeps_commented_template() {
        // 首启且 DB 为空 → 文件内容 = 带注释的默认模板（保留说明注释）
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);
        service.load().await.unwrap();

        let text = std::fs::read_to_string(service.config_path()).unwrap();
        assert_eq!(
            text, DEFAULT_CONFIG_TEMPLATE,
            "DB 为空时首启应保留带注释的模板原文"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
