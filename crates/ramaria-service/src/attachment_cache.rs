//! crates/ramaria-service/src/attachment_cache.rs - Ramaria 附件图片缓存模块
//!
//! 设计特点:
//! - 客户端图片缓存：图片字节按 md5（缺失时按附件 id）命名落盘，读取时优先命中
//! - 全链路 best-effort：缓存写入 / 单文件清理失败只记日志，不阻塞调用方主流程
//! - 清理只处理第一层文件（不递归子目录），统计仅计成功删除者
//! - 日志不落路径原文（只记附件 id 与错误原因）

use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::vision::image_mime_from_bytes;

// =========================================================
// 常量与统计
// =========================================================

/// 缓存文件候选扩展名（查找顺序固定）。
const EXTENSIONS: [&str; 5] = ["png", "jpg", "gif", "webp", "bmp"];

/// 缓存清理统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttachmentCacheStat {
    /// 成功删除的文件数。
    pub files: usize,
    /// 成功删除的文件字节数。
    pub bytes: u64,
}

// =========================================================
// 查找与写入
// =========================================================

/// 查找缓存文件（按 md5 或附件 id 生成文件名主干，依次尝试候选扩展名）。
///
/// 参数:
/// - `cache_dir`: 缓存目录。
/// - `attachment_id`: 附件行 id（md5 缺失时的命名回退）。
/// - `md5`: 图片内容指纹（可选；大小写不敏感）。
///
/// 返回:
/// - `Some(path)`: 首个存在的候选文件；
/// - `None`: 无缓存命中。
pub(crate) fn find_cached(
    cache_dir: &Path,
    attachment_id: i64,
    md5: Option<&str>,
) -> Option<PathBuf> {
    let stem = cache_stem(attachment_id, md5);
    EXTENSIONS
        .iter()
        .map(|ext| cache_dir.join(format!("{stem}.{ext}")))
        .find(|candidate| candidate.is_file())
}

/// 写入缓存文件（best-effort：失败只记日志并返回 None）。
///
/// 扩展名按内容魔数判定；未知格式回退 jpg。
///
/// 参数:
/// - `cache_dir`: 缓存目录（不存在时自动创建）。
/// - `attachment_id`: 附件行 id（md5 缺失时的命名回退）。
/// - `md5`: 图片内容指纹（可选；大小写不敏感）。
/// - `bytes`: 图片字节。
///
/// 返回:
/// - `Some(path)`: 写入成功后的缓存文件路径；
/// - `None`: 目录创建或写入失败（调用方不得因此失败）。
pub(crate) fn store(
    cache_dir: &Path,
    attachment_id: i64,
    md5: Option<&str>,
    bytes: &[u8],
) -> Option<PathBuf> {
    let path = cache_dir.join(format!(
        "{}.{}",
        cache_stem(attachment_id, md5),
        extension_for(bytes)
    ));
    if let Err(error) = std::fs::create_dir_all(cache_dir) {
        tracing::warn!(attachment_id, error = %error, "图片缓存目录创建失败（跳过缓存）");
        return None;
    }
    if let Err(error) = std::fs::write(&path, bytes) {
        tracing::warn!(attachment_id, error = %error, "图片缓存写入失败（跳过缓存）");
        return None;
    }
    Some(path)
}

/// 缓存文件名主干：md5 全小写；md5 缺失或空白 → `id-{attachment_id}`。
fn cache_stem(attachment_id: i64, md5: Option<&str>) -> String {
    match md5.map(str::trim).filter(|md5| !md5.is_empty()) {
        Some(md5) => md5.to_ascii_lowercase(),
        None => format!("id-{attachment_id}"),
    }
}

/// 按内容魔数判定缓存文件扩展名（未知格式回退 jpg）。
fn extension_for(bytes: &[u8]) -> &'static str {
    match image_mime_from_bytes(bytes) {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "jpg",
    }
}

// =========================================================
// 清理
// =========================================================

/// 清理缓存目录中的第一层文件（不递归子目录）。
///
/// 返回:
/// - 成功删除的文件数与字节数（目录不存在时全 0）；
/// - 目录不可读时返回 `Storage` 错误；单个文件删除失败只记日志跳过，
///   统计仅计成功删除者。
pub(crate) fn clear(cache_dir: &Path) -> RamariaResult<AttachmentCacheStat> {
    let mut stat = AttachmentCacheStat::default();
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(stat),
        Err(error) => {
            return Err(RamariaError::storage_with_source(
                "读取图片缓存目录失败",
                error,
            ));
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                stat.files += 1;
                stat.bytes += size;
            }
            Err(error) => {
                tracing::warn!(error = %error, "缓存文件删除失败（跳过该文件）");
            }
        }
    }
    Ok(stat)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
