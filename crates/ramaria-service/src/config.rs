//! crates/ramaria-service/src/config.rs - Ramaria 配置双写用例模块
//!
//! 设计特点:
//! - 统一配置用例：config.toml（canonical）↔ `backend_config` 表 / `settings` 表 双写同步
//! - 加载：读取两处并做一致性校验，不一致以文件为准并告警（写回 DB 侧）
//! - config.toml 缺失时生成含全部默认值的模板文件（打包 `config/default.toml`）
//! - 统一写入口（`save_config`）同时落文件与表，单侧写失败降级不阻塞
//! - settings 表使用 `config.*` 前缀的扁平键；`backend` 组映射 `backend_config` 表
//! - paths/version/schema_version 为环境相关元数据，不参与双写（仅文件侧）
//!
//! 安全约束:
//! - 本模块不记录完整配置内容，日志仅记录差异键名与失败上下文
//! - API key 不进本模块任何写入路径（密钥始终由 OS keychain 管理）
//! - 后端配置写回时保留既有 capability / embedding_model_path，避免覆盖丢失

use std::collections::{BTreeMap, BTreeSet};
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

/// 原子写入使用的临时文件后缀（写完后 `fs::rename` 替换目标）。
const CONFIG_TEMP_SUFFIX: &str = ".part";

/// 文件显式声明的键集（用于"以文件为准"的同步 diff）。
///
/// 背景:
/// - `RamariaConfig` 反序列化会为缺失键填入默认值；若按"反序列化后的全键集"
///   回写 DB，文件未声明的新键默认值会覆盖 DB 中用户显式设置的值。
/// - 因此同步只认"文件里真实写了"的键。
#[derive(Debug, Default, Clone)]
struct ExplicitFileKeys {
    /// 点分键集合（跳过 version / schema_version / paths / backend）
    flat: BTreeSet<String>,
    /// 文件是否显式声明了 `[backend]` 组（未声明 → 后端组不参与回写）
    has_backend_group: bool,
}

// =========================================================
// 结果类型
// =========================================================

/// 单条不一致记录（一致性校验时以文件为准）。
#[derive(Debug, Clone)]
pub struct MismatchEntry {
    /// 配置键（如 `utt.theta_gap_minutes`、`backend.provider`）
    pub key: String,
    /// 文件侧值（canonical）
    pub file_value: String,
    /// DB 侧值
    pub db_value: String,
}

/// 加载 + 一致性校验结果。
#[derive(Debug)]
pub struct SyncOutcome {
    /// 合并后的生效配置（正常路径以文件为准；首启/损坏路径以 DB 为准）
    pub config: RamariaConfig,
    /// 配置文件是否存在（false 表示本次自动生成了文件：DB 非空时为 merged，DB 空时为模板）
    pub file_existed: bool,
    /// 文件解析错误（解析失败时回退默认值，不阻塞）
    pub file_parse_errors: Vec<String>,
    /// 不一致项（已按文件为准写回 DB 侧）
    pub mismatches: Vec<MismatchEntry>,
    /// DB 侧写回失败（降级不阻塞，日志告警）
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
// 配置双写用例
// =========================================================

/// 配置双写用例。
///
/// 职责:
/// - 加载 config.toml + DB 两侧配置，一致性校验（文件为准）并回写 DB。
/// - 提供统一写入口，使文件与表永不单侧漂移。
///
/// 用法:
/// ```ignore
/// let writer = ConfigWriter::new(storage, config_dir.join("config.toml"));
/// let outcome = writer.load().await?;   // 加载链路
/// writer.save_config(&new_cfg).await;   // 设置页修改（统一写入口）
/// ```
///
/// 降级语义:
/// - 文件缺失 → 生成默认模板（不视为错误）。
/// - 文件解析失败 → 回退默认值并以 DB 侧继续，记 warn。
/// - 单侧写失败 → 另一侧仍生效，记 warn 不抛错。
pub struct ConfigWriter {
    storage: Arc<dyn StorageBackend>,
    config_path: PathBuf,
}

impl ConfigWriter {
    /// 创建配置双写用例。
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

