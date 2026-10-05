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
        is_proactive: false,
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

/// 主动消息标记：默认 false；真实写入 true 经各读取路径原样回读。
///
/// 说明:
/// - `save`（常规写入）与 `save_import` / `save_import_batch`（导入写入）
///   共用同一 INSERT，此处覆盖三条写入口的标记传递，防止只吃 DB 默认值的静默丢值；
/// - 读取侧覆盖 `list_by_session` / 分页 / 指纹 / 按 persona 四条投影。
#[tokio::test]
async fn is_proactive_roundtrip_and_default() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_fixture(&pool).await;

    // Message::new 默认 false：save 后回读仍为 false
    let plain = Message::new(
        session_id,
        MessageRole::Assistant,
        "常规回复".to_string(),
        MessageSource::Local,
    );
    assert!(!plain.is_proactive, "Message::new 默认应为非主动");
    save(&pool, &plain).await.expect("写入常规消息应成功");

    // with_proactive(true)：save 后回读为 true
    let proactive = Message::new(
        session_id,
        MessageRole::Assistant,
        "主动问候".to_string(),
        MessageSource::Online,
    )
    .with_proactive(true);
    save(&pool, &proactive).await.expect("写入主动消息应成功");

    let listed = list_by_session(&pool, session_id)
        .await
        .expect("读取应成功");
    let plain_read = listed
        .iter()
        .find(|m| m.id == plain.id)
        .expect("应包含常规消息");
    let proactive_read = listed
        .iter()
        .find(|m| m.id == proactive.id)
        .expect("应包含主动消息");
    assert!(!plain_read.is_proactive, "常规消息回读应为 false");
    assert!(proactive_read.is_proactive, "主动消息经 save 回读应为 true");

    // 分页读取同样解析标记（列投影一致性）
    let paged = list_by_session_paginated(&pool, session_id, 10, 0)
        .await
        .expect("分页读取应成功");
    let proactive_in_page = paged
        .iter()
        .find(|m| m.id == proactive.id)
        .expect("分页应包含主动消息");
    assert!(
        proactive_in_page.is_proactive,
        "分页读回应解析 is_proactive"
    );

    // 导入单条：标记必须真实写入（不是 DB 默认值兜底）
    let mut import_one = make_message(session_id, Some("fp-proactive-import"));
    import_one.is_proactive = true;
    save_import(&pool, &import_one)
        .await
        .expect("导入写入应成功");
    let hit = find_by_fingerprint(&pool, "fp-proactive-import")
        .await
        .expect("指纹查询应成功")
        .expect("应命中导入消息");
    assert!(hit.is_proactive, "save_import 应保留主动标记");

    // 导入批量：同一批次内 true / false 各自保持
    let mut import_a = make_message(session_id, Some("fp-proactive-batch-a"));
    import_a.is_proactive = true;
    let import_b = make_message(session_id, Some("fp-proactive-batch-b"));
    save_import_batch(&pool, &[import_a, import_b])
        .await
        .expect("批量导入写入应成功");
    let batch_a = find_by_fingerprint(&pool, "fp-proactive-batch-a")
        .await
        .expect("指纹查询应成功")
        .expect("应命中批量消息 a");
    let batch_b = find_by_fingerprint(&pool, "fp-proactive-batch-b")
        .await
        .expect("指纹查询应成功")
        .expect("应命中批量消息 b");
    assert!(batch_a.is_proactive, "批量导入应保留主动标记");
    assert!(!batch_b.is_proactive, "批量导入未标记的消息应保持 false");

    // 按 persona 读取同一投影：主动标记可见
    let by_persona = list_by_persona(&pool, "char-0001")
        .await
        .expect("按 persona 读取应成功");
    assert!(
        by_persona.iter().any(|m| m.is_proactive),
        "list_by_persona 应解析主动标记"
    );
}

// =========================================================
// persona 用户消息时间查询（主动调度退避 / 活跃时段统计）
// =========================================================

/// 插入 persona 与绑定该 persona 的 session fixture
/// （新查询按 `sessions.persona_uid` 归属，与消息自身 persona_uid 无关）。
async fn setup_persona_session(pool: &SqlitePool, persona_uid: &str) -> Uuid {
    sqlx::query(
        "INSERT INTO personas (uid, name, kind, seq, source, created_at, updated_at) \
         VALUES (?, '测试', 'char', 1, 'local', 0, 0) \
         ON CONFLICT(uid) DO NOTHING",
    )
    .bind(persona_uid)
    .execute(pool)
    .await
    .expect("插入 persona fixture 应成功");
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at, persona_uid) VALUES (?, 0, ?)")
        .bind(session_id.to_string())
        .bind(persona_uid)
        .execute(pool)
        .await
        .expect("插入 persona session fixture 应成功");
    session_id
}

