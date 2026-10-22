//! crates/ramaria-service/src/session/tests.rs - Ramaria 会话用例单元测试
//!
//! 设计特点:
//! - 覆盖历史分页 / 人格定位 / 空输入与视图字段
//! - 覆盖会话创建的绑定与未绑定口径、删除幂等与级联清理
//! - 覆盖发送目标解析：活跃沿用、已关闭或不存在时自动重建
//! - 用例数据由 test_support 夹具构造，全部经 Engine 门面调用

use super::*;
use crate::test_support::{
    engine_with_db, seed_closed_session_with_messages, seed_messages, seed_persona,
    seed_session_with_messages, seed_utt_block,
};
use crate::types::ChatRole;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{MemoryL1, MessageRole};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

/// 造会话 + N 条消息（created_at 自 base 起逐条 +1）。
async fn seed_session(storage: &SqliteStorage, persona: &str, count: usize, base_ts: i64) -> Uuid {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话");
    seed_messages(storage, session.id, persona, count, base_ts).await;
    session.id
}

/// 按 session_id 分页：页内时间正序、total 正确、offset 越界为空。
#[tokio::test]
async fn history_pages_within_session() {
    let (engine, storage, dir) = engine_with_db("history").await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session(&storage, "char-0001", 7, 1_000).await;

    // 第一页：最新 3 条（时间正序 = 1004..1006）
    let page1 = engine
        .history(HistoryRequest {
            session_id: Some(session_id),
            limit: Some(3),
            offset: None,
            persona: None,
        })
        .await
        .expect("历史读取成功");
    assert_eq!(page1.session_id, Some(session_id));
    assert_eq!(page1.total, 7);
    assert_eq!(page1.messages.len(), 3);
    assert_eq!(page1.messages[0].content, "消息内容 4", "页内时间正序");
    assert_eq!(page1.messages[2].content, "消息内容 6");

    // 第二页：offset 3 → 1001..1003
    let page2 = engine
        .history(HistoryRequest {
            session_id: Some(session_id),
            limit: Some(3),
            offset: Some(3),
            persona: None,
        })
        .await
        .expect("历史读取成功");
    assert_eq!(page2.messages[0].content, "消息内容 1");

    // 越界 → 空页但 total 保留
    let beyond = engine
        .history(HistoryRequest {
            session_id: Some(session_id),
            limit: Some(3),
            offset: Some(99),
            persona: None,
        })
        .await
        .expect("历史读取成功");
    assert!(beyond.messages.is_empty());
    assert_eq!(beyond.total, 7);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 按 persona 定位：取该人格最近消息所属会话。
#[tokio::test]
async fn history_resolves_latest_session_by_persona() {
    let (engine, storage, dir) = engine_with_db("history-persona").await;
    seed_persona(&storage, "char-0001").await;
    let first = seed_session(&storage, "char-0001", 2, 1_000).await;
    let latest = seed_session(&storage, "char-0001", 2, 2_000).await;

    let result = engine
        .history(HistoryRequest {
            session_id: None,
            persona: Some("char-0001".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .expect("历史读取成功");

    assert_eq!(result.session_id, Some(latest), "应定位最近会话");
    assert_ne!(result.session_id, Some(first));
    assert_eq!(result.total, 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 空请求 / 无数据人格 → 空结构（不报错）。
#[tokio::test]
async fn history_empty_inputs_return_empty() {
    let (engine, storage, dir) = engine_with_db("history-empty").await;
    seed_persona(&storage, "char-0001").await;

    let no_input = engine
        .history(HistoryRequest::default())
        .await
        .expect("历史读取成功");
    assert!(no_input.session_id.is_none());
    assert!(no_input.messages.is_empty());
    assert_eq!(no_input.total, 0);

    let no_messages = engine
        .history(HistoryRequest {
            session_id: None,
            persona: Some("char-0001".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .expect("历史读取成功");
    assert!(no_messages.session_id.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 消息视图字段完整（角色 / 内容 / 时间 / 发言人）。
#[tokio::test]
async fn history_message_view_fields() {
    let (engine, storage, dir) = engine_with_db("history-fields").await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session(&storage, "char-0001", 2, 5_000).await;

    let result = engine
        .history(HistoryRequest {
            session_id: Some(session_id),
            limit: None,
            offset: None,
            persona: None,
        })
        .await
        .expect("历史读取成功");

    assert_eq!(result.messages[0].role, MessageRole::User);
    assert_eq!(result.messages[0].persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(
        result.messages[0].time.timestamp_millis(),
        5_000,
        "时间应与存储一致"
    );
    // ChatRole 与内核角色的映射保持可用（供上层判断"最后一条用户消息"）
    assert_eq!(ChatRole::User.as_str(), "user");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话创建：绑定人格 / 不绑定两种口径，字段符合核心会话约定。
#[tokio::test]
async fn create_session_binds_persona_and_local_channel() {
    let (engine, _storage, dir) = engine_with_db("session-create").await;

    let bound = engine
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    assert_eq!(bound.persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(bound.channel, "local", "本地创建走 local 通道");
    assert!(bound.ended_at.is_none(), "新会话应处于进行中");
    assert!(bound.started_at > 0);

    let unbound = engine.create_session(None).await.expect("创建会话应成功");
    assert!(unbound.persona_uid.is_none());
    assert_ne!(bound.id, unbound.id, "两次创建应得到不同会话");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 会话删除：删除后不可查；不存在的会话幂等成功（与存储层语义一致）。
#[tokio::test]
async fn delete_session_removes_row_idempotently() {
    let (engine, storage, dir) = engine_with_db("session-delete").await;
    let session = engine.create_session(None).await.expect("创建会话应成功");

    engine.delete_session(session.id).await.expect("删除应成功");
    assert!(
        storage
            .get_session(session.id)
            .await
            .expect("查询应成功")
            .is_none(),
        "删除后会话不应可见"
    );

    // 不存在（含刚删除的）会话：幂等成功
    engine
        .delete_session(session.id)
        .await
        .expect("重复删除应幂等成功");
    engine
        .delete_session(Uuid::nil())
        .await
        .expect("不存在会话应幂等成功");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 级联删除：会话与消息 / utt 块 / L1 摘要整体移除；不存在的会话幂等成功。
#[tokio::test]
async fn delete_session_cascade_removes_related_rows() {
    let (engine, storage, dir) = engine_with_db("session-cascade").await;
    seed_persona(&storage, "char-0001").await;
    let session_id = seed_session(&storage, "char-0001", 3, 1_000).await;
    seed_utt_block(&storage, session_id, "char-0001", "原文块").await;
    let mut l1 = MemoryL1::new(session_id, "摘要".to_string(), None);
    l1.persona_uid = Some("char-0001".to_string());
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

    engine
        .delete_session_cascade(session_id)
        .await
        .expect("级联删除应成功");

    assert!(
        storage
            .get_session(session_id)
            .await
            .expect("查询应成功")
            .is_none()
    );
    assert!(
        storage
            .list_messages(session_id)
            .await
            .expect("查询消息应成功")
            .is_empty(),
        "级联删除后不应残留消息"
    );
    assert!(
        storage
            .list_memory_l1(session_id)
            .await
            .expect("查询 L1 应成功")
            .is_empty(),
        "级联删除后不应残留 L1"
    );
    assert!(
        storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .expect("查询块应成功")
            .is_empty(),
        "级联删除后不应残留 utt 块"
    );

    // 不存在会话：幂等成功
    engine
        .delete_session_cascade(Uuid::nil())
        .await
        .expect("不存在会话应幂等成功");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 发送目标解析：活跃会话原样返回；已关闭 / 不存在 / 未指定均新建并绑定人格。
#[tokio::test]
async fn resolve_send_session_keeps_active_and_rebuilds_closed_or_missing() {
    let (engine, storage, dir) = engine_with_db("session-resolve-send").await;
    seed_persona(&storage, "char-0001").await;

    // 活跃会话：原样返回（不新建）
    let active = seed_session_with_messages(&storage, "char-0001", 1, 1_000).await;
    let resolved = engine
        .resolve_send_session(Some("char-0001"), Some(active))
        .await
        .expect("解析应成功");
    assert_eq!(resolved, active, "活跃会话应原样返回");

    // 已关闭会话：新建（绑定人格、未关闭）
    let closed = seed_closed_session_with_messages(&storage, "char-0001", 1, 2_000).await;
    let rebuilt = engine
        .resolve_send_session(Some("char-0001"), Some(closed))
        .await
        .expect("解析应成功");
    assert_ne!(rebuilt, closed, "已关闭会话应触发新建");
    let row = storage
        .get_session(rebuilt)
        .await
        .expect("查询应成功")
        .expect("新会话应存在");
    assert_eq!(row.persona_uid.as_deref(), Some("char-0001"));
    assert!(row.ended_at.is_none(), "新会话应处于进行中");
    let original = storage
        .get_session(closed)
        .await
        .expect("查询应成功")
        .expect("原会话应存在");
    assert!(original.ended_at.is_some(), "原会话状态不应被改写");

    // 不存在的会话：新建（未提供人格时不绑定）
    let missing = Uuid::new_v4();
    let rebuilt2 = engine
        .resolve_send_session(None, Some(missing))
        .await
        .expect("解析应成功");
    assert_ne!(rebuilt2, missing);
    let row2 = storage
        .get_session(rebuilt2)
        .await
        .expect("查询应成功")
        .expect("新会话应存在");
    assert!(row2.persona_uid.is_none(), "未提供人格时新会话暂不绑定");

    // 未指定会话：新建（绑定人格）
    let created = engine
        .resolve_send_session(Some("char-0001"), None)
        .await
        .expect("解析应成功");
    let row3 = storage
        .get_session(created)
        .await
        .expect("查询应成功")
        .expect("新会话应存在");
    assert_eq!(row3.persona_uid.as_deref(), Some("char-0001"));

    let _ = std::fs::remove_dir_all(&dir);
}
