//! crates/ramaria-service/src/image_source.rs - Ramaria 本地发图目录用例
//!
//! 设计特点:
//! - 单目录模型：默认 `{data_dir}/outgoing`，用户至多设置一个自定义目录（`settings` 内部键，不进 `config.toml`）
//! - 恢复默认 = 删除设置键：不在库里写死绝对路径，数据目录整体搬迁后默认目录仍正确
//! - 写路径校验收紧：目录必须存在且为目录，`canonicalize` 后拒绝盘符根与系统目录
//! - 文件校验收紧：只接受目录内普通文件，拒绝符号链接与 `..` 穿越（`canonicalize` + 前缀）
//! - 读路径容错：自定义目录被删除 / 不可解析时回退默认目录并发 warn，不阻塞发图
//! - 隐私：日志只记脱敏路径标签，不记绝对路径

use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::engine::Engine;

// =========================================================
// 常量
// =========================================================

/// 发图目录设置键（`settings` 表内部键，不进入 `config.toml`）。
pub(crate) const SETTING_KEY: &str = "image_source_dir";

/// 默认发图目录名（位于数据目录下）。
pub(crate) const DEFAULT_DIR_NAME: &str = "outgoing";

/// 拒绝作为发图目录的系统目录名（小写比较，任一组件命中即拒绝）。
const FORBIDDEN_DIR_NAMES: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
];

// =========================================================
// 目录解析与设置
// =========================================================

/// 默认发图目录：数据库文件所在目录下的 `outgoing/`。
///
/// 参数:
/// - `db_path`: 数据库文件路径（数据目录由调用方给出）。
///
/// 返回:
/// - 未创建的默认目录路径（父目录缺失时回退当前目录）。
pub(crate) fn default_dir(db_path: &Path) -> PathBuf {
    match db_path.parent() {
        Some(parent) => parent.join(DEFAULT_DIR_NAME),
        None => PathBuf::from(DEFAULT_DIR_NAME),
    }
}

/// 读取生效的发图目录（必要时创建默认目录）。
///
/// 返回:
/// - 已存在且可用的目录真实路径；库内自定义目录失效时回退默认目录。
///
/// 说明:
/// - 自定义目录不可解析（被删除 / 变成文件 / 越权）时回退默认目录并记录 warn，
///   不回写设置（用户恢复该目录后自动重新生效）。
pub(crate) async fn dir(engine: &Engine) -> RamariaResult<PathBuf> {
    let fallback = default_dir(engine.db_path());
    let stored = engine
        .storage_ref()
        .as_ref()
        .get_setting(SETTING_KEY)
        .await?;

    let resolved = match stored.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => match validate_dir(Path::new(raw)) {
            Ok(real) => real,
            Err(error) => {
                tracing::warn!(
                    reason = %error,
                    "发图目录不可用，回退默认目录"
                );
                fallback
            }
        },
        None => fallback,
    };

    ensure_dir(&resolved)?;
    Ok(canonical_or_self(resolved))
}

/// 设置发图目录（`None` / 空白 = 恢复默认）。
///
/// 参数:
/// - `path`: 自定义目录路径；`None` 或空白表示恢复默认并删除设置键。
///
/// 返回:
/// - 生效目录的真实路径。
///
/// 错误:
/// - 目录不存在 / 不是目录 / 是符号链接 / 位于系统目录或盘符根时返回 `Validation`。
pub(crate) async fn set(engine: &Engine, path: Option<&str>) -> RamariaResult<PathBuf> {
    let storage = engine.storage_ref();

    let Some(raw) = path.map(str::trim).filter(|v| !v.is_empty()) else {
        storage.as_ref().delete_setting(SETTING_KEY).await?;
        let dir = default_dir(engine.db_path());
        ensure_dir(&dir)?;
        tracing::info!("发图目录已恢复默认");
        return Ok(canonical_or_self(dir));
    };

    let real = validate_dir(Path::new(raw))?;
    storage
        .as_ref()
        .set_setting(SETTING_KEY, &real.to_string_lossy())
        .await?;
    tracing::info!("发图目录已更新为自定义目录");
    Ok(real)
}

