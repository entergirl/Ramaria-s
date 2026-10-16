//! crates/ramaria-service/src/sticker.rs - Ramaria 表情资产用例
//!
//! 设计特点:
//! - 资产目录在数据库同级的 `stickers/`，文件名为内容 md5 + 扩展名；同内容覆盖写（幂等）
//! - 入库与文件落盘同点：先写文件再落库（落库失败残留文件为孤儿，不影响正确性）
//! - 主题解析两段：纯函数匹配（标签全等 > 标签包含 > 描述包含，并列取小 id）+
//!   文件读取（读失败降级 None，不阻塞发送链路）
//! - 隐私：日志只记资产 id 与文件名，不记描述原文

use std::path::{Path, PathBuf};

use md5::{Digest, Md5};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::StickerAsset;
use ramaria_core::types::now_ms;

use crate::engine::Engine;
use crate::types::StickerPayload;

/// 表情资产目录名（数据库文件同级）。
const STICKER_DIR_NAME: &str = "stickers";

/// 单张资产的字节上限（20 MB）。
const MAX_STICKER_BYTES: u64 = 20 * 1024 * 1024;

// =========================================================
// 资产读写
// =========================================================

/// 新增一条表情资产（写文件 + 落库）。
///
/// 校验:
/// - 字节为空 / 超限 / 非可识别图片格式（魔数识别）→ 校验错误。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `file_name`: 原始文件名。
/// - `bytes`: 图片字节。
/// - `description`: 可选描述；
/// - `tags`: 标签列表。
///
/// 返回:
/// - 落库后的资产行。
pub(crate) async fn add(
    engine: &Engine,
    file_name: &str,
    bytes: &[u8],
    description: Option<&str>,
    tags: &[String],
) -> RamariaResult<StickerAsset> {
    if bytes.is_empty() {
        return Err(RamariaError::validation("图片字节不能为空"));
    }
    if bytes.len() as u64 > MAX_STICKER_BYTES {
        return Err(RamariaError::validation("图片超过 20 MB 上限"));
    }
    // 魔数识别（未知格式不回退）：非图片字节在此拒绝
    let Some(mime) = crate::vision::image_mime_checked(bytes) else {
        return Err(RamariaError::validation("不是可识别的图片格式"));
    };
    let md5 = md5_hex(bytes);
    let ext = extension_for_mime(mime);
    let file_name_stored = format!("{md5}.{ext}");
    let dir = sticker_dir(engine)?;
    let path = dir.join(&file_name_stored);
    write_sticker_file(&dir, &path, bytes)?;

    let now = now_ms();
    let asset = StickerAsset {
        id: 0,
        file_name: file_name.to_string(),
        file_path: file_name_stored,
        md5: Some(md5),
        size: Some(bytes.len() as u64),
        description: description
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        tags: tags.to_vec(),
        created_at: now,
        updated_at: now,
    };
    let storage = engine.storage_ref().as_ref();
    let id = storage.insert_sticker_asset(&asset).await?;
    storage
        .get_sticker_asset(id)
        .await?
        .ok_or_else(|| RamariaError::storage(format!("表情资产写入后读取失败: id={id}")))
}

/// 列出全部表情资产（按 id 升序）。
pub(crate) async fn list(engine: &Engine) -> RamariaResult<Vec<StickerAsset>> {
    engine.storage_ref().as_ref().list_sticker_assets().await
}

/// 更新表情资产的描述与标签。
///
/// 归一:
/// - `description`: `None` 保持现值；`Some` 在本层 trim 后透传，空串由存储侧
///   清空为 NULL（清空语义）；
/// - `tags`: `Some` 时整体覆盖；`None` 保持现值。
///
/// 返回:
/// - 目标行存在且已更新为 true；不存在为 false。
pub(crate) async fn update(
    engine: &Engine,
    id: i64,
    description: Option<&str>,
    tags: Option<&[String]>,
) -> RamariaResult<bool> {
    let description = description.map(|value| value.trim().to_string());
    engine
        .storage_ref()
        .as_ref()
        .update_sticker_asset(id, description.as_deref(), tags)
        .await
}

