//! crates/ramaria-storage/src/repo/attachments/tests.rs - 消息附件行存取单元测试
//!
//! 设计特点:
//! - 覆盖写入 / 按会话扫描 pending（排序与上限分支）/ 跨会话隔离
//! - 覆盖描述回填（md5 批量与单行兜底）与状态迁移（description 不动）
//! - 覆盖按消息分片查询、脏枚举值读取降级与消息 / 会话删除级联

use super::*;
use crate::database::init_test_pool;

/// 插入 session fixture，满足 messages / message_attachments 的外键链。
async fn setup_session(pool: &SqlitePool) -> Uuid {
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO sessions (id, started_at) VALUES (?, 0)")
        .bind(session_id.to_string())
        .execute(pool)
        .await
        .expect("插入 session fixture 应成功");
    session_id
}

/// 插入 message fixture（归属指定会话）。
async fn setup_message(pool: &SqlitePool, session_id: Uuid, content: &str) -> Uuid {
    let message_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source) \
         VALUES (?, ?, 'user', ?, 0, 'local')",
    )
    .bind(message_id.to_string())
    .bind(session_id.to_string())
    .bind(content)
    .execute(pool)
    .await
    .expect("插入 message fixture 应成功");
    message_id
}

/// 构造 pending 图片附件行（描述与模型默认 None）。
fn attachment(message_id: Uuid, md5: Option<&str>) -> MessageAttachment {
    MessageAttachment {
        id: 0,
        message_id,
        kind: InboundAttachmentKind::Image,
        source_ref: "images/a.png".to_string(),
        md5: md5.map(|s| s.to_string()),
        size: Some(1024),
        width: Some(640),
        height: Some(480),
        sub_type: Some("photo".to_string()),
        status: AttachmentStatus::Pending,
        description: None,
        description_model: None,
        created_at: 100,
        updated_at: 100,
    }
}

/// 读取指定消息的全部附件（供断言复用）。
async fn attachments_of(pool: &SqlitePool, message_id: Uuid) -> Vec<MessageAttachment> {
    list_by_messages(pool, &[message_id])
        .await
        .expect("按消息查询附件应成功")
}

/// 空批次直接成功（不开启事务、不访问数据库）。
#[tokio::test]
async fn insert_empty_batch_is_ok() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    insert_batch(&pool, &[]).await.expect("空批次应直接成功");
}

/// 写入后按会话扫 pending：id 升序、只含 pending、limit=0 全量与 limit=N 截断。
#[tokio::test]
async fn insert_then_scan_pending_with_sort_and_limit() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "含图消息").await;

    // 两条 pending + 一条 done（done 不参与扫描）
    let mut done = attachment(message_id, Some("bbbbbbbb000000000000000000000000"));
    done.status = AttachmentStatus::Done;
    done.description = Some("已完成".to_string());
    insert_batch(
        &pool,
        &[
            attachment(message_id, Some("aaaaaaaa000000000000000000000000")),
            done,
            attachment(message_id, Some("cccccccc000000000000000000000000")),
        ],
    )
    .await
    .expect("批量写入应成功");

    let all = list_pending_by_session(&pool, session_id, 0)
        .await
        .expect("扫描应成功");
    let ids: Vec<i64> = all.iter().map(|a| a.id).collect();
    assert_eq!(all.len(), 2, "只应扫出 pending 行");
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "应按附件 id 升序返回");
    assert_eq!(
        all[0].md5.as_deref(),
        Some("aaaaaaaa000000000000000000000000")
    );
    assert_eq!(all[0].kind, InboundAttachmentKind::Image);

    let limited = list_pending_by_session(&pool, session_id, 1)
        .await
        .expect("截断扫描应成功");
    assert_eq!(limited.len(), 1, "limit=1 应只返回首条");
    assert_eq!(limited[0].id, ids[0]);
}

