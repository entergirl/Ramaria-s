//! crates/ramaria-core/src/config/paths.rs - Ramaria 路径配置模块
//!
//! 设计特点:
//! - 定义数据目录、数据库与模型文件等路径配置
//! - 提供平台无关的默认路径
//! - 支持 serde，供 config.toml 共享
//! - 只描述数据，不做文件系统 IO

use serde::{Deserialize, Serialize};

// =========================================================
// 路径配置
// =========================================================

/// 数据与路径配置。
///
/// 职责:
/// - 描述 Ramaria 的数据目录、配置目录、日志目录和向量索引目录。
/// - 只保存路径字符串，不负责解析 `%APPDATA%` 或环境变量。
///
/// 说明:
/// - 默认值为空字符串，由上层配置加载器根据平台和运行模式填充。
/// - 开发模式可由 `RAMARIA_DATA_DIR` 或测试夹具覆盖。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PathConfig {
    /// SQLite 数据库路径。Windows 默认 `%APPDATA%\Ramaria\data\assistant.db`
    pub data_dir: String,
    /// 配置文件目录
    pub config_dir: String,
    /// 日志目录
    pub log_dir: String,
    /// 向量索引目录
    pub vector_index_dir: String,
}

impl Default for PathConfig {
    /// 创建空路径配置。
    ///
    /// 返回:
    /// - 所有路径字段为空，等待配置加载层填充平台默认路径。
    fn default() -> Self {
        Self {
            data_dir: String::new(),
            config_dir: String::new(),
            log_dir: String::new(),
            vector_index_dir: String::new(),
        }
    }
}
