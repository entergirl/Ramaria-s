//! crates/ramaria-service/src/config/mod.rs - Ramaria 配置双写用例模块
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
//!
//! 模块划分:
//! - `results`：用例结果类型（不一致明细 / 加载结果 / 写入结果）；
//! - `flatten`：配置 ↔ 扁平键值（settings 表 `config.*` 文本编解码）；
//! - `merge`：合并保留（文件头注释 / 未知键）与"文件显式声明键集"提取；
//! - `file_io`：config.toml 文件侧 I/O（读取 / 渲染 / 原子写 / 模板生成）；
//! - `db_io`：DB 侧 I/O 与以文件为准的同步写回（settings / backend_config 表）；
//! - `backend_map`：BackendSelection ↔ BackendConfig 映射与业务字段比较。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::BackendConfig;

use self::backend_map::backend_selection_from_backend_config;
use self::file_io::path_log_label;
use self::merge::merge_db_into_file;

mod backend_map;
mod db_io;
mod file_io;
mod flatten;
mod merge;
mod results;

pub use results::{MismatchEntry, SyncOutcome, SyncWriteResult};

/// settings 表受管键前缀（与既有 `profile_mode` 等键无冲突）。
const SETTINGS_KEY_PREFIX: &str = "config.";

/// 不参与双写的顶级键：环境相关元数据 / 运行时路径 / LLM 连接（后者走 backend_config 表）。
const SKIP_FLAT_KEYS: &[&str] = &["version", "schema_version", "paths", "backend"];

/// 默认配置模板（打包 `config/default.toml`，含完整注释说明）。
const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../../../config/default.toml");

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
                        path = %path_log_label(&self.config_path),
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
                        path = %path_log_label(&self.config_path),
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
                tracing::warn!(path = %path_log_label(&self.config_path), error = %e, "配置文件写入失败");
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
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
