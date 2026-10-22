//! crates/ramaria-service/src/engine/assemble.rs - Ramaria 引擎装配与依赖访问器
//!
//! 设计特点:
//! - 装配顺序固定：数据库连接池 + migration → 配置只读加载 → 后端配置（DB 真源）
//!   → LLM provider（按 `[cache]` 注入精确缓存）→ 嵌入 provider（缺失降级）→ 检索器占位
//! - 注入构造（`from_parts`）：不创建连接池与迁移、不加载配置文件，调用方对依赖生命周期负责
//! - 访问器统一返回"已脱离锁"的句柄：Arc 克隆（provider 快照）或引用（存储 / 锁槽），
//!   异步路径不跨 `.await` 持锁
//! - 策略与钩子（召回策略 / 封存钩子 / 封存许可）与投递接收端由入口层注入，
//!   缺省按配置闸门映射（接收端门面见 `sinks` 子模块）
//! - provider 热更新：整体替换快照，读取方取到替换前或替换后的完整快照，无中间态
//! - 装配辅助（配置只读加载 / provider 构造 / 嵌入恢复）与热更新共用同一构造口径
//!   （provider 构造与嵌入恢复见 `providers` 子模块）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ramaria_core::config::RamariaConfig;
use ramaria_core::lock::{read_recover, write_recover};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider};
use ramaria_memory::job::JobManagerConfig;

use crate::engine::Engine;
use crate::recall::RecallPolicy;
use crate::seal::SealHooks;

mod accessors;
mod open;
pub(super) mod providers;
mod sinks;

// =========================================================
// 策略与钩子（入口层注入）
// =========================================================

impl Engine {
    /// 设置召回隐私与边界策略。
    ///
    /// 用法:
    /// - 入口层按需注入覆盖装配缺省（如按 `[mcp].allow_raw_text`、
    ///   `allowed_personas` 收紧口径）；未注入时使用 [`RecallPolicy::from_config`]
    ///   的配置映射缺省。
    ///
    /// 参数:
    /// - `policy`: 召回策略快照。
    pub fn set_recall_policy(&self, policy: RecallPolicy) {
        tracing::info!(
            allow_raw_text = policy.allow_raw_text,
            allowed_persona_rules = policy.allowed_personas.len(),
            "召回策略已更新"
        );
        let mut guard = write_recover(&self.recall_policy, "engine.recall_policy");
        *guard = policy;
    }

    /// 当前召回策略快照（副本）。
    pub fn recall_policy(&self) -> RecallPolicy {
        read_recover(&self.recall_policy, "engine.recall_policy").clone()
    }

    /// 注册封存钩子（行为 / 风格 / L2 触发；未注册的步骤在封存时跳过）。
    ///
    /// 用法:
    /// - 入口层启动时注入（可用 `default_seal_hooks` / `full_seal_hooks` 两套默认装配）；
    ///   未注册的步骤在封存时跳过（L1 / utt / examples 不受影响）。
    pub fn set_seal_hooks(&self, hooks: SealHooks) {
        tracing::info!(
            behavior = hooks.behavior.is_some(),
            style = hooks.style.is_some(),
            l2_trigger = hooks.l2_trigger.is_some(),
            "封存钩子已更新"
        );
        let mut guard = write_recover(&self.seal_hooks, "engine.seal_hooks");
        *guard = hooks;
    }

    /// 当前封存钩子快照（Arc 克隆，供封存流程在锁外调用）。
    pub fn seal_hooks(&self) -> SealHooks {
        read_recover(&self.seal_hooks, "engine.seal_hooks").clone()
    }

    /// 设置封存许可（入口层按配置注入，如 `[mcp].allow_seal`）。
    ///
    /// 语义:
    /// - `false`: 封存用例直接跳过（不关闭会话、不生成摘要、不消耗 LLM 做记忆加工）；
    ///   写用例（`ingest` / `chat_send`）仍可正常写入，超时会话留待允许封存的
    ///   宿主（桌面）或下次允许时的空闲检查处理；
    /// - `true`: 恢复默认行为（抢占式封存与空闲检查照常）。
    ///
    /// 用法:
    /// - 入口层启动时注入一次（进程级快照语义，与其它配置一致）。
    pub fn set_seal_allowed(&self, allowed: bool) {
        tracing::info!(allow_seal = allowed, "封存许可已更新（服务层门禁）");
        self.seal_allowed.store(allowed, Ordering::Release);
    }

    /// 当前封存许可（`false` = 只写不封存）。
    pub fn seal_allowed(&self) -> bool {
        self.seal_allowed.load(Ordering::Acquire)
    }

    /// 后台任务管理器配置快照（L1 生成 / 补扫的重试退避口径；读锁内克隆后返回）。
    pub(crate) fn job_manager_config(&self) -> JobManagerConfig {
        read_recover(&self.job_manager_config, "engine.job_manager_config").clone()
    }

    /// 设置后台任务管理器配置（测试装配注入短退避等场景）。
    #[cfg(test)]
    pub(crate) fn set_job_manager_config(&self, config: JobManagerConfig) {
        let mut guard = write_recover(&self.job_manager_config, "engine.job_manager_config");
        *guard = config;
    }
}

