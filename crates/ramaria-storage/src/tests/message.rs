//! crates/ramaria-storage/src/tests/message.rs - 原始消息存储测试
//!
//! 设计特点:
//! - 覆盖消息写入、persona_uid 绑定与按会话 / 按 persona 的读取
//! - 覆盖会话消息计数聚合的归组语义
//! - 覆盖按 persona 全量读取（不截断）与分页读取的边界

use super::*;

#[tokio::test]
async fn message_crud() {
    let storage = setup().await;
    let session = storage.create_session(None).await.unwrap();
    let msg = Message::new(
        session.id,
        MessageRole::User,
        "测试消息".into(),
        MessageSource::Local,
    );
    storage.save_message(&msg).await.unwrap();

    let msgs = storage.list_messages(session.id).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "测试消息");
}

#[tokio::test]
async fn message_with_persona_uid() {
    let storage = setup().await;
    // 先创建 persona，否则 FK 约束会失败
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();

    let session = storage.create_session(None).await.unwrap();
    let mut msg = Message::new(
        session.id,
        MessageRole::User,
        "你好".into(),
        MessageSource::Local,
    );
    msg.persona_uid = Some("user-0001".into());
    storage.save_message(&msg).await.unwrap();

    let msgs = storage.list_messages(session.id).await.unwrap();
    assert_eq!(msgs[0].persona_uid.as_deref(), Some("user-0001"));
}

/// 会话消息计数聚合：多会话按会话归组，无消息会话不出现在映射中。
#[tokio::test]
async fn count_messages_by_session_aggregates_per_session() {
    let storage = setup().await;
    let s1 = storage.create_session(None).await.unwrap();
    let s2 = storage.create_session(None).await.unwrap();
    let empty = storage.create_session(None).await.unwrap();

    for (session_id, count) in [(&s1.id, 2_i64), (&s2.id, 3_i64)] {
        for i in 0..count {
            let mut m = Message::new(
                *session_id,
                MessageRole::User,
                format!("m{i}"),
                MessageSource::Local,
            );
            m.created_at = 1_000 + i;
            storage.save_message(&m).await.unwrap();
        }
    }

    let counts = storage.count_messages_by_session().await.unwrap();
    assert_eq!(counts.get(&s1.id).copied(), Some(2));
    assert_eq!(counts.get(&s2.id).copied(), Some(3));
    assert_eq!(counts.len(), 2, "聚合只应包含有消息的会话");
    assert!(
        !counts.contains_key(&empty.id),
        "无消息会话不应出现在聚合映射中（调用方按 0 处理）"
    );
}

// list_messages_by_persona 不再截断（原 LIMIT 200），
// 导入管线重建能枚举该 persona 的全部消息与 session
#[tokio::test]
async fn message_list_by_persona_returns_all_over_200() {
    let storage = setup().await;
    let p = Persona::new(
        "char-p22".into(),
        "P2-2 角色".into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let session = storage.create_session(Some("char-p22")).await.unwrap();

    // 写入 250 条消息（> 原 LIMIT 200），横跨 3 个 session 更贴近导入场景
    let total = 250usize;
    for i in 0..total {
        let msg = Message::new(
            session.id,
            MessageRole::User,
            format!("消息{i}"),
            MessageSource::Local,
        )
        .with_persona_uid(Some("char-p22".to_string()));
        let mut m = msg;
        m.created_at = 1_700_000_000_000 + i as i64 * 1000;
        storage.save_message(&m).await.unwrap();
    }

    let all = storage.list_messages_by_persona("char-p22").await.unwrap();
    assert_eq!(all.len(), total, "应返回全部 {} 条消息而非截断", total);

    // 枚举出的 session 集合覆盖该 persona 全部会话
    let sessions: std::collections::HashSet<_> = all.iter().map(|m| m.session_id).collect();
    assert!(sessions.contains(&session.id));
}

// trait 层 list_messages_by_persona_paginated 分页正确性（storage 覆写为高效 SQL）。
#[tokio::test]
async fn message_list_by_persona_paginated_works() {
    let storage = setup().await;
    let p = Persona::new(
        "char-pg".into(),
        "分页角色".into(),
        PersonaKind::Char,
        1,
        "local".into(),
    );
    storage.create_persona(&p).await.unwrap();
    let session = storage.create_session(Some("char-pg")).await.unwrap();

    // 写入 7 条，created_at 从 base 递增，超过单页大小(3)。
    let total = 7usize;
    let base = 1_700_000_000_000i64;
    for i in 0..total {
        let msg = Message::new(
            session.id,
            MessageRole::User,
            format!("分页消息{i}"),
            MessageSource::Local,
        )
        .with_persona_uid(Some("char-pg".to_string()));
        let mut m = msg;
        m.created_at = base + i as i64;
        storage.save_message(&m).await.unwrap();
    }

    // 第 1 页（最新 3 条，created_at DESC）。
    let page1 = storage
        .list_messages_by_persona_paginated("char-pg", 3, 0)
        .await
        .unwrap();
    let p1_ts: Vec<i64> = page1.iter().map(|m| m.created_at).collect();
    assert_eq!(p1_ts, (base + 4..=base + 6).rev().collect::<Vec<_>>());

    // 末页（offset 6 → 余 1 条，页不满）。
    let page3 = storage
        .list_messages_by_persona_paginated("char-pg", 3, 6)
        .await
        .unwrap();
    let p3_ts: Vec<i64> = page3.iter().map(|m| m.created_at).collect();
    assert_eq!(p3_ts, vec![base]);

    // offset 越界 → 空。
    let beyond = storage
        .list_messages_by_persona_paginated("char-pg", 3, 20)
        .await
        .unwrap();
    assert!(beyond.is_empty(), "offset 越界应返回空页");
}