/// 跨会话隔离：其他会话的 pending 不被扫出。
#[tokio::test]
async fn scan_isolates_sessions() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_a = setup_session(&pool).await;
    let session_b = setup_session(&pool).await;
    let message_a = setup_message(&pool, session_a, "会话 a").await;
    let message_b = setup_message(&pool, session_b, "会话 b").await;

    insert_batch(
        &pool,
        &[
            attachment(message_a, Some("aaaa0000000000000000000000000000")),
            attachment(message_b, Some("bbbb0000000000000000000000000000")),
        ],
    )
    .await
    .expect("写入应成功");

    let rows = list_pending_by_session(&pool, session_a, 0)
        .await
        .expect("扫描应成功");
    assert_eq!(rows.len(), 1, "跨会话附件不应混入");
    assert_eq!(rows[0].message_id, message_a);
}

/// fill_done_by_md5：同 md5 的全部 pending 置 done（含描述与模型）；已 done 行不动。
#[tokio::test]
async fn fill_done_by_md5_updates_all_pending_only() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "两处同图").await;

    let md5 = "aaaaaaaabbbbbbbbccccccccdddddddd";
    let mut done = attachment(message_id, Some(md5));
    done.status = AttachmentStatus::Done;
    done.description = Some("旧描述".to_string());
    done.description_model = Some("old-model".to_string());
    insert_batch(
        &pool,
        &[
            attachment(message_id, Some(md5)),
            attachment(message_id, Some(md5)),
            done,
            attachment(message_id, Some("eeeeeeeeffffffff0000000000000000")),
        ],
    )
    .await
    .expect("写入应成功");

    let affected = fill_done_by_md5(&pool, md5, "一只橘猫", "vision-mock")
        .await
        .expect("回填应成功");
    assert_eq!(affected, 2, "应命中两条 pending 行");

    let rows = attachments_of(&pool, message_id).await;
    let filled: Vec<&MessageAttachment> = rows
        .iter()
        .filter(|a| a.md5.as_deref() == Some(md5))
        .collect();
    assert_eq!(filled.len(), 3);
    for row in &filled {
        assert_eq!(row.status, AttachmentStatus::Done);
    }
    let refreshed: Vec<&&MessageAttachment> = filled
        .iter()
        .filter(|a| a.description.as_deref() == Some("一只橘猫"))
        .collect();
    assert_eq!(refreshed.len(), 2, "两条 pending 应被回填新描述");
    for row in &refreshed {
        assert_eq!(row.description_model.as_deref(), Some("vision-mock"));
    }
    assert_eq!(
        filled
            .iter()
            .filter(|a| a.description.as_deref() == Some("旧描述"))
            .count(),
        1,
        "重复回填不应改写已 done 行的描述"
    );
    let untouched = rows
        .iter()
        .find(|a| a.md5.as_deref() == Some("eeeeeeeeffffffff0000000000000000"))
        .expect("其他 md5 行应存在");
    assert_eq!(
        untouched.status,
        AttachmentStatus::Pending,
        "其他 md5 行不被动"
    );
}

/// mark_done：单行置 done 并写入描述与模型（md5 缺失兜底路径）。
#[tokio::test]
async fn mark_done_single_row() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "无指纹图").await;
    insert_batch(&pool, &[attachment(message_id, None)])
        .await
        .expect("写入应成功");
    let id = attachments_of(&pool, message_id).await[0].id;

    mark_done(&pool, id, "一张风景照", "vision-mock")
        .await
        .expect("单行置 done 应成功");

    let row = &attachments_of(&pool, message_id).await[0];
    assert_eq!(row.status, AttachmentStatus::Done);
    assert_eq!(row.description.as_deref(), Some("一张风景照"));
    assert_eq!(row.description_model.as_deref(), Some("vision-mock"));
}