/// 写入一条指定角色与时间戳的消息（指纹含时间戳，同 session 内保持唯一）。
async fn insert_role_message(
    pool: &SqlitePool,
    session_id: Uuid,
    role: MessageRole,
    created_at: i64,
) {
    let mut m = make_message(
        session_id,
        Some(&format!("fp-role-{session_id}-{created_at}")),
    );
    m.role = role;
    m.created_at = created_at;
    save_import(pool, &m).await.expect("写入角色消息成功");
}

/// last_user_message_time_by_persona 只计 user 消息，按 persona 会话归属隔离。
#[tokio::test]
async fn last_user_message_time_counts_user_role_only() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    // 无会话的 persona → None
    assert_eq!(
        last_user_message_time_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        None,
        "无会话应返回 None"
    );

    // 仅 assistant 消息：不视为用户回应
    let session_a = setup_persona_session(&pool, "char-0001").await;
    insert_role_message(&pool, session_a, MessageRole::Assistant, 5_000).await;
    assert_eq!(
        last_user_message_time_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        None,
        "仅 assistant 消息应返回 None"
    );

    // 写入 user 消息后取 user 的最大时间；更新的 assistant 消息不影响结果
    insert_role_message(&pool, session_a, MessageRole::User, 1_000).await;
    insert_role_message(&pool, session_a, MessageRole::User, 2_000).await;
    assert_eq!(
        last_user_message_time_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        Some(2_000),
        "应取 user 消息最大时间而非 assistant 时间"
    );

    // 另一 persona 的会话不串扰
    let session_b = setup_persona_session(&pool, "char-0002").await;
    insert_role_message(&pool, session_b, MessageRole::User, 9_000).await;
    assert_eq!(
        last_user_message_time_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        Some(2_000),
        "其他 persona 的会话不应串扰"
    );
    assert_eq!(
        last_user_message_time_by_persona(&pool, "char-0002")
            .await
            .expect("查询应成功"),
        Some(9_000),
        "目标 persona 应取自身 user 消息时间"
    );
}

/// list_user_message_times_since：窗口闭区间、升序、只计 user 消息、
/// 跨会话聚合、persona 隔离。
#[tokio::test]
async fn list_user_message_times_since_filters_window_and_role() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_a = setup_persona_session(&pool, "char-0001").await;
    insert_role_message(&pool, session_a, MessageRole::User, 1_000).await;
    insert_role_message(&pool, session_a, MessageRole::Assistant, 1_500).await;
    insert_role_message(&pool, session_a, MessageRole::User, 2_000).await;
    insert_role_message(&pool, session_a, MessageRole::User, 3_000).await;

    // 同 persona 的另一会话（模拟空闲封存后另起）：应跨会话聚合
    let session_b = setup_persona_session(&pool, "char-0001").await;
    insert_role_message(&pool, session_b, MessageRole::User, 2_500).await;

    // 另一 persona 的 user 消息：不应混入
    let session_c = setup_persona_session(&pool, "char-0002").await;
    insert_role_message(&pool, session_c, MessageRole::User, 2_100).await;

    // 窗口 [2000, ∞)：闭区间含边界 2000；更早的 1000 与 assistant 1500 排除；
    // 跨会话 2500 纳入；另一 persona 2100 排除；结果升序。
    let times = list_user_message_times_since(&pool, "char-0001", 2_000)
        .await
        .expect("查询应成功");
    assert_eq!(
        times,
        vec![2_000, 2_500, 3_000],
        "应升序返回窗口内 user 消息时间"
    );

    // 放大窗口 → 包含更早的 user 消息，assistant 仍排除
    let all = list_user_message_times_since(&pool, "char-0001", 0)
        .await
        .expect("查询应成功");
    assert_eq!(all, vec![1_000, 2_000, 2_500, 3_000]);

    // 窗口内无消息 → 空列表
    let none = list_user_message_times_since(&pool, "char-0001", 10_000)
        .await
        .expect("查询应成功");
    assert!(none.is_empty(), "窗口内无用户消息应返回空列表");
}

