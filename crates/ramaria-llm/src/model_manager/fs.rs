//! crates/ramaria-llm/src/model_manager/fs.rs - 路径与文件系统工具
//!
//! 设计特点:
//! - `path_log_label`: 日志只记录文件名，完整路径不进日志
//! - `dir_size`: 递归计算目录占用空间（路径缺失时返回 0）
//! - `default_models_root`: 默认模型根目录（Windows `%APPDATA%\Ramaria\models`）
//! - `RAMARIA_DATA_DIR` 环境变量可覆盖默认根目录

use std::fs;
use std::path::{Path, PathBuf};

// =========================================================
// 工具函数
// =========================================================

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
pub(crate) fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
}

/// 递归计算目录大小。
pub(crate) fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }

    if path.is_file() {
        return fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }

    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            total += dir_size(&entry.path());
        }
    }
    total
}

// =========================================================
// 便捷工厂
// =========================================================

/// 获取默认的模型根目录。
///
/// Windows: `%APPDATA%\Ramaria\models`
/// 可通过 `RAMARIA_DATA_DIR` 环境变量覆盖。
pub fn default_models_root() -> PathBuf {
    if let Ok(dir) = std::env::var("RAMARIA_DATA_DIR") {
        return PathBuf::from(dir).join("models");
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(appdata).join("Ramaria").join("models")
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".ramaria").join("models")
    }
}