// =========================================================
// 路径校验
// =========================================================

/// 校验发图文件：必须是发图目录内的普通文件。
///
/// 参数:
/// - `dir`: 生效的发图目录（应已为真实路径）。
/// - `file`: 待发送文件路径。
///
/// 返回:
/// - 规范化后的文件真实路径。
///
/// 错误:
/// - 文件不存在 / 不是普通文件 / 是符号链接 / 不在发图目录内时返回 `Validation`。
pub(crate) fn validate_file(dir: &Path, file: &Path) -> RamariaResult<PathBuf> {
    if !file.exists() {
        return Err(RamariaError::validation(format!(
            "图片不存在：{}",
            file.display()
        )));
    }
    if !file.is_file() {
        return Err(RamariaError::validation(format!(
            "不是普通文件：{}",
            file.display()
        )));
    }

    let metadata = std::fs::symlink_metadata(file)
        .map_err(|_| RamariaError::validation(format!("无法读取文件信息：{}", file.display())))?;
    if metadata.file_type().is_symlink() {
        return Err(RamariaError::validation(format!(
            "不支持符号链接：{}",
            file.display()
        )));
    }

    let real_dir = dir
        .canonicalize()
        .map_err(|_| RamariaError::validation("发图目录不可用，请在设置中重新选择"))?;
    let real_file = file
        .canonicalize()
        .map_err(|_| RamariaError::validation(format!("路径无法解析：{}", file.display())))?;

    if !real_file.starts_with(&real_dir) {
        return Err(RamariaError::validation(
            "只能发送发图目录内的图片，请在设置中调整发图目录",
        ));
    }

    Ok(real_file)
}

/// 校验候选目录：存在、是目录、非链接、非盘符根与系统目录。
///
/// 返回:
/// - `canonicalize` 后的真实目录路径。
fn validate_dir(candidate: &Path) -> RamariaResult<PathBuf> {
    if !candidate.exists() {
        return Err(RamariaError::validation(format!(
            "目录不存在：{}",
            candidate.display()
        )));
    }
    if !candidate.is_dir() {
        return Err(RamariaError::validation(format!(
            "不是目录：{}",
            candidate.display()
        )));
    }

    let metadata = std::fs::symlink_metadata(candidate).map_err(|_| {
        RamariaError::validation(format!("无法读取目录信息：{}", candidate.display()))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(RamariaError::validation(format!(
            "不支持符号链接目录：{}",
            candidate.display()
        )));
    }

    let real = candidate
        .canonicalize()
        .map_err(|_| RamariaError::validation(format!("目录无法解析：{}", candidate.display())))?;

    if is_forbidden_dir(&real) {
        return Err(RamariaError::validation(format!(
            "不能使用系统目录或盘符根作为发图目录：{}",
            real.display()
        )));
    }

    Ok(real)
}

/// 判定目录是否为盘符根或系统目录。
fn is_forbidden_dir(real: &Path) -> bool {
    // 盘符根 / 文件系统根：没有父目录
    if real.parent().is_none() {
        return true;
    }
    real.components().any(|component| {
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        FORBIDDEN_DIR_NAMES
            .iter()
            .any(|forbidden| name == *forbidden)
    })
}

/// 确保目录存在（默认目录首次使用时创建）。
fn ensure_dir(dir: &Path) -> RamariaResult<()> {
    std::fs::create_dir_all(dir)
        .map_err(|error| RamariaError::storage(format!("创建发图目录失败: {error}")))
}

/// 返回目录真实路径（解析失败时保留原路径，已创建目录正常可解析）。
fn canonical_or_self(dir: PathBuf) -> PathBuf {
    match dir.canonicalize() {
        Ok(real) => real,
        Err(_) => dir,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