/// last_message_time_by_session：全部角色计入取最大时间、无消息 None、会话隔离。
#[tokio::test]
async fn last_message_time_by_session_counts_all_roles() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    // 无消息会话 → None
    let session_a = setup_persona_session(&pool, "char-0001").await;
    assert_eq!(
        last_message_time_by_session(&pool, session_a)
            .await
            .expect("查询应成功"),
        None,
        "无消息会话应返回 None"
    );

    // 仅 assistant 消息也计入（与 last_user_message_time_by_persona 的仅 user 口径不同）
    insert_role_message(&pool, session_a, MessageRole::Assistant, 5_000).await;
    assert_eq!(
        last_message_time_by_session(&pool, session_a)
            .await
            .expect("查询应成功"),
        Some(5_000),
        "assistant 消息应计入会话最后消息时间"
    );

    // user 与 assistant 混合：取全部角色中的最大时间
    insert_role_message(&pool, session_a, MessageRole::User, 1_000).await;
    insert_role_message(&pool, session_a, MessageRole::Assistant, 6_000).await;
    assert_eq!(
        last_message_time_by_session(&pool, session_a)
            .await
            .expect("查询应成功"),
        Some(6_000),
        "应取全部角色中的最大时间"
    );

    // 另一会话独立计算（会话隔离）
    let session_b = setup_persona_session(&pool, "char-0002").await;
    insert_role_message(&pool, session_b, MessageRole::User, 9_000).await;
    assert_eq!(
        last_message_time_by_session(&pool, session_a)
            .await
            .expect("查询应成功"),
        Some(6_000),
        "其他会话的消息不应串扰"
    );
    assert_eq!(
        last_message_time_by_session(&pool, session_b)
            .await
            .expect("查询应成功"),
        Some(9_000)
    );

    // 不存在的会话 → None
    assert_eq!(
        last_message_time_by_session(&pool, Uuid::new_v4())
            .await
            .expect("查询应成功"),
        None,
        "不存在的会话应返回 None"
    );
}

// =========================================================
// persona 本地用户消息存在性查询（人格解锁判定）
// =========================================================

/// 写入一条本地用户消息（无导入指纹，走常规保存路径）。
async fn insert_local_user_message(pool: &SqlitePool, session_id: Uuid, created_at: i64) {
    let mut m = make_message(session_id, None);
    m.created_at = created_at;
    save(pool, &m).await.expect("写入本地用户消息成功");
}

/// 本地 user 消息（无导入指纹）命中；无消息时返回 false。
#[tokio::test]
async fn has_local_user_message_true_for_local_user() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session = setup_persona_session(&pool, "char-0001").await;

    assert!(
        !has_local_user_message_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        "无消息应为 false"
    );

    insert_local_user_message(&pool, session, 1_000).await;
    assert!(
        has_local_user_message_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        "本地 user 消息应命中"
    );
}

/// 仅 assistant 角色消息（如主动消息）不计入。
#[tokio::test]
async fn has_local_user_message_ignores_assistant_role() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session = setup_persona_session(&pool, "char-0001").await;

    let mut m = make_message(session, None);
    m.role = MessageRole::Assistant;
    m.created_at = 1_000;
    save(&pool, &m).await.expect("写入 assistant 消息成功");

    assert!(
        !has_local_user_message_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        "assistant 角色不应命中"
    );
}

/// 带导入指纹的 user 消息（导入历史）不计入。
#[tokio::test]
async fn has_local_user_message_ignores_imported() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session = setup_persona_session(&pool, "char-0001").await;

    // 该辅助写入的消息指纹非空（导入消息）
    insert_role_message(&pool, session, MessageRole::User, 1_000).await;

    assert!(
        !has_local_user_message_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        "导入消息不应命中"
    );
}

/// 归属以会话 `persona_uid` 为准：他人格会话不串扰，消息自身 persona_uid 不参与归属。
#[tokio::test]
async fn has_local_user_message_scopes_by_session_persona() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_a = setup_persona_session(&pool, "char-0001").await;
    let session_b = setup_persona_session(&pool, "char-0002").await;

    // 仅 char-0002 的会话有本地用户消息
    insert_local_user_message(&pool, session_b, 1_000).await;
    assert!(
        !has_local_user_message_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        "他人格会话不应串扰"
    );
    assert!(
        has_local_user_message_by_persona(&pool, "char-0002")
            .await
            .expect("查询应成功"),
        "目标人格自身的本地消息应命中"
    );

    // char-0001 的会话内写入 persona_uid 标记为 char-0002 的本地消息：
    // 归属仍取会话归属 → char-0001 命中
    let mut m = make_message(session_a, None);
    m.persona_uid = Some("char-0002".to_string());
    m.created_at = 2_000;
    save(&pool, &m).await.expect("写入消息成功");
    assert!(
        has_local_user_message_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        "归属应取 sessions.persona_uid 而非消息自身 persona_uid"
    );
}

/// 未绑定 persona 的会话（`persona_uid IS NULL`）消息不计入。
#[tokio::test]
async fn has_local_user_message_ignores_unbound_session() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session = setup_fixture(&pool).await;

    insert_local_user_message(&pool, session, 1_000).await;
    assert!(
        !has_local_user_message_by_persona(&pool, "char-0001")
            .await
            .expect("查询应成功"),
        "未绑定 persona 的会话不应命中"
    );
}