/// mark_status：failed / skipped 迁移不影响既有 description。
#[tokio::test]
async fn mark_status_keeps_description() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "失败图").await;
    let md5 = "aaaaaaaabbbbbbbbccccccccdddddddd";
    insert_batch(&pool, &[attachment(message_id, Some(md5))])
        .await
        .expect("写入应成功");
    let id = attachments_of(&pool, message_id).await[0].id;
    fill_done_by_md5(&pool, md5, "旧描述", "vision-mock")
        .await
        .expect("先置 done 应成功");

    mark_status(&pool, id, AttachmentStatus::Failed)
        .await
        .expect("置 failed 应成功");
    let row = &attachments_of(&pool, message_id).await[0];
    assert_eq!(row.status, AttachmentStatus::Failed);
    assert_eq!(
        row.description.as_deref(),
        Some("旧描述"),
        "description 不应被清空"
    );

    mark_status(&pool, id, AttachmentStatus::Skipped)
        .await
        .expect("置 skipped 应成功");
    let row = &attachments_of(&pool, message_id).await[0];
    assert_eq!(row.status, AttachmentStatus::Skipped);
    assert_eq!(row.description.as_deref(), Some("旧描述"));
}

/// find_done_description_by_md5：取 updated_at 最新；未命中 / 空描述不算。
#[tokio::test]
async fn find_done_description_picks_latest_and_skips_empty() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "重复图").await;
    let md5 = "aaaaaaaabbbbbbbbccccccccdddddddd";

    sqlx::query(
        "INSERT INTO message_attachments \
             (message_id, kind, source_ref, md5, status, description, description_model, \
              created_at, updated_at) \
         VALUES (?, 'image', 'images/a.png', ?, 'done', '旧描述', 'model-a', 1, 100)",
    )
    .bind(message_id.to_string())
    .bind(md5)
    .execute(&pool)
    .await
    .expect("插入旧描述行应成功");
    sqlx::query(
        "INSERT INTO message_attachments \
             (message_id, kind, source_ref, md5, status, description, description_model, \
              created_at, updated_at) \
         VALUES (?, 'image', 'images/b.png', ?, 'done', '新描述', NULL, 2, 200)",
    )
    .bind(message_id.to_string())
    .bind(md5)
    .execute(&pool)
    .await
    .expect("插入新描述行应成功");

    let found = find_done_description_by_md5(&pool, md5)
        .await
        .expect("查询应成功");
    assert_eq!(
        found,
        Some(("新描述".to_string(), String::new())),
        "应取 updated_at 最新的描述；模型缺失回退空串"
    );

    assert_eq!(
        find_done_description_by_md5(&pool, "00000000000000000000000000000000")
            .await
            .expect("未知 md5 查询应成功"),
        None,
        "未知 md5 应返回 None"
    );

    // done 但描述为空（NULL）的行不算
    sqlx::query(
        "INSERT INTO message_attachments \
             (message_id, kind, source_ref, md5, status, created_at, updated_at) \
         VALUES (?, 'image', 'images/c.png', ?, 'done', 3, 300)",
    )
    .bind(message_id.to_string())
    .bind("bbbbbbbbccccccccdddddddd00000000")
    .execute(&pool)
    .await
    .expect("插入空描述行应成功");
    assert_eq!(
        find_done_description_by_md5(&pool, "bbbbbbbbccccccccdddddddd00000000")
            .await
            .expect("空描述查询应成功"),
        None,
        "description 为空不应命中"
    );
}

