//! crates/ramaria-service/src/browse/tests/session_preview.rs - Ramaria 会话列表预览用例
//!
//! 设计特点:
//! - 覆盖预览字段经会话列表透传到摘要视图、空会话为 None
//! - 覆盖预览正文的图片占位符按附件描述渲染
//! - 使用真实 SQLite 临时库；SQL 取数语义由存储层用例锁定，此处只验装配

use crate::test_support::{engine_with_db, seed_persona};
use crate::types::SessionBrowseRequest;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    AttachmentStatus, InboundAttachmentKind, Message, MessageAttachment, MessageRole, MessageSource,
};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

/// 构造并写入一条本地消息，返回消息实体。
async fn save_message(
    storage: &SqliteStorage,
    session_id: Uuid,
    role: MessageRole,
    content: &str,
    created_at: i64,
) -> Message {
    let mut message = Message::new(session_id, role, content.to_string(), MessageSource::Local)
        .with_persona_uid(Some("char-0001".to_string()));
    message.created_at = created_at;
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
    message
}

/// 会话列表透传最后一条可见消息；空会话为 None。
#[tokio::test]
async fn session_list_exposes_last_visible_message_preview() {
    let (engine, storage, dir) = engine_with_db("browse-session-preview").await;
    seed_persona(&storage, "char-0001").await;

    let with_messages = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    save_message(
        &storage,
        with_messages.id,
        MessageRole::User,
        "第一条",
        1_000,
    )
    .await;
    save_message(
        &storage,
        with_messages.id,
        MessageRole::Assistant,
        "最新一条",
        1_100,
    )
    .await;

    let empty = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    let recalled_last = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    save_message(
        &storage,
        recalled_last.id,
        MessageRole::User,
        "可回退",
        2_000,
    )
    .await;
    let recalled = save_message(
        &storage,
        recalled_last.id,
        MessageRole::Assistant,
        "已撤回",
        2_100,
    )
    .await;
    storage
        .mark_message_recalled_by_id(recalled.id, 2_200)
        .await
        .expect("撤回标记应成功");

    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await
        .expect("会话列表应成功");

    let item = |id: Uuid| {
        page.items
            .iter()
            .find(|item| item.id == id)
            .expect("会话应出现在列表中")
    };
    assert_eq!(
        item(with_messages.id).last_message_preview.as_deref(),
        Some("最新一条"),
        "预览应取最后一条可见消息"
    );
    assert!(
        item(empty.id).last_message_preview.is_none(),
        "空会话预览应为 None"
    );
    assert_eq!(
        item(recalled_last.id).last_message_preview.as_deref(),
        Some("可回退"),
        "已撤回消息不作为预览"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 预览正文的图片占位符按附件描述渲染（与会话消息浏览同口径）。
#[tokio::test]
async fn session_list_renders_image_placeholder_in_preview() {
    let (engine, storage, dir) = engine_with_db("browse-session-preview-attachment").await;
    seed_persona(&storage, "char-0001").await;

    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let message = save_message(
        &storage,
        session.id,
        MessageRole::User,
        "看图 [图片#abcd1234]",
        1_000,
    )
    .await;
    storage
        .insert_message_attachments(&[MessageAttachment {
            id: 0,
            message_id: message.id,
            kind: InboundAttachmentKind::Image,
            source_ref: "resources/images/a.jpg".to_string(),
            md5: Some("abcd1234deadbeefdeadbeefdeadbeef".to_string()),
            size: None,
            width: None,
            height: None,
            sub_type: Some("photo".to_string()),
            status: AttachmentStatus::Done,
            description: Some("一片海".to_string()),
            description_model: Some("mock-vision".to_string()),
            created_at: 1_000,
            updated_at: 1_000,
        }])
        .await
        .expect("写入附件应成功");

    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await
        .expect("会话列表应成功");
    let item = page
        .items
        .iter()
        .find(|item| item.id == session.id)
        .expect("会话应出现在列表中");
    assert_eq!(
        item.last_message_preview.as_deref(),
        Some("看图 [图片: 一片海]"),
        "占位符应按附件描述渲染"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
