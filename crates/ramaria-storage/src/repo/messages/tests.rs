//! crates/ramaria-storage/src/repo/messages/tests.rs - 原始消息存取单元测试
//!
//! 设计特点:
//! - 覆盖指纹去重的 UNIQUE 约束与命中 / 未命中
//! - 覆盖按 persona 分页的正确性与隔离
//! - 覆盖按通道 / 外部标识的去重键查询与正文 TRIM

use super::*;
use crate::database::init_test_pool;
use ramaria_core::types::{Message, MessageRole, MessageSource};
use uuid::Uuid;

/// 插入 persona 与 session fixture，满足 messages 的外键约束。
async fn setup_fixture(pool: &SqlitePool) -> Uuid {
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
         VALUES ('char-0001', '测试', 'char', 1, 'local', 0, 0)",
    )
    .execute(pool)
    .await
    .expect("插入 persona fixture 应成功");
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .expect("插入 session fixture 应成功");
    session_id
}

fn make_message(session_id: Uuid, fingerprint: Option<&str>) -> Message {
    Message {
        id: uuid::Uuid::new_v4(),
        session_id,
        role: MessageRole::User,
        content: "内容".to_string(),
        created_at: 1000,
        source: MessageSource::Local,
        fingerprint: fingerprint.map(|s| s.to_string()),
        persona_uid: Some("char-0001".to_string()),
    }
}

/// 同 fingerprint 二次写入被 UNIQUE 约束拒绝；不同 fingerprint 正常写入。
#[tokio::test]
async fn fingerprint_unique_rejects_duplicate() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_fixture(&pool).await;

    // 首次写入成功
    save_import(&pool, &make_message(session_id, Some("fp-same")))
        .await
        .expect("首次写入成功");

    // 同 fingerprint 再次写入 → UNIQUE 冲突
    let dup = make_message(session_id, Some("fp-same"));
    let err = save_import(&pool, &dup)
        .await
        .expect_err("同指纹应被 UNIQUE 拒绝");
    let unique_in_chain = std::iter::successors(std::error::Error::source(&err), |e| e.source())
        .map(|e| e.to_string())
        .chain([err.to_string()])
        .any(|msg| msg.contains("UNIQUE"));
    assert!(
        unique_in_chain,
        "底层错误链应含 UNIQUE 约束冲突，实际: {err}"
    );

    // 不同 fingerprint 正常写入
    save_import(&pool, &make_message(session_id, Some("fp-other")))
        .await
        .expect("不同指纹写入成功");
    assert_eq!(count_by_session(&pool, session_id).await.unwrap(), 2);
}

/// find_by_fingerprint 能命中已入库指纹，未入库则返回 None。
#[tokio::test]
async fn find_by_fingerprint_hits_and_misses() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_fixture(&pool).await;

    assert!(
        find_by_fingerprint(&pool, "fp-present")
            .await
            .unwrap()
            .is_none()
    );
    save_import(&pool, &make_message(session_id, Some("fp-present")))
        .await
        .expect("写入成功");
    let hit = find_by_fingerprint(&pool, "fp-present")
        .await
        .unwrap()
        .expect("应命中");
    assert_eq!(hit.fingerprint.as_deref(), Some("fp-present"));
}

/// 构造某 persona 的 N 条消息，created_at 从 base 起逐条 +1。
async fn insert_persona_messages(
    pool: &SqlitePool,
    session_id: Uuid,
    persona_uid: &str,
    count: usize,
    base_ts: i64,
) {
    for i in 0..count {
        let mut m = make_message(session_id, Some(&format!("fp-pg-{persona_uid}-{i}")));
        m.persona_uid = Some(persona_uid.to_string());
        m.created_at = base_ts + i as i64;
        save_import(pool, &m).await.expect("写入 persona 消息成功");
    }
}

/// list_by_persona_paginated 分页正确性：页大小、页间排序、末页不满、offset 越界为空。
#[tokio::test]
async fn list_by_persona_paginated_pages_correctly() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_fixture(&pool).await;
    // 插入超过页大小的消息（页大小 3，共 7 条），created_at 递增。
    insert_persona_messages(&pool, session_id, "char-0001", 7, 1_000).await;

    // 全量参照：created_at DESC（最新在前）。
    let all = list_by_persona(&pool, "char-0001").await.unwrap();
    assert_eq!(all.len(), 7);
    // created_at 应为 1000..=1006，全量返回后整体降序。
    let all_ts: Vec<i64> = all.iter().map(|m| m.created_at).collect();
    assert_eq!(all_ts, (1000..=1006).rev().collect::<Vec<_>>());

    // 第 1 页：limit 3 offset 0 → 最新 3 条 (1006,1005,1004)。
    let page1 = list_by_persona_paginated(&pool, "char-0001", 3, 0)
        .await
        .unwrap();
    let p1_ts: Vec<i64> = page1.iter().map(|m| m.created_at).collect();
    assert_eq!(p1_ts, vec![1006, 1005, 1004]);

    // 第 2 页：offset 3 → 中间 3 条 (1003,1002,1001)。
    let page2 = list_by_persona_paginated(&pool, "char-0001", 3, 3)
        .await
        .unwrap();
    let p2_ts: Vec<i64> = page2.iter().map(|m| m.created_at).collect();
    assert_eq!(p2_ts, vec![1003, 1002, 1001]);

    // 末页：offset 6 → 余下 1 条 (1000)，页不满。
    let page3 = list_by_persona_paginated(&pool, "char-0001", 3, 6)
        .await
        .unwrap();
    let p3_ts: Vec<i64> = page3.iter().map(|m| m.created_at).collect();
    assert_eq!(p3_ts, vec![1000]);

    // offset 越界 → 空。
    let beyond = list_by_persona_paginated(&pool, "char-0001", 3, 9)
        .await
        .unwrap();
    assert!(beyond.is_empty(), "offset 越界应返回空页");
}