/// 删除表情资产（best-effort 删除资产文件，失败只记 warn）。
///
/// 返回:
/// - 目标行存在且已删除为 true；不存在为 false。
pub(crate) async fn remove(engine: &Engine, id: i64) -> RamariaResult<bool> {
    let storage = engine.storage_ref().as_ref();
    let Some(asset) = storage.get_sticker_asset(id).await? else {
        return Ok(false);
    };
    if !storage.delete_sticker_asset(id).await? {
        return Ok(false);
    }
    if !asset.file_path.trim().is_empty() {
        if let Ok(dir) = sticker_dir(engine) {
            let path = dir.join(&asset.file_path);
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::warn!(
                    id,
                    file_path = %asset.file_path,
                    error = %error,
                    "表情资产文件删除失败（仅删除行记录）"
                );
            }
        }
    }
    Ok(true)
}

// =========================================================
// 主题解析
// =========================================================

/// 按主题关键词匹配资产（纯函数）。
///
/// 评分:
/// - 标签全等 = 3；标签包含 = 2；描述包含 = 1；未命中 = 0。
/// - 取最高分；并列取 id 最小。
///
/// 参数:
/// - `assets`: 资产列表；
/// - `keyword`: 主题关键词（trim 后为空 → None）。
///
/// 返回:
/// - 命中的资产引用（None = 无匹配）。
pub(crate) fn match_asset<'a>(
    assets: &'a [StickerAsset],
    keyword: &str,
) -> Option<&'a StickerAsset> {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return None;
    }

    let mut best: Option<(&StickerAsset, u8)> = None;
    for asset in assets {
        let mut score: u8 = 0;
        for tag in &asset.tags {
            let tag = tag.trim();
            if tag.is_empty() {
                continue;
            }
            if tag == keyword {
                score = score.max(3);
            } else if tag.contains(keyword) {
                score = score.max(2);
            }
        }
        if score < 3 {
            if let Some(description) = asset.description.as_deref() {
                if description.contains(keyword) {
                    score = score.max(1);
                }
            }
        }
        if score == 0 {
            continue;
        }
        best = match best {
            Some((current, current_score)) => {
                if score > current_score || (score == current_score && asset.id < current.id) {
                    Some((asset, score))
                } else {
                    Some((current, current_score))
                }
            }
            None => Some((asset, score)),
        };
    }
    best.map(|(asset, _)| asset)
}

/// 解析表情主题（匹配资产并读取文件字节）。
///
/// 降级:
/// - 无匹配 / 文件读取失败 → None（记 warn，不阻塞发送链路）。
///
/// 返回:
/// - 命中的资产载荷（id / 文件名 / 字节）。
pub(crate) async fn resolve(
    engine: &Engine,
    keyword: &str,
) -> RamariaResult<Option<StickerPayload>> {
    let assets = list(engine).await?;
    let Some(asset) = match_asset(&assets, keyword) else {
        return Ok(None);
    };
    let dir = sticker_dir(engine)?;
    let path = dir.join(&asset.file_path);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(StickerPayload {
            id: asset.id,
            file_name: asset.file_name.clone(),
            bytes,
        })),
        Err(error) => {
            tracing::warn!(
                id = asset.id,
                file_path = %asset.file_path,
                error = %error,
                "表情资产文件读取失败，降级为未命中"
            );
            Ok(None)
        }
    }
}

// =========================================================
// 资产目录与格式
// =========================================================

/// 表情资产目录（数据库文件同级的 `stickers/`）。
fn sticker_dir(engine: &Engine) -> RamariaResult<PathBuf> {
    let db_path = engine.db_path();
    let parent = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| RamariaError::validation("数据库路径无效，无法定位表情资产目录"))?;
    Ok(parent.join(STICKER_DIR_NAME))
}

/// 写入资产文件（目录按需创建；同内容覆盖写）。
fn write_sticker_file(dir: &Path, path: &Path, bytes: &[u8]) -> RamariaResult<()> {
    std::fs::create_dir_all(dir)
        .map_err(|error| RamariaError::storage(format!("表情资产目录创建失败: {error}")))?;
    std::fs::write(path, bytes)
        .map_err(|error| RamariaError::storage(format!("表情资产文件写入失败: {error}")))?;
    Ok(())
}

/// 扩展名映射（未知 mime 回退 `jpg`）。
fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "jpg",
    }
}

/// 计算字节的 md5（小写 hex）。
fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
