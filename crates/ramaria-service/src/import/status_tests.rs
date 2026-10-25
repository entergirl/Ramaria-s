//! crates/ramaria-service/src/import/status_tests.rs - Ramaria 导入续跑状态查询用例
//!
//! 设计特点:
//! - 真实临时库：直接落会话 / 消息 / 附件，覆盖三类待办计数与 `resumable` 口径
//! - 与"继续处理"同口径：补上 L1 后缺失会话计数归零
//! - 只断言持久化规模；进程内 `post_active` 由并行用例共享，不做真假断言
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网。

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    AttachmentStatus, InboundAttachmentKind, MemoryL1, Message, MessageAttachment, MessageRole,
    MessageSource, now_ms,
};
use uuid::Uuid;

use super::*;
use crate::test_support::{MockLlm, engine_with_shared_llm};

// ---- 测试脚手架 ----

/// 装配"真实临时库 + mock LLM"引擎。
async fn status_env() -> (
    Engine,
    Arc<ramaria_storage::SqliteStorage>,
    std::path::PathBuf,
) {
    let llm = Arc::new(MockLlm::local());
    engine_with_shared_llm("import-status", llm, RamariaConfig::default(), None).await
}

/// 造一条带附件的消息，返回消息 id。
async fn seed_attachment(
    storage: &ramaria_storage::SqliteStorage,
    session_id: Uuid,
    status: AttachmentStatus,
) -> Uuid {
    let message = Message::new(
        session_id,
        MessageRole::User,
        "看这张图".to_string(),
        MessageSource::Local,
    );
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");

    let attachment = MessageAttachment {
        id: 0,
        message_id: message.id,
        kind: InboundAttachmentKind::Image,
        source_ref: "resources/images/a.jpg".to_string(),
        md5: None,
        size: None,
        width: None,
        height: None,
        sub_type: None,
        status,
        description: None,
        description_model: None,
        created_at: now_ms(),
        updated_at: now_ms(),
    };
    storage
        .insert_message_attachments(&[attachment])
        .await
        .expect("写入附件应成功");
    message.id
}

// ---- 用例 ----

/// 空库：三类待办为 0，`resumable` 为 false。
#[tokio::test]
async fn empty_database_is_not_resumable() {
    let _serial = super::test_lock::serial().await;
    let (engine, _storage, dir) = status_env().await;

    let status = engine.import_status().await.expect("查询导入状态应成功");
    assert_eq!(status.pending_attachments, 0);
    assert_eq!(status.skipped_attachments, 0);
    assert_eq!(status.missing_l1_sessions, 0);
    assert!(!status.resumable(), "无待办时不应可继续处理");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 附件待处理 / 跳过与缺摘要会话分别计数，任一大于 0 即可继续处理。
#[tokio::test]
async fn pending_and_skipped_attachments_and_missing_l1_are_counted() {
    let _serial = super::test_lock::serial().await;
    let (engine, storage, dir) = status_env().await;

    // 已关闭且有消息、无 L1 的本地会话
    let session = storage.create_session(None).await.expect("创建会话应成功");
    seed_attachment(&storage, session.id, AttachmentStatus::Pending).await;
    seed_attachment(&storage, session.id, AttachmentStatus::Skipped).await;
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");

    let status = engine.import_status().await.expect("查询导入状态应成功");
    assert_eq!(status.pending_attachments, 1, "待处理附件应计 1");
    assert_eq!(status.skipped_attachments, 1, "跳过附件应计 1");
    assert_eq!(status.missing_l1_sessions, 1, "缺摘要会话应计 1");
    assert!(status.resumable(), "存在待办时应可继续处理");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 补上 L1 后缺摘要会话计数归零，`resumable` 只看剩余待办。
#[tokio::test]
async fn missing_l1_count_drops_after_summary_written() {
    let _serial = super::test_lock::serial().await;
    let (engine, storage, dir) = status_env().await;

    let session = storage.create_session(None).await.expect("创建会话应成功");
    let message = Message::new(
        session.id,
        MessageRole::User,
        "今天有点累".to_string(),
        MessageSource::Local,
    );
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");

    let before = engine.import_status().await.expect("查询导入状态应成功");
    assert_eq!(before.missing_l1_sessions, 1);

    let l1 = MemoryL1::new(session.id, "今天有点累".to_string(), None);
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    let after = engine.import_status().await.expect("查询导入状态应成功");
    assert_eq!(after.missing_l1_sessions, 0, "已补摘要的会话不再计入");
    assert!(!after.resumable(), "无任何待办时应不可继续处理");

    let _ = std::fs::remove_dir_all(&dir);
}