/// list_by_messages：空输入零访问；命中按消息聚合；未知 id 忽略。
#[tokio::test]
async fn list_by_messages_hits_and_empty_input() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_a = setup_message(&pool, session_id, "消息 a").await;
    let message_b = setup_message(&pool, session_id, "消息 b").await;

    assert!(
        list_by_messages(&pool, &[])
            .await
            .expect("空输入应成功")
            .is_empty(),
        "空输入应返回空列表"
    );

    insert_batch(
        &pool,
        &[
            attachment(message_a, Some("aaaa0000000000000000000000000000")),
            attachment(message_b, Some("bbbb0000000000000000000000000000")),
        ],
    )
    .await
    .expect("写入应成功");

    let rows = list_by_messages(&pool, &[message_a, Uuid::new_v4(), message_b])
        .await
        .expect("批量查询应成功");
    assert_eq!(rows.len(), 2, "未知消息 id 应被忽略");
    let message_ids: Vec<Uuid> = rows.iter().map(|a| a.message_id).collect();
    assert!(message_ids.contains(&message_a) && message_ids.contains(&message_b));
}

/// 分片路径：消息 id 超单片上限（500）时分多片执行，第二片仍能命中。
#[tokio::test]
async fn list_by_messages_chunks_over_limit() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "跨片消息").await;
    insert_batch(
        &pool,
        &[attachment(
            message_id,
            Some("aaaa0000000000000000000000000000"),
        )],
    )
    .await
    .expect("写入应成功");

    // 目标消息放在第 501 个位置（落入第二片）；其余为不存在的 id
    let mut ids: Vec<Uuid> = (0..MESSAGE_ID_CHUNK).map(|_| Uuid::new_v4()).collect();
    ids.push(message_id);
    let rows = list_by_messages(&pool, &ids).await.expect("分片查询应成功");
    assert_eq!(rows.len(), 1, "跨片命中应返回附件");
    assert_eq!(rows[0].message_id, message_id);
}

/// 脏枚举值读取降级：非法 kind / status 不报错，分别回退 File / Pending。
#[tokio::test]
async fn dirty_enum_values_degrade_on_read() {
    let pool = init_test_pool().await.expect("测试库初始化失败");
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "脏数据消息").await;

    sqlx::query(
        "INSERT INTO message_attachments \
             (message_id, kind, source_ref, status, created_at, updated_at) \
         VALUES (?, 'sticker_v2', 'images/a.webp', 'unknown_state', 1, 1)",
    )
    .bind(message_id.to_string())
    .execute(&pool)
    .await
    .expect("直写脏数据应成功");

    let rows = attachments_of(&pool, message_id).await;
    assert_eq!(rows.len(), 1, "脏数据行应可读出");
    assert_eq!(
        rows[0].kind,
        InboundAttachmentKind::File,
        "非法 kind 应回退 File"
    );
    assert_eq!(
        rows[0].status,
        AttachmentStatus::Pending,
        "非法 status 应回退 Pending"
    );
}

/// 删除消息或会话后附件行级联消失（ON DELETE CASCADE 链路）。
#[tokio::test]
async fn attachments_cascade_with_message_and_session() {
    let pool = init_test_pool().await.expect("测试库初始化失败");

    // 删除消息 → 附件级联
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "将被删除").await;
    insert_batch(
        &pool,
        &[attachment(
            message_id,
            Some("aaaa0000000000000000000000000000"),
        )],
    )
    .await
    .expect("写入应成功");
    sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(message_id.to_string())
        .execute(&pool)
        .await
        .expect("删除消息应成功");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM message_attachments WHERE message_id = ?")
            .bind(message_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("计数应成功");
    assert_eq!(count, 0, "删除消息后附件应级联删除");

    // 删除会话 → 消息 → 附件级联
    let session_id = setup_session(&pool).await;
    let message_id = setup_message(&pool, session_id, "随会话删除").await;
    insert_batch(
        &pool,
        &[attachment(
            message_id,
            Some("bbbb0000000000000000000000000000"),
        )],
    )
    .await
    .expect("写入应成功");
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id.to_string())
        .execute(&pool)
        .await
        .expect("删除会话应成功");
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM message_attachments WHERE message_id = ?")
            .bind(message_id.to_string())
            .fetch_one(&pool)
            .await
            .expect("计数应成功");
    assert_eq!(count, 0, "删除会话后附件应经消息级联删除");
}
