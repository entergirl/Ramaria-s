//! crates/ramaria-channel/src/runtime/assemble/tests.rs - 入站消息组装单元测试
//!
//! 设计特点:
//! - 纯函数断言：段序列与下载结果的映射、占位符 hash、附件引用字段
//! - 降级口径：下载失败不产生附件；多图顺序一一对应

use super::*;
use ramaria_core::types::{InboundAttachmentKind, InboundSender};

/// 构造带指定段序列的事件（其余字段固定）。
fn event_with(segments: Vec<InboundSegment>) -> TransportEvent {
    TransportEvent {
        external_ref: "c2c:10001".to_string(),
        sender: InboundSender {
            platform_id: "10001".to_string(),
            uin: Some("10001".to_string()),
            display_name: "小明".to_string(),
            group_nickname: None,
            role: None,
        },
        platform_message_id: Some("m-1".to_string()),
        reply_to: Some("m-0".to_string()),
        created_at: 1_700_000_000_000,
        segments,
        should_respond: true,
        self_id: Some("42".to_string()),
    }
}

/// 构造下载成功结果（md5 由字节计算）。
fn downloaded(bytes: &[u8]) -> DownloadedImage {
    DownloadedImage {
        md5: md5_hex(bytes),
        bytes: bytes.to_vec(),
    }
}

/// md5 已知向量（空串与 "abc"）。
#[test]
fn md5_hex_known_vectors() {
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
}

/// 纯文本：正文拼接、无附件。
#[test]
fn plain_text_assembles_without_attachments() {
    let event = event_with(vec![
        InboundSegment::Text("你好".to_string()),
        InboundSegment::Text("，在吗".to_string()),
    ]);
    let assembled = assemble_inbound(&event, "qq", &[]);

    assert_eq!(assembled.message.text, "你好，在吗");
    assert!(assembled.message.attachments.is_empty());
    assert!(assembled.downloaded.is_empty());
    assert_eq!(assembled.message.channel, "qq");
    assert_eq!(
        assembled.message.platform_message_id.as_deref(),
        Some("m-1")
    );
    assert_eq!(assembled.message.reply_to.as_deref(), Some("m-0"));
    assert_eq!(assembled.message.created_at, 1_700_000_000_000);
    assert_eq!(assembled.message.sender.display_name, "小明");
}

/// 单图成功：占位符为 md5 前 8 位小写；附件引用携带 md5 / url / 尺寸。
#[test]
fn single_image_assembles_placeholder_and_attachment() {
    let image = downloaded(b"fake-png-bytes");
    let full_md5 = image.md5.clone();
    let hash = full_md5[..8].to_string();
    let segments = vec![
        InboundSegment::Text("看这个".to_string()),
        InboundSegment::Image {
            url: "https://example.com/a.png".to_string(),
            size: Some(2048),
            sub_type: Some("0".to_string()),
        },
    ];
    let assembled = assemble_inbound(&event_with(segments), "qq", &[Some(image)]);

    assert_eq!(assembled.message.text, format!("看这个[图片#{hash}]"));
    assert_eq!(assembled.message.attachments.len(), 1);
    let attachment = &assembled.message.attachments[0];
    assert_eq!(attachment.kind, InboundAttachmentKind::Image);
    assert_eq!(attachment.source_ref, "https://example.com/a.png");
    assert_eq!(
        attachment.md5.as_deref(),
        Some(full_md5.as_str()),
        "附件 md5 为完整指纹"
    );
    assert_eq!(attachment.size, Some(2048));
    assert_eq!(attachment.sub_type.as_deref(), Some("0"));
    assert_eq!(assembled.downloaded.len(), 1);
    assert_eq!(assembled.downloaded[0].md5, full_md5);
}

/// 下载失败：正文降级 `[图片]`，无附件引用。
#[test]
fn failed_download_degrades_to_plain_placeholder() {
    let segments = vec![
        InboundSegment::Text("这张图".to_string()),
        InboundSegment::Image {
            url: "https://example.com/b.png".to_string(),
            size: None,
            sub_type: None,
        },
    ];
    let assembled = assemble_inbound(&event_with(segments), "qq", &[None]);

    assert_eq!(assembled.message.text, "这张图[图片]");
    assert!(assembled.message.attachments.is_empty());
    assert!(assembled.downloaded.is_empty());
}

/// 多图顺序一一对应：成功与失败混排时附件只含成功项且保持相对顺序。
#[test]
fn multiple_images_keep_order_with_partial_failures() {
    let first = downloaded(b"first");
    let second = downloaded(b"second");
    let segments = vec![
        InboundSegment::Image {
            url: "https://example.com/1.png".to_string(),
            size: None,
            sub_type: None,
        },
        InboundSegment::Text("中".to_string()),
        InboundSegment::Image {
            url: "https://example.com/2.png".to_string(),
            size: None,
            sub_type: None,
        },
        InboundSegment::Image {
            url: "https://example.com/3.png".to_string(),
            size: None,
            sub_type: None,
        },
    ];
    let images = vec![Some(first.clone()), None, Some(second.clone())];
    let assembled = assemble_inbound(&event_with(segments), "qq", &images);

    let expected = format!(
        "[图片#{}]中[图片][图片#{}]",
        &first.md5[..8],
        &second.md5[..8]
    );
    assert_eq!(assembled.message.text, expected);
    assert_eq!(assembled.message.attachments.len(), 2);
    assert_eq!(
        assembled.message.attachments[0].source_ref,
        "https://example.com/1.png"
    );
    assert_eq!(
        assembled.message.attachments[1].source_ref,
        "https://example.com/3.png"
    );
    assert_eq!(assembled.downloaded, vec![first, second]);
}

/// 下载结果数量少于图片段：缺位视为下载失败。
#[test]
fn missing_download_slots_treated_as_failure() {
    let segments = vec![InboundSegment::Image {
        url: "https://example.com/a.png".to_string(),
        size: None,
        sub_type: None,
    }];
    let assembled = assemble_inbound(&event_with(segments), "qq", &[]);
    assert_eq!(assembled.message.text, "[图片]");
    assert!(assembled.message.attachments.is_empty());
}