    /// 加载配置：读取两侧配置 → 一致性校验（文件为准）→ 回写 DB 侧。
    ///
    /// 流程:
    /// 1. 读取 config.toml（缺失则生成含全部默认值的模板；解析失败回退默认值）。
    /// 2. 读取 backend_config 表与 settings 表（`config.*` 键）作为 DB 侧配置。
    /// 3. 逐键对比，不一致记录 mismatch（以文件为准）。
    /// 4. 将文件侧配置写回 DB（backend_config 表 + settings 表），单侧失败降级。
    ///
    /// 首启/损坏语义（防止覆盖用户已有配置）:
    /// - 文件缺失（首次启动无 config.toml）：以 **DB 为准** 合并生效配置，
    ///   并把 DB 侧值写入生成的文件，**不向 DB 回写**。
    /// - 文件解析失败：回退默认值 + DB 侧合并（DB 为准），**不向 DB 回写**。
    ///
    /// 返回:
    /// - `SyncOutcome`：生效配置与校验结果（调用方记录日志）。
    pub async fn load(&self) -> RamariaResult<SyncOutcome> {
        // ---- Step 1: 读取文件侧（含"文件显式声明的键集"，供同步 diff 使用）----
        let mut file_parse_errors = Vec::new();
        let mut file_existed = true;
        let mut explicit_keys = ExplicitFileKeys::default();
        let file_config: RamariaConfig = if !self.config_path.exists() {
            // 文件缺失：不在此处写模板（由下方首启分支统一决策写入，
            // 避免"模板写成功 + merged 写失败"留下合法模板导致下次启动
            // 以默认值覆盖 DB 用户配置的中间态）。
            file_existed = false;
            RamariaConfig::default()
        } else {
            match self.read_file_config_with_keys() {
                Ok((cfg, keys)) => {
                    explicit_keys = keys;
                    cfg
                }
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
            .sync_db_to_file(&file_config, &explicit_keys, &db_flat, db_backend.as_ref())
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
    /// - 设置页回显：反映运行时实际生效值且无写副作用。
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

    /// 后端配置同步（供后端配置更新用例复用，保持表 / 文件一致）。
    ///
    /// 参数:
    /// - `backend`: 已写入 backend_config 表的新后端配置。
    ///
    /// 说明:
    /// - 读取当前 config.toml（缺失则生成模板），仅更新 `[backend]` 组后写回文件。
    /// - 保留文件侧 `online_memory_injection` 等 DB 无对应字段的值。
    /// - `db_ok` 恒为 true：本方法只同步文件侧，DB 侧已由调用方写入
    ///   （调用方先写表再调用本方法）。
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
        self.read_file_config_with_keys().map(|(cfg, _)| cfg)
    }

    /// 读取并解析 config.toml，同时提取"文件显式声明的键集"。
    ///
    /// 返回:
    /// - `(配置, 显式键集)`；键集用于同步 diff（只回写文件里真实写了的键）。
    fn read_file_config_with_keys(&self) -> RamariaResult<(RamariaConfig, ExplicitFileKeys)> {
        let text = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ramaria_core::error::RamariaError::io(
                format!("读取 config.toml 失败: {}", self.config_path.display()),
                Some(e),
            )
        })?;
        let cfg = toml::from_str(&text).map_err(|e| {
            ramaria_core::error::RamariaError::config(format!("解析 config.toml 失败: {e}"))
        })?;
        Ok((cfg, parse_explicit_file_keys(&text)))
    }

    /// 将配置写为 config.toml（保留文件头注释与未知键，原子替换）。
    fn write_file_config(&self, cfg: &RamariaConfig) -> RamariaResult<()> {
        let text = self.render_config_text(cfg)?;
        atomic_write(&self.config_path, &text)
    }

    /// 渲染 config.toml 文本：序列化当前配置，并尽量保留用户文件中的既有内容。
    ///
    /// 保留策略（无 TOML 编辑器依赖下的保守合并）:
    /// - 文件头注释块（开头的注释与空行）原样保留；
    /// - 旧文件中"当前 schema 未知"的键（含未知分组）原样保留；
    /// - 其余键以当前配置为准（分组内部注释不保留）。
    fn render_config_text(&self, cfg: &RamariaConfig) -> RamariaResult<String> {
        let mut root = toml::Value::try_from(cfg).map_err(|e| {
            ramaria_core::error::RamariaError::config(format!("配置转换为 TOML 值失败: {e}"))
        })?;

        let mut header = String::new();
        if let Ok(old_text) = std::fs::read_to_string(&self.config_path) {
            header = leading_comment_block(&old_text);
            if let Ok(toml::Value::Table(old_table)) = old_text.parse::<toml::Value>() {
                if let Some(new_table) = root.as_table_mut() {
                    merge_unknown_keys(new_table, &old_table);
                }
            }
        }

        let body = toml::to_string_pretty(&root).map_err(|e| {
            ramaria_core::error::RamariaError::config(format!("序列化配置为 TOML 失败: {e}"))
        })?;
        if header.is_empty() {
            Ok(body)
        } else {
            Ok(format!("{header}{body}"))
        }
    }

