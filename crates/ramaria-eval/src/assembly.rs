//! crates/ramaria-eval/src/assembly.rs - 评测装配（独立配置快照 + 无后台循环）
//!
//! 设计特点:
//! - 配置快照取产品默认值的全量序列化再覆盖评测专用项，评测不受用户配置与默认模板影响
//! - 评测根自包含：库、配置快照、日志与向量索引目录都落在 root 下，不写用户库
//! - 只装配引擎，不拉起生命周期循环，也不注册任何后台调度任务
//! - 通道与主动对话在快照里关闭，评测进程不外发消息
//! - 错误统一走 core 结构化错误，不吞错也不 panic

use std::path::{Path, PathBuf};

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{Engine, EngineOptions};

#[cfg(test)]
mod tests;

/// 评测根下的数据库文件名。
pub const EVAL_DB_FILE: &str = "ramaria.db";

/// 评测根下的配置快照文件名。
pub const EVAL_CONFIG_FILE: &str = "config.toml";

/// 写入评测配置快照。
///
/// 职责:
/// - 把产品默认配置全量序列化后覆盖评测专用项，写成评测根的 `config.toml`。
///
/// 参数:
/// - `root`: 评测根目录（不存在时创建）。
///
/// 返回:
/// - 成功时返回快照文件路径。
/// - 目录创建 / 写入失败返回 Io 错误；序列化失败返回 Config 错误。
///
/// 说明:
/// - 覆盖项只有两类：评测必须关掉的行为（通道 / 主动对话）与必须落在评测根的路径；
///   其余字段保持产品默认，保证对照实验可复现；
/// - 快照是完整配置而非片段：装配路径按它解析，缺项与否都会改变评测口径。
pub fn write_config_snapshot(root: &Path) -> RamariaResult<PathBuf> {
    std::fs::create_dir_all(root).map_err(|error| {
        RamariaError::io(
            format!("创建评测根目录失败: {}", root.display()),
            Some(error),
        )
    })?;

    let mut config = RamariaConfig::default();
    apply_eval_overrides(&mut config, root);
    let text = toml::to_string_pretty(&config).map_err(|error| RamariaError::Config {
        context: "序列化评测配置快照失败".to_string(),
        source: Some(Box::new(error)),
        trace_id: None,
    })?;

    let path = root.join(EVAL_CONFIG_FILE);
    std::fs::write(&path, text).map_err(|error| {
        RamariaError::io(
            format!("写入评测配置快照失败: {}", path.display()),
            Some(error),
        )
    })?;
    Ok(path)
}

/// 以评测根装配引擎。
///
/// 参数:
/// - `root`: 评测根目录（不存在时创建）。
///
/// 返回:
/// - 成功时返回完成装配的引擎；库初始化 / 迁移 / provider 构建失败返回对应错误。
///
/// 说明:
/// - 配置来自本目录的快照（不继承用户配置），库落在 `root/ramaria.db`；
/// - 不调用 `start_lifecycle`：空闲封存、L2/L3 调度、主动对话与启动期补扫都不在评测进程里跑，
///   评测节奏由调用方逐次驱动，避免后台循环搅动计时与库状态。
pub async fn open_engine(root: &Path) -> RamariaResult<Engine> {
    let config_path = write_config_snapshot(root)?;
    let db_path = root.join(EVAL_DB_FILE);
    Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path)).await
}

/// 覆盖评测专用项：关闭通道与主动对话，路径全部指向评测根。
///
/// 说明:
/// - 通道四个开关与主动对话总开关一并关闭：评测只读本地库，不代客户端发言；
/// - 路径以评测根为数据根，与装配路径"库所在目录即数据根"的约定一致，
///   日志与向量索引各占 root 下一个子目录，产物不散落到用户目录。
fn apply_eval_overrides(config: &mut RamariaConfig, root: &Path) {
    config.channel.enabled = false;
    config.channel.qq_enabled = false;
    config.channel.telegram_enabled = false;
    config.channel.proactive_delivery_enabled = false;
    config.proactive.enabled = false;

    let root_text = root.to_string_lossy().into_owned();
    config.paths.data_dir = root_text.clone();
    config.paths.config_dir = root_text;
    config.paths.log_dir = root.join("logs").to_string_lossy().into_owned();
    config.paths.vector_index_dir = root.join("vectors").to_string_lossy().into_owned();
}