/// persona 分页不跨 persona：只返回目标 persona 的消息。
#[tokio::test]
async fn list_by_persona_paginated_is_isolated_by_persona() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_fixture(&pool).await;
    insert_persona_messages(&pool, session_id, "char-0001", 5, 2_000).await;

    // 先建立 char-0002 persona 行（messages.persona_uid 有外键约束），
    // 再写入其消息，验证不影响 char-0001 的分页结果。
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
         VALUES ('char-0002', '另一角色', 'char', 2, 'local', 0, 0)",
    )
    .execute(&pool)
    .await
    .expect("插入 char-0002 persona fixture 应成功");
    insert_persona_messages(&pool, session_id, "char-0002", 3, 3_000).await;

    let all = list_by_persona_paginated(&pool, "char-0001", 100, 0)
        .await
        .unwrap();
    assert_eq!(all.len(), 5);
    assert!(
        all.iter()
            .all(|m| m.persona_uid.as_deref() == Some("char-0001")),
        "分页结果应仅含目标 persona 消息"
    );
}

// =========================================================
// 通道 + 外部对话标识去重键查询（外部入口重复提交去重）
// =========================================================

/// 在指定通道/外部标识下建会话并写入 N 条消息
/// （created_at 自 base 递增；时间戳编号编入正文便于断言顺序）。
async fn insert_channel_messages(
    pool: &SqlitePool,
    channel: &str,
    external_ref: Option<&str>,
    count: usize,
    base_ts: i64,
) -> Uuid {
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
                 VALUES ('char-0001', '测试', 'char', 1, 'local', 0, 0) \
                 ON CONFLICT(uid) DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("插入 persona fixture 应成功");
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at, channel, external_ref) VALUES (?, 0, ?, ?)")
        .bind(session_id.to_string())
        .bind(channel)
        .bind(external_ref)
        .execute(pool)
        .await
        .expect("插入通道 session fixture 应成功");
    for i in 0..count {
        let mut m = make_message(session_id, Some(&format!("fp-ch-{session_id}-{i}")));
        m.content = format!("内容 {}", base_ts + i as i64);
        m.created_at = base_ts + i as i64;
        save_import(pool, &m).await.expect("写入通道消息成功");
    }
    session_id
}

/// 按 (channel, external_ref) 取去重键：跨会话、升序、通道/标识严格隔离、
/// NULL 安全、正文 SQL 侧 TRIM。
#[tokio::test]
async fn list_keys_by_channel_ref_scopes_and_orders() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    // 同一外部对话两个会话（模拟空闲封存后另起）
    insert_channel_messages(&pool, "mcp", Some("client-A"), 3, 1_000).await;
    insert_channel_messages(&pool, "mcp", Some("client-A"), 2, 2_000).await;
    // 其它通道 / 其它标识 / 无标识会话：不应混入
    insert_channel_messages(&pool, "mcp", Some("client-B"), 2, 3_000).await;
    insert_channel_messages(&pool, "local", Some("client-A"), 2, 4_000).await;
    insert_channel_messages(&pool, "mcp", None, 2, 5_000).await;

    // 全量：跨会话 5 条，按 created_at ASC
    let all = list_keys_by_channel_ref(&pool, "mcp", Some("client-A"))
        .await
        .expect("查询成功");
    let contents: Vec<&str> = all.iter().map(|k| k.content.as_str()).collect();
    assert_eq!(
        contents,
        vec![
            "内容 1000",
            "内容 1001",
            "内容 1002",
            "内容 2000",
            "内容 2001"
        ],
        "应跨会话按时间升序返回去重键"
    );
    assert!(
        all.iter().all(|k| k.role == MessageRole::User),
        "角色应正确解析"
    );

    // 无标识会话：绑定 None 只命中 NULL 会话
    let null_ref = list_keys_by_channel_ref(&pool, "mcp", None)
        .await
        .expect("查询成功");
    assert_eq!(null_ref.len(), 2, "None 应命中无标识会话");
    assert!(
        null_ref.iter().all(|k| k.content.starts_with("内容 5")),
        "无标识查询不应混入带标识会话的消息: {null_ref:?}"
    );

    // 不存在的通道 → 空
    assert!(
        list_keys_by_channel_ref(&pool, "telegram", Some("client-A"))
            .await
            .expect("查询成功")
            .is_empty()
    );

    // 正文 TRIM：写入口径保留空白时，键按 trim 后返回
    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO sessions (id, started_at, channel, external_ref) \
         VALUES (?, 0, 'mcp', 'client-C')",
    )
    .bind(session_id.to_string())
    .execute(&pool)
    .await
    .expect("插入 session fixture 应成功");
    let mut padded = make_message(session_id, Some("fp-padded"));
    padded.content = "  前后有空白  ".to_string();
    save_import(&pool, &padded).await.expect("写入消息成功");
    let keys = list_keys_by_channel_ref(&pool, "mcp", Some("client-C"))
        .await
        .expect("查询成功");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].content, "前后有空白", "正文应在 SQL 侧 TRIM");
}
