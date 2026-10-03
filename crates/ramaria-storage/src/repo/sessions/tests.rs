//! crates/ramaria-storage/src/repo/sessions/tests.rs - 会话存取单元测试
//!
//! 设计特点:
//! - 覆盖级联删除与缺失会话的幂等
//! - 覆盖通道会话创建与本地默认通道
//! - 覆盖按通道 / 外部标识查询活跃会话
//! - 覆盖抢占式条件关闭与通道概览统计

use super::*;
use crate::database::init_test_pool;
use ramaria_core::types::{Message, MessageRole, MessageSource};

/// 插入 persona + session + 消息 + utt 块 fixture（验证级联删除的外键依赖顺序）。
///
/// 返回 session_id。utt_blocks 同时引用 messages（start/end_msg_id）与 sessions，
/// 若不先删 utt_blocks 直接删 session 会被外键约束拒绝。
async fn setup_fixture(pool: &SqlitePool) -> Uuid {
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
         VALUES ('char-0001', '测试', 'char', 1, 'local', 0, 0)",
    )
    .execute(pool)
    .await
    .expect("插入 persona fixture 应成功");

    let session = create(pool, Some("char-0001"))
        .await
        .expect("创建 session 成功");
    let m1 = Message::new(
        session.id,
        MessageRole::User,
        "问题一".to_string(),
        MessageSource::Local,
    );
    let m2 = Message::new(
        session.id,
        MessageRole::Assistant,
        "回复一".to_string(),
        MessageSource::Online,
    )
    .with_persona_uid(Some("char-0001".to_string()));
    crate::repo::messages::save_import(pool, &m1)
        .await
        .expect("插入消息 1 成功");
    crate::repo::messages::save_import(pool, &m2)
        .await
        .expect("插入消息 2 成功");

    sqlx::query(
        "INSERT INTO utt_blocks \
         (persona_uid, session_id, start_msg_id, end_msg_id, block_text, msg_count, \
          time_span_ms, embedding, created_at) \
         VALUES ('char-0001', ?, ?, ?, '测试话语块', 2, 1000, NULL, 0)",
    )
    .bind(session.id.to_string())
    .bind(m1.id.to_string())
    .bind(m2.id.to_string())
    .execute(pool)
    .await
    .expect("插入 utt_blocks fixture 应成功");

    session.id
}

/// 级联删除应移除 session 及其消息与 utt 块（utt 块引用顺序正确、无外键残留）。
#[tokio::test]
async fn delete_cascade_removes_session_and_dependents() {
    let pool = init_test_pool().await.expect("测试库初始化成功");
    let session_id = setup_fixture(&pool).await;

    // 前置：session、2 条消息、1 个 utt 块均存在
    assert!(get(&pool, session_id).await.unwrap().is_some());
    assert_eq!(
        crate::repo::messages::count_by_session(&pool, session_id)
            .await
            .unwrap(),
        2
    );

    delete_cascade(&pool, session_id)
        .await
        .expect("级联删除成功");

    assert!(
        get(&pool, session_id).await.unwrap().is_none(),
        "session 应已删除"
    );
    assert_eq!(
        crate::repo::messages::count_by_session(&pool, session_id)
            .await
            .unwrap(),
        0,
        "session 消息应已级联删除"
    );
    let utt_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM utt_blocks WHERE session_id = ?")
        .bind(session_id.to_string())
        .fetch_one(&pool)
        .await
        .expect("查询 utt_blocks 数量成功");
    assert_eq!(utt_count.0, 0, "session 的 utt 块应已删除");
}

/// 删除不存在的 session 幂等成功（不报错）。
#[tokio::test]
async fn delete_cascade_missing_session_is_idempotent() {
    let pool = init_test_pool().await.expect("测试库初始化成功");
    delete_cascade(&pool, Uuid::new_v4())
        .await
        .expect("删除不存在的 session 应幂等成功");
}

// =========================================================
// 通道支持（channel / external_ref）
// =========================================================