    /// 生成默认模板文件（config.toml 缺失时调用，原子替换）。
    fn write_template_file(&self) -> RamariaResult<()> {
        atomic_write(&self.config_path, DEFAULT_CONFIG_TEMPLATE)
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
    /// - `explicit_keys`: 文件**显式声明**的键集（缺失键不参与回写）。
    /// - `db_flat`: DB 侧 settings 真实键集。
    /// - `db_backend`: DB 侧 backend_config 表内容。
    ///
    /// 返回:
    /// - `(mismatches, db_write_failures)`：不一致明细与写失败明细。
    async fn sync_db_to_file(
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
// 文件侧工具：显式键集 / 合并保留 / 原子写入
// =========================================================

/// 从原始 TOML 文本提取"文件显式声明的键集"。
///
/// 说明:
/// - 空文件/解析失败 → 返回空键集（调用方在解析失败路径已回退默认配置，不会误回写）；
/// - 跳过 version / schema_version / paths 组（与 `SKIP_FLAT_KEYS` 一致）；
/// - `[backend]` 组单独标记（键集里不含点分键，回写策略见 `sync_db_to_file`）。
fn parse_explicit_file_keys(text: &str) -> ExplicitFileKeys {
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
fn leading_comment_block(text: &str) -> String {
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
fn merge_unknown_keys(new_table: &mut toml::value::Table, old_table: &toml::value::Table) {
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

/// 原子写入文本文件：先写同目录 `{文件名}.part`，再 `fs::rename` 替换目标。
///
/// 说明:
/// - 同目录临时文件保证 rename 在同一文件系统内、可原子覆盖旧文件；
/// - 任一步失败都会清理临时文件，绝不留下半截目标文件（旧文件保持原样）。
fn atomic_write(path: &Path, content: &str) -> RamariaResult<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ramaria_core::error::RamariaError::io(
                    format!("创建配置目录失败: {}", parent.display()),
                    Some(e),
                )
            })?;
        }
    }

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            ramaria_core::error::RamariaError::io(
                format!("配置路径缺少文件名: {}", path.display()),
                None,
            )
        })?;
    let temp_path = path.with_file_name(format!("{file_name}{CONFIG_TEMP_SUFFIX}"));

    let result = std::fs::write(&temp_path, content)
        .map_err(|e| {
            ramaria_core::error::RamariaError::io(
                format!("写入临时配置文件失败: {}", temp_path.display()),
                Some(e),
            )
        })
        .and_then(|()| {
            std::fs::rename(&temp_path, path).map_err(|e| {
                ramaria_core::error::RamariaError::io(
                    format!("原子替换配置文件失败: {}", path.display()),
                    Some(e),
                )
            })
        });

    if result.is_err() {
        // 失败路径清理可能残留的临时文件（不覆盖原始错误）
        let _ = std::fs::remove_file(&temp_path);
    }
    result
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
    use ramaria_core::traits::StoreInfrastructure;
    use ramaria_core::types::{LlmProvider, PersonaKind};
    use std::sync::Mutex;

    /// 最小 mock storage：仅实现本模块用到的 settings / backend_config 方法。
    #[derive(Default)]
    struct MockStorage {
        settings: Mutex<BTreeMap<String, String>>,
        backend: Mutex<Option<BackendConfig>>,
    }

    #[async_trait::async_trait]
    impl ramaria_core::traits::StoreCrud for MockStorage {
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
    }

    #[async_trait::async_trait]
    impl ramaria_core::traits::StoreInfrastructure for MockStorage {
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
    }

