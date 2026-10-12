//! crates/ramaria-service/src/attachment_read.rs - Ramaria 附件图片读取模块
//!
//! 设计特点:
//! - 导出根持久化：settings 内部键存 JSON 字符串数组，读取容错（读失败 / 解析失败按空处理）
//! - 会话浏览读图：消息正文图片槽位扫描（hash / 描述 / 无文件三形态）与附件按序对齐
//! - 读取链路：客户端缓存优先 → 导出根直连 / md5 反查 → 体积校验 → best-effort 回填缓存
//! - 日志不落路径原文与消息正文（只记附件 id 与体积）

use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    InboundAttachmentKind, MessageAttachment, build_render_map, replace_image_placeholders,
};
use uuid::Uuid;

use crate::attachment_cache;
use crate::engine::Engine;
use crate::vision::{AttachmentLocator, MAX_IMAGE_BYTES, image_mime_from_bytes};

// =========================================================
// 导出根持久化
// =========================================================

/// 导出根持久化所用的 settings 内部键。
const SETTING_EXPORT_ROOTS: &str = "import_export_roots";

/// 登记一个导出根（幂等：已存在不重复写入）。
///
/// 语义:
/// - 读取现有列表（JSON 字符串数组）→ 去重追加 → 写回，顺序为首次登记顺序；
/// - 空字符串直接成功；现有值解析失败按空列表重建（仅 warn）。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `root`: 导出根目录字符串（导入侧已规范化的路径）。
#[cfg(feature = "importer")]
pub(crate) async fn register_export_root(
    storage: &dyn StorageBackend,
    root: &str,
) -> RamariaResult<()> {
    if root.is_empty() {
        return Ok(());
    }
    let mut roots = read_export_roots(storage).await;
    if roots.iter().any(|existing| existing == root) {
        return Ok(());
    }
    roots.push(root.to_string());
    let value = serde_json::to_string(&roots)
        .map_err(|error| RamariaError::storage_with_source("导出根序列化失败", error))?;
    storage.set_setting(SETTING_EXPORT_ROOTS, &value).await
}

