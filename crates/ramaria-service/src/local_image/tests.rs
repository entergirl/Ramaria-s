//! crates/ramaria-service/src/local_image/tests.rs - Ramaria 本地发图用例测试
//!
//! 设计特点:
//! - 真实 SQLite 临时库 + 临时缓存目录：断言消息 / 附件行 / 缓存文件三侧
//! - 覆盖边界拒绝（空字节 / 非图片字节 / 社交通道会话 / 已关闭会话 / 会话不存在）

use super::*;
use crate::test_support::{engine_with_db, seed_persona};
use crate::types::{DEFAULT_PERSONA_UID, LocalImageSendRequest};
use ramaria_core::traits::StoreCrud;
use uuid::Uuid;

/// 最小合法 PNG 头字节（魔数识别口径）。
const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x03, 0x04];

/// 构造发图请求（默认带说明）。
fn request(session_id: Uuid, bytes: &[u8], caption: Option<&str>) -> LocalImageSendRequest {
    LocalImageSendRequest {
        session_id,
        file_name: "cat.png".to_string(),
        bytes: bytes.to_vec(),
        caption: caption.map(str::to_string),
    }
}

/// 往返：消息 + 附件行 + 缓存文件三侧一致。
#[tokio::test]
async fn send_roundtrip_persists_message_attachment_and_cache() {
    let (engine, storage, dir) = engine_with_db("local-image-roundtrip").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let session = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("建会话应成功");

    let outcome = send(&engine, request(session.id, PNG_BYTES, Some("看这个")))
        .await
        .expect("本地发图应成功");
    assert!(
        outcome.placeholder.starts_with("[图片#"),
        "应产出图片占位符: {}",
        outcome.placeholder
    );
    assert_eq!(
        outcome.content,
        format!("看这个{}", outcome.placeholder),
        "说明在前、占位符在后"
    );

    // 消息行
    let messages = storage
        .list_messages(session.id)
        .await
        .expect("查询消息应成功");
    assert_eq!(messages.len(), 1, "应新增一条用户消息");
    let message = &messages[0];
    assert_eq!(message.id, outcome.message_id);
    assert_eq!(message.role, MessageRole::User);
    assert_eq!(message.content, outcome.content);
    assert_eq!(message.source, MessageSource::Local);
    assert_eq!(message.persona_uid.as_deref(), Some(DEFAULT_PERSONA_UID));

    // 附件行（pending，待理解回填）
    let rows = storage
        .list_attachments_by_messages(&[outcome.message_id])
        .await
        .expect("查询附件应成功");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, outcome.attachment_id);
    assert_eq!(rows[0].status, AttachmentStatus::Pending);
    assert_eq!(rows[0].source_ref, "cat.png");
    assert_eq!(rows[0].size, Some(PNG_BYTES.len() as u64));

    // 结果携带 md5（缓存命名与理解链路共用）
    let md5 = md5_hex(PNG_BYTES);
    assert_eq!(outcome.md5, md5, "发图结果应携带图片 md5");
    let cache_file = dir.join("image_cache").join(format!("{md5}.png"));
    assert!(cache_file.is_file(), "缓存文件应落盘: {cache_file:?}");
    assert_eq!(
        std::fs::read(&cache_file).expect("读取缓存应成功"),
        PNG_BYTES
    );

    // 无说明：正文仅占位符
    let bare = send(&engine, request(session.id, PNG_BYTES, None))
        .await
        .expect("无说明发图应成功");
    assert_eq!(bare.content, bare.placeholder);

    let _ = std::fs::remove_dir_all(dir);
}

/// 边界拒绝：空字节 / 非图片字节 / 社交通道会话 / 已关闭会话 / 会话不存在。
#[tokio::test]
async fn send_rejects_invalid_inputs() {
    let (engine, storage, dir) = engine_with_db("local-image-reject").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let local = storage.create_session(None).await.expect("建会话应成功");

    // 空字节
    let err = send(&engine, request(local.id, &[], None))
        .await
        .expect_err("空字节应拒绝");
    assert!(err.to_string().contains("图片字节不能为空"), "{err}");

    // 非图片字节
    let err = send(&engine, request(local.id, b"not an image", None))
        .await
        .expect_err("非图片应拒绝");
    assert!(err.to_string().contains("不是可识别的图片格式"), "{err}");

    // 社交通道会话
    let social = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:1"))
        .await
        .expect("建通道会话应成功");
    let err = send(&engine, request(social.id, PNG_BYTES, None))
        .await
        .expect_err("社交通道应拒绝");
    assert!(
        err.to_string().contains("社交平台会话不支持本地发送图片"),
        "{err}"
    );

    // 已关闭会话
    storage
        .close_session(local.id)
        .await
        .expect("关闭会话应成功");
    let err = send(&engine, request(local.id, PNG_BYTES, None))
        .await
        .expect_err("已关闭会话应拒绝");
    assert!(err.to_string().contains("会话已关闭"), "{err}");

    // 会话不存在
    let err = send(&engine, request(Uuid::new_v4(), PNG_BYTES, None))
        .await
        .expect_err("会话不存在应拒绝");
    assert!(err.to_string().contains("会话不存在"), "{err}");

    let _ = std::fs::remove_dir_all(dir);
}