    /// 创建临时目录 + 用例实例。
    fn temp_service(storage: Arc<dyn StorageBackend>) -> (ConfigWriter, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "ramaria-config-writer-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("config.toml");
        (ConfigWriter::new(storage, path.clone()), dir)
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
        assert_eq!(outcome.config.utt.theta_gap_minutes, 10);
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
        // 文件缺失（无 config.toml）且 DB 中已有用户后端配置时，
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

        // 生成的文件应包含 DB 侧值（下次加载以文件为准时不会漂移）
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

        // 先首启生成文件（默认值 theta=10）
        service.load().await.unwrap();

        // 模拟外部修改 DB 侧值（绕过统一写入口直写 settings 表）
        storage
            .set_setting("config.utt.theta_gap_minutes", "60")
            .await
            .unwrap();

        // 再次加载：文件（10）vs DB（60）→ mismatch → 以文件为准写回 DB
        let outcome = service.load().await.unwrap();
        assert!(
            outcome
                .mismatches
                .iter()
                .any(|m| m.key == "config.utt.theta_gap_minutes"),
            "应检出 theta_gap_minutes 不一致: {:?}",
            outcome.mismatches
        );

        // DB 被回写为文件值 10
        let db_val = storage
            .get_setting("config.utt.theta_gap_minutes")
            .await
            .unwrap();
        assert_eq!(db_val.as_deref(), Some("10"), "DB 应以文件为准回写");

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

    /// 历史窗口键纳入双写与逐键比对：save 后 settings 表可见、文件侧携带，加载读回一致。
    #[tokio::test]
    async fn max_history_chars_is_covered_by_full_sync() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());

        let mut cfg = RamariaConfig::default();
        cfg.session.max_history_chars = 9000;
        let result = service.save_config(&cfg).await;
        assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

        // settings 表：键存在且值一致
        let stored = storage
            .get_setting("config.session.max_history_chars")
            .await
            .unwrap();
        assert_eq!(stored.as_deref(), Some("9000"), "键应写入 settings");

        // 文件侧：[session] 组含该键且值一致
        let text = std::fs::read_to_string(service.config_path()).unwrap();
        assert!(
            text.contains("max_history_chars"),
            "文件应携带该键:\n{text}"
        );
        let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
        assert_eq!(file_cfg.session.max_history_chars, 9000);