// =========================================================
// 依赖热更新（后端配置 / 嵌入模型变更时整体替换快照）
// =========================================================

impl Engine {
    /// 热更新 LLM provider（后端配置变更后整体替换）。
    ///
    /// 参数:
    /// - `provider`: 已构造好的新 provider（线上 provider 需已注入 keychain 密钥来源）。
    ///
    /// 说明:
    /// - 并发读取方取到的是替换前或替换后的完整快照，不存在"半个 provider"的中间态；
    /// - 缓存实例不在本方法内替换（由 [`Engine::llm_cache`] 持有，构造新 provider 时复用）。
    pub fn update_llm(&self, provider: Arc<dyn LlmProvider>) {
        let new_name = provider.name();
        let old = {
            let mut guard = write_recover(&self.llm, "engine.llm");
            std::mem::replace(&mut *guard, provider)
        };
        tracing::info!(
            old_provider = old.name(),
            new_provider = new_name,
            "LLM provider 已热更新"
        );
    }

    /// 热更新嵌入 provider（加载 / 卸载模型后整体替换）。
    ///
    /// 参数:
    /// - `provider`: `Some` 为加载（向量通道就绪），`None` 为卸载（向量通道降级）。
    ///
    /// 说明:
    /// - 替换只影响后续召回与索引构建；既有内存索引在下一次懒加载刷新时重建
    ///   （与"跨进程写入后刷新"同一路径）。
    pub fn update_embedding(&self, provider: Option<Arc<dyn EmbeddingProvider>>) {
        match provider.as_ref() {
            Some(provider) => {
                let info = provider.model_info();
                tracing::info!(
                    model = %info.model_id,
                    dimension = info.dimension,
                    "嵌入模型已热更新（向量通道就绪）"
                );
            }
            None => tracing::info!("嵌入模型已卸载（向量通道降级，BM25 + 关键词镜像继续可用）"),
        }
        let mut guard = write_recover(&self.embedding, "engine.embedding");
        *guard = provider;
    }
}

// =========================================================
// 装配辅助（文件内私有）
// =========================================================

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
pub(super) fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// 默认配置文件路径：数据库同目录 `config.toml`。
fn default_config_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .map(|dir| dir.join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

/// 只读加载配置：读取 `config.toml` 并填充实际路径；缺失 / 解析失败回退默认值。
///
/// 返回:
/// - 配置快照与可选的回退告警：文件存在但读取 / 解析失败时，告警携带脱敏摘要
///   （错误首行、按字符截断），供入口层提示携带原因；其余情况为 None。
///
/// 说明:
/// - 装配路径不生成模板、不回写 DB（无副作用的只读装配；写入只经配置用例）。
/// - 路径字段以数据库所在目录为数据根填充（与入口层同一约定）。
fn load_config_readonly(config_path: &Path, db_path: &Path) -> (RamariaConfig, Option<String>) {
    let mut warning = None;
    let mut config = if config_path.exists() {
        match std::fs::read_to_string(config_path) {
            Ok(text) => match toml::from_str::<RamariaConfig>(&text) {
                Ok(parsed) => parsed,
                Err(e) => {
                    tracing::warn!(
                        path = %path_log_label(config_path),
                        error = %e,
                        "config.toml 解析失败，回退默认配置"
                    );
                    warning = Some(format!(
                        "config.toml 解析失败，已回退默认配置：{}",
                        config_error_digest(&e.to_string())
                    ));
                    RamariaConfig::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    path = %path_log_label(config_path),
                    error = %e,
                    "config.toml 读取失败，回退默认配置"
                );
                warning = Some(format!(
                    "config.toml 读取失败，已回退默认配置：{}",
                    config_error_digest(&e.to_string())
                ));
                RamariaConfig::default()
            }
        }
    } else {
        tracing::debug!(
            path = %path_log_label(config_path),
            "config.toml 不存在，使用默认配置"
        );
        RamariaConfig::default()
    };

    let data_dir = db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    config.paths.data_dir = data_dir.to_string_lossy().into_owned();
    config.paths.log_dir = data_dir.join("logs").to_string_lossy().into_owned();
    config.paths.config_dir = data_dir.to_string_lossy().into_owned();
    config.paths.vector_index_dir = data_dir.join("vectors").to_string_lossy().into_owned();
    (config, warning)
}

/// 配置错误摘要（面向入口提示）：取错误文本首行并按字符截断到上限内。
///
/// 说明:
/// - 提示只携带首行（通常含行号与简短原因），不把文件内容带入面向用户文案；
/// - 超长时保留前 `MAX_CHARS - 1` 个字符并以省略号标记截断。
fn config_error_digest(error: &str) -> String {
    const MAX_CHARS: usize = 200;
    let first_line = error.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= MAX_CHARS {
        return first_line.to_string();
    }
    let mut digest: String = first_line.chars().take(MAX_CHARS - 1).collect();
    digest.push('…');
    digest
}
