//! crates/ramaria-channel/src/runtime/assemble.rs - Ramaria 入站消息组装模块
//!
//! 设计特点:
//! - 段序列 + 下载结果 → core 规范入站消息（图片占位符 + 附件引用）
//! - 图片占位符使用 md5 前 8 位小写（与读取口渲染口径一致）
//! - 下载失败降级为纯文本占位（`[图片]`），不产生附件引用
//! - 下载结果与图片段按出现顺序一一对应（None = 下载失败）

use md5::{Digest, Md5};
use ramaria_core::types::{
    InboundAttachmentKind, InboundAttachmentRef, InboundMessage, image_placeholder_hash,
};

use crate::transport::{InboundSegment, TransportEvent};

// =========================================================
// 组装类型
// =========================================================

/// 下载成功图片的字节与指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedImage {
    pub md5: String,
    pub bytes: Vec<u8>,
}

/// 组装产出。
///
/// 字段约定:
/// - `message`: core 规范入站消息（正文含占位符，附件引用带 md5）。
/// - `downloaded`: 下载成功图片（与 `message.attachments` 一一对应、同序）。
#[derive(Debug, Clone, PartialEq)]
pub struct AssembledInbound {
    pub message: InboundMessage,
    pub downloaded: Vec<DownloadedImage>,
}

// =========================================================
// 组装
// =========================================================

/// 计算字节的 md5（小写 hex）。
pub fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 段序列 + 下载结果 → 入站消息。
///
/// 规则:
/// - Text 段直接拼接；
/// - Image 段：下载成功 → `[图片#{hash}]` 占位符 + 附件引用（`source_ref` 为下载地址）；
///   下载失败 → `[图片]` 纯文本（无附件引用）；
/// - `images` 与图片段按出现顺序一一对应（序号外的缺失视为下载失败）。
///
/// 参数:
/// - `event`: 规范入站事件。
/// - `channel`: 通道标识（写入 `InboundMessage.channel`）。
/// - `images`: 图片段的下载结果（按图片段出现顺序）。
///
/// 返回:
/// - 组装产出（正文可能为空串，调用方负责拒绝空内容）。
pub fn assemble_inbound(
    event: &TransportEvent,
    channel: &str,
    images: &[Option<DownloadedImage>],
) -> AssembledInbound {
    let mut text = String::new();
    let mut attachments = Vec::new();
    let mut downloaded = Vec::new();
    let mut image_index = 0usize;

    for segment in &event.segments {
        match segment {
            InboundSegment::Text(part) => text.push_str(part),
            InboundSegment::Image {
                url,
                size,
                sub_type,
            } => {
                let image = images.get(image_index).and_then(|slot| slot.as_ref());
                image_index += 1;
                match image {
                    Some(image) => {
                        text.push_str(&format!("[图片#{}]", image_placeholder_hash(&image.md5)));
                        attachments.push(InboundAttachmentRef {
                            kind: InboundAttachmentKind::Image,
                            source_ref: url.clone(),
                            md5: Some(image.md5.clone()),
                            size: *size,
                            width: None,
                            height: None,
                            sub_type: sub_type.clone(),
                        });
                        downloaded.push(DownloadedImage {
                            md5: image.md5.clone(),
                            bytes: image.bytes.clone(),
                        });
                    }
                    None => text.push_str("[图片]"),
                }
            }
        }
    }

    AssembledInbound {
        message: InboundMessage {
            channel: channel.to_string(),
            platform_message_id: event.platform_message_id.clone(),
            reply_to: event.reply_to.clone(),
            created_at: event.created_at,
            text,
            sender: event.sender.clone(),
            attachments,
        },
        downloaded,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