        // 加载读回一致（双写完成后无不一致项）
        let outcome = service.load().await.unwrap();
        assert_eq!(outcome.config.session.max_history_chars, 9000);
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
        assert!(flat.contains_key("session.max_history_chars"));
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
        assert_eq!(
            back.session.max_history_chars,
            cfg.session.max_history_chars
        );
        // backend 组未被扁平 map 覆盖 → 保持基础值
        assert_eq!(back.backend.provider, cfg.backend.provider);
    }

    #[tokio::test]
    async fn flat_map_ignores_unknown_keys() {
        // 未知键（当前 schema 不存在）合并时忽略，不报错
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
        // 后端配置更新用例：写表后同步文件（仅文件侧）
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
    async fn default_template_contains_expected_groups() {
        // 模板文件包含主要配置组（[utt] / [examples] / [bridge]）
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

    /// 默认配置模板必须与 `RamariaConfig::default()` 逐键一致。
    ///
    /// 背景:
    /// - 首启生成的 config.toml 直接复制本模板（`DEFAULT_CONFIG_TEMPLATE`），
    ///   模板缺段会让用户无法从文件发现新增配置开关（静默取结构默认）。
    /// - 本用例按扁平键（跳过 version/schema_version/paths/backend）逐键比对
    ///   模板解析值与结构默认值；新增配置组时必须同步模板。
    #[test]
    fn default_template_matches_config_defaults_key_by_key() {
        let template: RamariaConfig =
            toml::from_str(DEFAULT_CONFIG_TEMPLATE).expect("模板应为合法 TOML");
        let template_flat = config_to_flat_map(&template);
        let default_flat = config_to_flat_map(&RamariaConfig::default());

        let missing: Vec<&String> = default_flat
            .keys()
            .filter(|key| !template_flat.contains_key(*key))
            .collect();
        assert!(missing.is_empty(), "模板缺少默认配置键: {missing:?}");

        let unknown: Vec<&String> = template_flat
            .keys()
            .filter(|key| !default_flat.contains_key(*key))
            .collect();
        assert!(
            unknown.is_empty(),
            "模板含当前 schema 不存在的键: {unknown:?}"
        );

        let drifted: Vec<String> = default_flat
            .iter()
            .filter_map(|(key, default_value)| {
                let template_value = template_flat.get(key)?;
                (template_value != default_value)
                    .then(|| format!("{key}: 模板={template_value} 默认={default_value}"))
            })
            .collect();
        assert!(
            drifted.is_empty(),
            "模板默认值与结构默认值不一致: {drifted:?}"
        );
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

    // =========================================================
    // 同步 diff 只认"文件显式声明的键"（文件缺键不覆盖 DB 显式值）
    // =========================================================

    /// 文件缺键时，DB 中用户显式设置不得被文件默认值覆盖。
    #[tokio::test]
    async fn load_does_not_overwrite_db_keys_absent_from_file() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());
        std::fs::create_dir_all(&dir).unwrap();

        // 文件只显式声明 utt.theta_gap_minutes；其余键走反序列化默认值
        std::fs::write(&service.config_path, "[utt]\ntheta_gap_minutes = 10\n").unwrap();

        // DB 侧：theta 与文件不同（文件为准）；examples.max_examples 文件未声明（必须保留）
        storage
            .set_setting("config.utt.theta_gap_minutes", "45")
            .await
            .unwrap();
        storage
            .set_setting("config.examples.max_examples", "3")
            .await
            .unwrap();
        // DB 后端为 DeepSeek，文件无 [backend] → 后端配置必须保留（不被默认 LM Studio 覆盖）
        let bc = BackendConfig::deepseek_default();
        storage.save_backend_config(&bc).await.unwrap();

        let outcome = service.load().await.unwrap();

        // 文件显式键：以文件为准回写 DB，并记入 mismatch
        let theta = storage
            .get_setting("config.utt.theta_gap_minutes")
            .await
            .unwrap();
        assert_eq!(theta.as_deref(), Some("10"), "文件显式键应以文件为准");
        assert!(
            outcome
                .mismatches
                .iter()
                .any(|m| m.key == "config.utt.theta_gap_minutes"),
            "文件显式键不一致应记入 mismatch"
        );

        // 文件未声明的键：DB 显式值保留（默认值为 5，不得覆盖）
        let max_examples = storage
            .get_setting("config.examples.max_examples")
            .await
            .unwrap();
        assert_eq!(
            max_examples.as_deref(),
            Some("3"),
            "文件未声明的键不得被默认值覆盖"
        );

        // 文件未声明 [backend] → DB 后端配置保留
        let db_backend = storage.get_backend_config().await.unwrap().unwrap();
        assert_eq!(
            db_backend.provider,
            LlmProvider::DeepSeek,
            "文件未声明 [backend] 时后端配置应保留"
        );

        // 生效配置以文件为准
        assert_eq!(outcome.config.utt.theta_gap_minutes, 10);

        let _ = std::fs::remove_dir_all(dir);
    }

    // =========================================================
    // [mcp] 组纳入双写同步（MCP 接入配置通道）
    // =========================================================

    /// [mcp] 组七键纳入统一写入口：save 后 settings 表逐键可见、文件侧携带其值。
    #[tokio::test]
    async fn mcp_group_keys_are_covered_by_full_sync() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());

        let mut cfg = RamariaConfig::default();
        cfg.mcp.enabled = true;
        cfg.mcp.allow_ingest = false;
        cfg.mcp.allow_seal = false;
        cfg.mcp.allowed_personas = vec!["rama-0001".to_string()];
        cfg.mcp.allow_raw_text = true;
        cfg.mcp.max_items = 8;
        cfg.mcp.max_chars = 999;

        let result = service.save_config(&cfg).await;
        assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

        // settings 表侧：标量键逐键断言（新增配置组必须自动纳入扁平化覆盖）
        for (key, want) in [
            ("config.mcp.enabled", "true"),
            ("config.mcp.allow_ingest", "false"),
            ("config.mcp.allow_seal", "false"),
            ("config.mcp.allow_raw_text", "true"),
            ("config.mcp.max_items", "8"),
            ("config.mcp.max_chars", "999"),
        ] {
            let got = storage.get_setting(key).await.unwrap();
            assert_eq!(got.as_deref(), Some(want), "键 {key} 应写入 settings");
        }
        // 白名单数组以 JSON 文本存储
        let personas = storage
            .get_setting("config.mcp.allowed_personas")
            .await
            .unwrap()
            .expect("白名单键应写入 settings");
        let parsed: JsonValue = serde_json::from_str(&personas).unwrap();
        assert_eq!(parsed, serde_json::json!(["rama-0001"]));

        // 文件侧：完整序列化应携带 [mcp] 全组
        let text = std::fs::read_to_string(service.config_path()).unwrap();
        let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
        assert!(file_cfg.mcp.enabled);
        assert!(!file_cfg.mcp.allow_seal);
        assert_eq!(file_cfg.mcp.max_items, 8);
        assert_eq!(file_cfg.mcp.allowed_personas, vec!["rama-0001".to_string()]);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 文件与 DB 的 [mcp] 键不一致：以文件为准回写 DB 并记入 mismatch（既有不一致处理口径）。
    #[tokio::test]
    async fn mcp_mismatch_is_resolved_by_file_side() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());
        std::fs::create_dir_all(&dir).unwrap();

        // 文件显式声明 [mcp].enabled = true（其余键走反序列化默认值）
        std::fs::write(&service.config_path, "[mcp]\nenabled = true\n").unwrap();
        // DB 侧为相反残值 → 应被文件回写
        storage
            .set_setting("config.mcp.enabled", "false")
            .await
            .unwrap();

        let outcome = service.load().await.unwrap();

        let enabled = storage.get_setting("config.mcp.enabled").await.unwrap();
        assert_eq!(
            enabled.as_deref(),
            Some("true"),
            "文件显式键应以文件为准回写 DB"
        );
        assert!(
            outcome
                .mismatches
                .iter()
                .any(|m| m.key == "config.mcp.enabled"),
            "不一致应记入 mismatch: {:?}",
            outcome.mismatches
        );
        assert!(outcome.config.mcp.enabled, "生效配置应取文件侧值");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 文件未声明的 [mcp] 键：DB 显式值保留（不被模板默认值覆盖）。
    #[tokio::test]
    async fn mcp_db_keys_absent_from_file_are_preserved() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage.clone());
        std::fs::create_dir_all(&dir).unwrap();

        // 文件只声明 [mcp].enabled（其余键走反序列化默认值）
        std::fs::write(&service.config_path, "[mcp]\nenabled = true\n").unwrap();
        // DB 侧用户显式设置：max_items=9（文件未声明 → 不得被默认值 5 覆盖）
        storage
            .set_setting("config.mcp.max_items", "9")
            .await
            .unwrap();

        service.load().await.unwrap();

        let max_items = storage.get_setting("config.mcp.max_items").await.unwrap();
        assert_eq!(
            max_items.as_deref(),
            Some("9"),
            "文件未声明的键不得被默认值覆盖"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 保存配置时保留文件头注释与未知键（全量序列化不丢用户手写内容）。
    #[tokio::test]
    async fn save_config_preserves_header_comments_and_unknown_keys() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &service.config_path,
            "# 用户手写说明：不要删我\n# 第二行注释\n\n\
             [utt]\ntheta_gap_minutes = 10\nfuture_key = \"keep-me\"\n\n\
             [future_group]\nx = 1\n",
        )
        .unwrap();

        let mut cfg = RamariaConfig::default();
        cfg.utt.theta_gap_minutes = 25;
        let result = service.save_config(&cfg).await;
        assert!(result.file_ok, "文件写入应成功: {:?}", result.failures);

        let text = std::fs::read_to_string(service.config_path()).unwrap();
        assert!(
            text.starts_with("# 用户手写说明：不要删我\n# 第二行注释\n"),
            "文件头注释应保留:\n{text}"
        );
        assert!(
            text.contains("future_key = \"keep-me\""),
            "未知键应保留:\n{text}"
        );
        assert!(text.contains("[future_group]"), "未知分组应保留:\n{text}");
        assert!(
            !dir.join("config.toml.part").exists(),
            "成功后不应残留临时文件"
        );

        // 合并结果仍是合法 TOML 且写入值生效
        let parsed: RamariaConfig = toml::from_str(&text).expect("合并后应为合法 TOML");
        assert_eq!(parsed.utt.theta_gap_minutes, 25);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 原子写入失败（目标被目录占位）→ 返回错误、无 `.part` 残留、原目标保持原样。
    #[tokio::test]
    async fn write_file_config_failure_leaves_no_part_file() {
        let storage = Arc::new(MockStorage::default());
        let (service, dir) = temp_service(storage);
        std::fs::create_dir_all(service.config_path()).unwrap(); // 目标位置放目录 → rename 必失败

        let err = service
            .write_file_config(&RamariaConfig::default())
            .expect_err("目标为目录时写入应失败");
        assert!(!err.to_string().is_empty(), "错误信息不应为空");
        assert!(
            !dir.join("config.toml.part").exists(),
            "失败后不应残留 .part 临时文件"
        );
        assert!(service.config_path().is_dir(), "原目标应保持不变");

        let _ = std::fs::remove_dir_all(dir);
    }
}
