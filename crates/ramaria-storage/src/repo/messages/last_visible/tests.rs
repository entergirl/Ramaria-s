//! crates/ramaria-storage/src/repo/messages/last_visible/tests.rs - 会话最后可见消息查询单元测试
//!
//! 设计特点:
//! - 覆盖按会话取最新可见消息、空会话缺席与同刻消息的入库顺序决胜
//! - 覆盖撤回回退与 system / tool 角色跳过（可见口径）
//! - 使用真实 SQLite 临时库；不使用真实用户数据

use super::*;
use crate::database::init_test_pool;
use crate::repo::messages::save_import;
use ramaria_core::types::{MessageRole, MessageSource};

/// 插入一个空会话（无消息）并返回 ID。
async fn insert_session(pool: &SqlitePool) -> Uuid {
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .expect("插入 session fixture 应成功");
    session_id
}

/// 构造一条本地消息（未撤回；调用方按需改写字段）。
fn message(session_id: Uuid, role: MessageRole, content: &str, created_at: i64) -> Message {
    Message {
        id: Uuid::new_v4(),
        session_id,
        role,
        content: content.to_string(),
        created_at,
        source: MessageSource::Local,
        fingerprint: None,
        persona_uid: None,
        is_proactive: false,
        sender_ref: None,
        sender_name: None,
        platform_message_id: None,
        recalled_at: None,
    }
}

/// 每个会话取最新一条可见消息：多会话互不干扰，空会话不出现在映射中。
#[tokio::test]
async fn returns_latest_visible_message_per_session() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let s1 = insert_session(&pool).await;
    let s2 = insert_session(&pool).await;
    let empty = insert_session(&pool).await;

    save_import(&pool, &message(s1, MessageRole::User, "第一条", 1_000))
        .await
        .expect("写入消息应成功");
    save_import(
        &pool,
        &message(s1, MessageRole::Assistant, "最新一条", 1_100),
    )
    .await
    .expect("写入消息应成功");
    save_import(&pool, &message(s2, MessageRole::User, "同刻先写", 2_000))
        .await
        .expect("写入消息应成功");
    save_import(&pool, &message(s2, MessageRole::User, "同刻后写", 2_000))
        .await
        .expect("写入消息应成功");

    let last = list_last_visible_messages(&pool)
        .await
        .expect("查询最后可见消息应成功");
    assert_eq!(last.len(), 2, "空会话不应出现在映射中");
    assert_eq!(last.get(&s1).expect("s1 应有预览").content, "最新一条");
    assert_eq!(
        last.get(&s2).expect("s2 应有预览").content,
        "同刻后写",
        "同刻消息以入库顺序靠后者为准"
    );
    assert!(!last.contains_key(&empty), "空会话不产生预览");
}

/// 末条已撤回时回退到上一条可见消息；全部撤回的会话不产生预览。
#[tokio::test]
async fn recalled_messages_fall_back_and_all_recalled_is_absent() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let fallback = insert_session(&pool).await;
    let all_recalled = insert_session(&pool).await;

    save_import(
        &pool,
        &message(fallback, MessageRole::Assistant, "可回退消息", 1_000),
    )
    .await
    .expect("写入消息应成功");
    let mut recalled = message(fallback, MessageRole::Assistant, "已撤回消息", 2_000);
    recalled.recalled_at = Some(2_500);
    save_import(&pool, &recalled).await.expect("写入消息应成功");

    let mut only = message(all_recalled, MessageRole::User, "唯一一条已撤回", 3_000);
    only.recalled_at = Some(3_100);
    save_import(&pool, &only).await.expect("写入消息应成功");

    let last = list_last_visible_messages(&pool)
        .await
        .expect("查询最后可见消息应成功");
    assert_eq!(
        last.get(&fallback).expect("应有回退预览").content,
        "可回退消息",
        "撤回消息不作为预览"
    );
    assert!(
        !last.contains_key(&all_recalled),
        "全部消息已撤回的会话不产生预览"
    );
}

/// system / tool 消息不计入预览：末条为非可见角色时继续向前取。
#[tokio::test]
async fn system_and_tool_messages_are_skipped() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let mixed = insert_session(&pool).await;
    let tool_only = insert_session(&pool).await;

    save_import(&pool, &message(mixed, MessageRole::User, "用户发言", 1_000))
        .await
        .expect("写入消息应成功");
    save_import(
        &pool,
        &message(mixed, MessageRole::System, "系统提示", 2_000),
    )
    .await
    .expect("写入消息应成功");
    save_import(&pool, &message(mixed, MessageRole::Tool, "工具输出", 3_000))
        .await
        .expect("写入消息应成功");
    save_import(
        &pool,
        &message(tool_only, MessageRole::Tool, "只有工具消息", 4_000),
    )
    .await
    .expect("写入消息应成功");

    let last = list_last_visible_messages(&pool)
        .await
        .expect("查询最后可见消息应成功");
    assert_eq!(
        last.get(&mixed).expect("应跳过非可见角色").content,
        "用户发言"
    );
    assert!(!last.contains_key(&tool_only), "无可见消息的会话不产生预览");
}