/// 新建带通道会话：通道与外部标识正确回读；本地会话取默认通道。
#[tokio::test]
async fn create_in_channel_roundtrip_and_local_default() {
    let pool = init_test_pool().await.expect("测试库初始化成功");

    let session = create_in_channel(&pool, Some("rama-0001"), "mcp", Some("client-A"))
        .await
        .expect("创建带通道 session 成功");
    assert_eq!(session.channel, "mcp");
    assert_eq!(session.external_ref.as_deref(), Some("client-A"));

    let fetched = get(&pool, session.id)
        .await
        .expect("查询成功")
        .expect("session 应存在");
    assert_eq!(fetched.channel, "mcp");
    assert_eq!(fetched.external_ref.as_deref(), Some("client-A"));

    // 本地创建走默认通道 'local'（存量行为不变）
    let local = create(&pool, Some("rama-0001"))
        .await
        .expect("创建本地 session 成功");
    assert_eq!(local.channel, "local");
    assert!(local.external_ref.is_none());
    let fetched_local = get(&pool, local.id)
        .await
        .expect("查询成功")
        .expect("本地 session 应存在");
    assert_eq!(fetched_local.channel, "local");
    assert!(fetched_local.external_ref.is_none());
}

/// 按 `(channel, external_ref)` 查询：命中活跃会话；无匹配返回 None；已关闭不返回。
#[tokio::test]
async fn find_active_by_channel_hits_and_misses() {
    let pool = init_test_pool().await.expect("测试库初始化成功");

    let session = create_in_channel(&pool, Some("rama-0001"), "mcp", Some("client-A"))
        .await
        .expect("创建带通道 session 成功");

    // 命中：同通道 + 同外部标识
    let hit = find_active_by_channel(&pool, "mcp", Some("client-A"))
        .await
        .expect("查询成功");
    assert_eq!(hit.map(|s| s.id), Some(session.id));

    // 无匹配：不同通道 / 不同标识 / 不同 NULL 形态
    assert!(
        find_active_by_channel(&pool, "telegram", Some("client-A"))
            .await
            .expect("查询成功")
            .is_none(),
        "不同通道不应命中"
    );
    assert!(
        find_active_by_channel(&pool, "mcp", Some("client-B"))
            .await
            .expect("查询成功")
            .is_none(),
        "不同外部标识不应命中"
    );
    assert!(
        find_active_by_channel(&pool, "mcp", None)
            .await
            .expect("查询成功")
            .is_none(),
        "无标识查询不应命中带标识会话"
    );

    // 关闭后不再命中（活跃过滤）
    close(&pool, session.id).await.expect("关闭 session 成功");
    assert!(
        find_active_by_channel(&pool, "mcp", Some("client-A"))
            .await
            .expect("查询成功")
            .is_none(),
        "已关闭会话不应命中"
    );
}

/// 无标识通道会话（external_ref = NULL）可被 `None` 查询命中。
#[tokio::test]
async fn find_active_by_channel_matches_null_ref() {
    let pool = init_test_pool().await.expect("测试库初始化成功");

    let session = create_in_channel(&pool, None, "mcp", None)
        .await
        .expect("创建无标识通道 session 成功");
    let hit = find_active_by_channel(&pool, "mcp", None)
        .await
        .expect("查询成功");
    assert_eq!(hit.map(|s| s.id), Some(session.id));
}

/// 抢占式条件关闭：首次抢到置 ended_at，二次调用返回 false（幂等，不重复关闭）。
#[tokio::test]
async fn close_if_active_is_single_winner() {
    let pool = init_test_pool().await.expect("测试库初始化成功");
    let session = create(&pool, Some("rama-0001"))
        .await
        .expect("创建 session 成功");

    assert!(
        close_if_active(&pool, session.id)
            .await
            .expect("条件关闭成功"),
        "首次调用应抢到关闭权"
    );
    assert!(
        !close_if_active(&pool, session.id)
            .await
            .expect("条件关闭成功"),
        "二次调用不应再抢到（幂等）"
    );
    // 不存在的会话同样返回 false（不报错）
    assert!(
        !close_if_active(&pool, Uuid::new_v4())
            .await
            .expect("条件关闭成功"),
        "不存在的会话不应抢到"
    );

    let fetched = get(&pool, session.id)
        .await
        .expect("查询成功")
        .expect("session 应存在");
    assert!(fetched.ended_at.is_some(), "关闭后 ended_at 应落库");
}

