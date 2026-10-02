//! crates/ramaria-service/src/config/file_io.rs - Ramaria config.toml 文件侧 I/O
//!
//! 设计特点:
//! - 读取：解析 config.toml 并同时提取"文件显式声明的键集"（供同步 diff）
//! - 渲染：序列化当前配置，保留文件头注释与未知键（无 TOML 编辑器依赖的保守合并）
//! - 原子替换：先写同目录 `{文件名}.part`，再 `fs::rename` 覆盖；失败清理临时文件
//! - 模板生成：文件缺失时写入打包的默认模板（`config/default.toml`）
//! - 日志标签只记录文件名，完整路径不进日志（避免暴露本机目录结构）

use std::path::Path;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;

use super::CONFIG_TEMP_SUFFIX;
use super::ConfigWriter;
use super::DEFAULT_CONFIG_TEMPLATE;
use super::ExplicitFileKeys;
use super::merge::{leading_comment_block, merge_unknown_keys, parse_explicit_file_keys};

// =========================================================
// 文件侧 I/O
// =========================================================

impl ConfigWriter {
    /// 读取并解析 config.toml。
    pub(super) fn read_file_config(&self) -> RamariaResult<RamariaConfig> {
        self.read_file_config_with_keys().map(|(cfg, _)| cfg)
    }

    /// 读取并解析 config.toml，同时提取"文件显式声明的键集"。
    ///
    /// 返回:
    /// - `(配置, 显式键集)`；键集用于同步 diff（只回写文件里真实写了的键）。
    pub(super) fn read_file_config_with_keys(
        &self,
    ) -> RamariaResult<(RamariaConfig, ExplicitFileKeys)> {
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
    pub(super) fn write_file_config(&self, cfg: &RamariaConfig) -> RamariaResult<()> {
        let text = self.render_config_text(cfg)?;
        atomic_write(&self.config_path, &text)
    }

    /// 渲染 config.toml 文本：序列化当前配置，并尽量保留用户文件中的既有内容。
    ///
    /// 保留策略（无 TOML 编辑器依赖下的保守合并）:
    /// - 文件头注释块（开头的注释与空行）原样保留；
    /// - 旧文件中"当前 schema 未知"的键（含未知分组）原样保留；
    /// - 其余键以当前配置为准（分组内部注释不保留）。
    pub(super) fn render_config_text(&self, cfg: &RamariaConfig) -> RamariaResult<String> {
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
    pub(super) fn write_template_file(&self) -> RamariaResult<()> {
        atomic_write(&self.config_path, DEFAULT_CONFIG_TEMPLATE)
    }
}

// =========================================================
// 路径日志标签与原子写入
// =========================================================

/// 取路径的文件名用于日志（完整路径不进日志，避免暴露本机目录结构）。
pub(super) fn path_log_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string())
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