/// 读取已登记的导出根列表（保序；读失败 / 解析失败 / 缺键均按空列表处理，仅 warn）。
async fn read_export_roots(storage: &dyn StorageBackend) -> Vec<String> {
    let raw = match storage.get_setting(SETTING_EXPORT_ROOTS).await {
        Ok(Some(raw)) => raw,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::warn!(error = %error, "读取导出根设置失败（按空列表处理）");
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<String>>(&raw) {
        Ok(roots) => roots,
        Err(error) => {
            tracing::warn!(error = %error, "导出根设置解析失败（按空列表处理）");
            Vec::new()
        }
    }
}

/// 读取已登记的导出根路径列表（保序，定位时按序尝试）。
pub(crate) async fn list_export_root_paths(storage: &dyn StorageBackend) -> Vec<PathBuf> {
    read_export_roots(storage)
        .await
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

// =========================================================
// 图片槽位扫描与对齐
// =========================================================

/// 消息正文中的图片槽位形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageSlot {
    /// `[图片#{hash}]`：8 位占位符 hash（保留原文形态，匹配时再小写化）。
    Hash(String),
    /// `[图片: {描述}]`：已渲染的图片描述。
    Desc(String),
    /// `[图片]`：无本地文件的图片。
    NoFile,
}

/// 占位符 hash 的固定 ASCII 字符数（与核心占位符渲染口径一致）。
const HASH_LEN: usize = 8;

/// `[图片#{hash}]` 槽位前缀。
const SLOT_PREFIX_HASH: &str = "[图片#";
/// `[图片: {描述}]` 槽位前缀。
const SLOT_PREFIX_DESC: &str = "[图片: ";
/// `[图片]` 槽位全量文本。
const SLOT_PREFIX_BARE: &str = "[图片]";

/// 扫描文本中的图片槽位（按出现顺序；其余形态不识别、不占槽）。
///
/// 形态:
/// - `[图片#` + 恰好 8 个 ASCII 字符 + `]` → `Hash`；
/// - `[图片: ` + 至少 1 个非 `[` `]` 字符（可含换行）+ `]` → `Desc`；
/// - `[图片]` → `NoFile`。
///
/// 参数:
/// - `text`: 待扫描文本（库内占位符形态或渲染后的描述形态）。
///
/// 返回:
/// - 按出现顺序的槽位列表。
pub(crate) fn scan_image_slots(text: &str) -> Vec<ImageSlot> {
    let mut slots = Vec::new();
    let mut pos = 0usize;
    while pos < text.len() {
        let Some((offset, prefix)) = find_next_prefix(&text[pos..]) else {
            break;
        };
        let start = pos + offset;
        let after = &text[start + prefix.len()..];
        if prefix == SLOT_PREFIX_HASH {
            let hash = after.get(..HASH_LEN).filter(|hash| hash.is_ascii());
            let closed = after.as_bytes().get(HASH_LEN) == Some(&b']');
            match (hash, closed) {
                (Some(hash), true) => {
                    slots.push(ImageSlot::Hash(hash.to_string()));
                    pos = start + prefix.len() + HASH_LEN + 1;
                }
                _ => pos = start + prefix.len(),
            }
        } else if prefix == SLOT_PREFIX_DESC {
            match desc_content(after) {
                Some(content) => {
                    slots.push(ImageSlot::Desc(content.to_string()));
                    pos = start + prefix.len() + content.len() + 1;
                }
                None => pos = start + prefix.len(),
            }
        } else {
            slots.push(ImageSlot::NoFile);
            pos = start + prefix.len();
        }
    }
    slots
}

/// 找最近出现的槽位前缀（三个候选的第 4 个字符互斥，位置不会重合）。
fn find_next_prefix(rest: &str) -> Option<(usize, &'static str)> {
    [
        (rest.find(SLOT_PREFIX_HASH), SLOT_PREFIX_HASH),
        (rest.find(SLOT_PREFIX_DESC), SLOT_PREFIX_DESC),
        (rest.find(SLOT_PREFIX_BARE), SLOT_PREFIX_BARE),
    ]
    .into_iter()
    .filter_map(|(offset, prefix)| offset.map(|offset| (offset, prefix)))
    .min_by_key(|(offset, _)| *offset)
}

/// 提取 `[图片: ` 前缀后的描述内容（撞到 `[` / 空内容 / 未闭合返回 None）。
fn desc_content(after: &str) -> Option<&str> {
    for (index, ch) in after.char_indices() {
        if ch == '[' {
            return None;
        }
        if ch == ']' {
            return after.get(..index).filter(|content| !content.is_empty());
        }
    }
    None
}

/// 把第 `slot` 个图片槽位对齐到具体附件（按序消费：Hash / Desc 各消费一个附件，NoFile 跳过）。
///
/// 规则:
/// - `Hash(h)`: 在未消费附件中找 md5 小写以 `h` 小写为前缀的第一个 → 消费；
/// - `Desc(d)`: 在未消费附件中找描述与 `d` 完全相等的第一个 → 消费；
/// - `NoFile`: 不消费附件。
///
/// 参数:
/// - `image_attachments`: 图片附件行（列表顺序为对齐顺序）。
/// - `slots`: 正文槽位序列。
/// - `slot`: 目标槽位下标。
///
/// 返回:
/// - `Some(index)`: 目标槽位对应的附件下标；
/// - `None`: 目标槽位无附件可定位（含槽位越界）。
pub(crate) fn align_slot(
    image_attachments: &[MessageAttachment],
    slots: &[ImageSlot],
    slot: usize,
) -> Option<usize> {
    if slot >= slots.len() {
        return None;
    }
    let mut used = vec![false; image_attachments.len()];
    for (index, current) in slots.iter().enumerate().take(slot + 1) {
        if matches!(current, ImageSlot::NoFile) {
            continue;
        }
        let hit = find_unused_attachment(image_attachments, &used, current);
        if index == slot {
            return hit;
        }
        if let Some(hit) = hit {
            used[hit] = true;
        }
    }
    None
}

/// 在未消费附件中定位槽位对应的第一个附件（无命中返回 None）。
fn find_unused_attachment(
    attachments: &[MessageAttachment],
    used: &[bool],
    slot: &ImageSlot,
) -> Option<usize> {
    match slot {
        ImageSlot::Hash(hash) => {
            let lowercase = hash.to_ascii_lowercase();
            attachments
                .iter()
                .enumerate()
                .position(|(index, attachment)| {
                    !used[index]
                        && attachment
                            .md5
                            .as_deref()
                            .map(|md5| md5.to_ascii_lowercase().starts_with(&lowercase))
                            .unwrap_or(false)
                })
        }
        ImageSlot::Desc(description) => {
            attachments
                .iter()
                .enumerate()
                .position(|(index, attachment)| {
                    !used[index] && attachment.description.as_deref() == Some(description.as_str())
                })
        }
        ImageSlot::NoFile => None,
    }
}

// =========================================================
// 图片读取
// =========================================================

/// 读取到的附件图片。
#[derive(Debug, Clone)]
pub struct AttachmentImage {
    /// 图片字节。
    pub bytes: Vec<u8>,
    /// 按魔数判定的 MIME。
    pub mime: &'static str,
    /// 附件行 id。
    pub attachment_id: i64,
    /// 是否来自客户端缓存（false = 源导出文件）。
    pub from_cache: bool,
}

/// 读取消息中第 `slot` 个图片槽位的图片字节。
///
/// 流程:
/// 1. 取目标消息与图片附件（消息不存在 → 校验错误）；
/// 2. 渲染占位符 → 扫描槽位 → 按序对齐目标附件（越界 / 无附件 → 校验错误）；
/// 3. 客户端缓存命中优先；未命中经导出根定位（直连 + md5 反查）读取源文件；
/// 4. 源读取成功后 best-effort 回填缓存；体积超限拒绝。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `message_id`: 目标消息 id。
/// - `slot`: 图片槽位下标（正文中第几张图片，从 0 起）。
/// - `cache_dir`: 客户端图片缓存目录。
///
/// 返回:
/// - [`AttachmentImage`]：字节、MIME、附件 id 与来源标志。
pub(crate) async fn read_message_image(
    engine: &Engine,
    message_id: Uuid,
    slot: usize,
    cache_dir: &Path,
) -> RamariaResult<AttachmentImage> {
    let storage = engine.storage_ref().clone();

    // ---- 1. 消息与图片附件 ----
    let message = storage
        .get_message(message_id)
        .await?
        .ok_or_else(|| RamariaError::validation("消息不存在"))?;
    let image_attachments: Vec<MessageAttachment> = storage
        .list_attachments_by_messages(&[message_id])
        .await?
        .into_iter()
        .filter(|attachment| attachment.kind == InboundAttachmentKind::Image)
        .collect();

    // ---- 2. 槽位扫描与对齐 ----
    let render_map = build_render_map(&image_attachments);
    let rendered = replace_image_placeholders(&message.content, &render_map);
    let slots = scan_image_slots(&rendered);
    if slot >= slots.len() {
        return Err(RamariaError::validation("图片序号越界"));
    }
    let index = align_slot(&image_attachments, &slots, slot)
        .ok_or_else(|| RamariaError::validation("该图片无本地文件"))?;
    let attachment = &image_attachments[index];

    // ---- 3. 缓存优先 ----
    let cached = attachment_cache::find_cached(cache_dir, attachment.id, attachment.md5.as_deref());
    if let Some(path) = cached {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let mime = image_mime_from_bytes(&bytes);
                tracing::info!(
                    attachment_id = attachment.id,
                    from_cache = true,
                    bytes_len = bytes.len(),
                    mime = %mime,
                    "会话图片读取完成"
                );
                return Ok(AttachmentImage {
                    bytes,
                    mime,
                    attachment_id: attachment.id,
                    from_cache: true,
                });
            }
            Err(error) => {
                tracing::warn!(
                    attachment_id = attachment.id,
                    error = %error,
                    "缓存文件读取失败（回退源文件）"
                );
            }
        }
    }

    // ---- 4. 导出根定位（直连优先，md5 反查兜底） ----
    let roots = list_export_root_paths(storage.as_ref()).await;
    if roots.is_empty() {
        return Err(unavailable_error());
    }
    let mut locator = AttachmentLocator::new(roots);
    let located = locator
        .locate(&attachment.source_ref, attachment.md5.as_deref())
        .ok_or_else(unavailable_error)?;

    // ---- 5. 体积校验与源文件读取 ----
    let size = std::fs::metadata(&located.path)
        .map(|metadata| metadata.len())
        .map_err(|error| {
            tracing::warn!(
                attachment_id = attachment.id,
                error = %error,
                "图片文件状态读取失败"
            );
            unavailable_error()
        })?;
    if size > MAX_IMAGE_BYTES {
        return Err(RamariaError::validation("图片文件过大"));
    }
    let bytes = std::fs::read(&located.path).map_err(|error| {
        tracing::warn!(
            attachment_id = attachment.id,
            error = %error,
            "图片文件读取失败"
        );
        unavailable_error()
    })?;
    let mime = image_mime_from_bytes(&bytes);

    // ---- 6. best-effort 回填缓存 ----
    attachment_cache::store(cache_dir, attachment.id, attachment.md5.as_deref(), &bytes);

    tracing::info!(
        attachment_id = attachment.id,
        from_cache = false,
        bytes_len = bytes.len(),
        mime = %mime,
        "会话图片读取完成"
    );
    Ok(AttachmentImage {
        bytes,
        mime,
        attachment_id: attachment.id,
        from_cache: false,
    })
}

/// 图片不可用（未登记导出目录 / 不可定位 / 读取失败）的统一校验错误。
fn unavailable_error() -> RamariaError {
    RamariaError::validation("未登记导出目录或图片已不可用")
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