/// 通道概览：活跃会话数与最近活动时间按通道归组；关闭后活跃数下降。
#[tokio::test]
async fn channel_overview_counts_active_and_latest_activity() {
    let pool = init_test_pool().await.expect("测试库初始化成功");

    // 空通道：计数为 0、无活动时间（面板空状态）
    let empty = channel_overview(&pool, "mcp")
        .await
        .expect("统计空通道成功");
    assert_eq!(empty.active_sessions, 0);
    assert_eq!(empty.last_activity_ms, None);

    // mcp 通道两个会话 + local 通道一个会话（local 不应计入 mcp 统计）
    let s1 = create_in_channel(&pool, Some("rama-0001"), "mcp", Some("client-A"))
        .await
        .expect("创建 mcp 会话 1 成功");
    let s2 = create_in_channel(&pool, Some("rama-0001"), "mcp", Some("client-B"))
        .await
        .expect("创建 mcp 会话 2 成功");
    let local = create(&pool, None).await.expect("创建本地会话成功");

    for (session_id, ts) in [(s1.id, 1_000_i64), (s2.id, 2_000), (local.id, 9_000)] {
        let mut m = Message::new(
            session_id,
            MessageRole::User,
            "内容".to_string(),
            MessageSource::Local,
        );
        m.created_at = ts;
        crate::repo::messages::save_import(&pool, &m)
            .await
            .expect("插入消息成功");
    }

    let overview = channel_overview(&pool, "mcp")
        .await
        .expect("统计 mcp 通道成功");
    assert_eq!(overview.active_sessions, 2, "两个 mcp 会话均活跃");
    assert_eq!(
        overview.last_activity_ms,
        Some(2_000),
        "最近活动取 mcp 通道消息，local 会话（9000）不计入"
    );

    // 关闭 s1：活跃数下降；最近活动时间不受影响（消息仍在库中）
    assert!(
        close_if_active(&pool, s1.id).await.expect("条件关闭成功"),
        "首次关闭应抢到"
    );
    let after = channel_overview(&pool, "mcp")
        .await
        .expect("统计 mcp 通道成功");
    assert_eq!(after.active_sessions, 1, "关闭一个会话后活跃数应下降");
    assert_eq!(after.last_activity_ms, Some(2_000));
}

/// 人格最近对话时间：取该 persona 全部会话中的最大消息时间；无历史返回 None。
#[tokio::test]
async fn last_message_time_by_persona_tracks_latest_message() {
    let pool = init_test_pool().await.expect("测试库初始化成功");

    // 空库：无对话历史
    assert_eq!(
        last_message_time_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        None,
        "空库应返回 None"
    );

    // 目标 persona 两个有消息会话 + 一个空会话；他人会话时间更高（不应计入）
    let session_a = create(&pool, Some("char-0001"))
        .await
        .expect("创建会话 A 成功");
    let session_b = create(&pool, Some("char-0001"))
        .await
        .expect("创建会话 B 成功");
    let _empty = create(&pool, Some("char-0001"))
        .await
        .expect("创建空会话成功");
    let other = create(&pool, Some("char-0002"))
        .await
        .expect("创建他人会话成功");

    for (session_id, ts) in [
        (session_a.id, 1_000_i64),
        (session_a.id, 5_000),
        (session_b.id, 3_000),
        (other.id, 9_000),
    ] {
        let mut m = Message::new(
            session_id,
            MessageRole::User,
            "内容".to_string(),
            MessageSource::Local,
        );
        m.created_at = ts;
        crate::repo::messages::save_import(&pool, &m)
            .await
            .expect("插入消息成功");
    }

    assert_eq!(
        last_message_time_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        Some(5_000),
        "应取该 persona 全部会话中的最大消息时间（他人 9000 不计入）"
    );
    assert_eq!(
        last_message_time_by_persona(&pool, "char-none")
            .await
            .expect("查询应成功"),
        None,
        "无会话的 persona 应返回 None"
    );
}
