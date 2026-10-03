//! crates/ramaria-importer/src/writer/tests.rs - 导入写入层单元测试
//!
//! 设计特点:
//! - 覆盖导入侧过滤（Me / Other / Both）的写入与画像归属
//! - 覆盖跨文件去重、本批内同指纹去重与不同指纹正常写入
//! - 覆盖失败补偿删除（不留半成品会话）与批量写入整体回滚
//! - 覆盖画像缺失的丢弃统计（session 级与消息级）
//! - 使用单连接内存库与最小 schema，不依赖真实数据

use super::*;

/// 构造一个含 self + other 各 1 条消息的 session。
fn make_side_session(self_content: &str, other_content: &str) -> crate::traits::ImportedSession {
    crate::traits::ImportedSession {
        messages: vec![
            crate::traits::ParsedMessage {
                role: "user".to_string(),
                content: self_content.to_string(),
                created_at: 1100,
                fingerprint: format!("f-self-{self_content}"),
                sender_uid: "SELF_UID".to_string(),
                sender_uin: Some("10001".to_string()),
                sender_name: "我".to_string(),
            },
            crate::traits::ParsedMessage {
                role: "assistant".to_string(),
                content: other_content.to_string(),
                created_at: 1200,
                fingerprint: format!("f-other-{other_content}"),
                sender_uid: "OTHER_UID".to_string(),
                sender_uin: Some("20002".to_string()),
                sender_name: "对方".to_string(),
            },
        ],
        started_at: 1000,
        ended_at: 2000,
    }
}

/// 创建单连接内存库（max_connections=1 保证 sqlite::memory: 共享同一库）。
async fn test_pool() -> sqlx::SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let options = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    // 最小 schema（sessions + messages，对应 create_historical / save_import_batch 所需列）
    // sessions 含 channel（导入会话落默认通道 'local'）与 external_ref（外部对话标识，导入为空）
    sqlx::query(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            started_at INTEGER NOT NULL,
            ended_at INTEGER,
            persona_uid TEXT,
            channel TEXT NOT NULL DEFAULT 'local',
            external_ref TEXT
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            source TEXT NOT NULL,
            import_fingerprint TEXT UNIQUE,
            persona_uid TEXT,
            is_proactive INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn msg_count(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn msg_persona_uids(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT persona_uid FROM messages ORDER BY created_at")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn session_owner(pool: &sqlx::SqlitePool) -> Option<String> {
    sqlx::query_scalar("SELECT persona_uid FROM sessions")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// side=self（Me）：只写我方消息，跳过侧（对方）零消息零画像；session 归属我方。
#[tokio::test]
async fn write_l0_side_me_filters_other() {
    let pool = test_pool().await;
    let sessions = vec![make_side_session("我的发言", "对方发言")];

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        Some("user-0001"),
        None, // side=Me：对方画像不创建
        "SELF_UID",
        ImportSide::Me,
    )
    .await
    .unwrap();

    assert_eq!(outcome.sessions_written, 1);
    assert_eq!(outcome.messages_written, 1, "跳过侧消息必须不入库");
    assert_eq!(msg_count(&pool).await, 1);
    assert_eq!(msg_persona_uids(&pool).await, vec!["user-0001".to_string()]);
    assert_eq!(session_owner(&pool).await.as_deref(), Some("user-0001"));
}

/// side=other：只写对方消息，我方画像不创建；session 归属对方。
#[tokio::test]
async fn write_l0_side_other_filters_self() {
    let pool = test_pool().await;
    let sessions = vec![make_side_session("我的发言", "对方发言")];

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        None, // side=Other：我方画像不创建
        Some("char-0001"),
        "SELF_UID",
        ImportSide::Other,
    )
    .await
    .unwrap();

    assert_eq!(outcome.sessions_written, 1);
    assert_eq!(outcome.messages_written, 1, "我方消息必须被过滤");
    assert_eq!(msg_count(&pool).await, 1);
    assert_eq!(msg_persona_uids(&pool).await, vec!["char-0001".to_string()]);
    assert_eq!(session_owner(&pool).await.as_deref(), Some("char-0001"));
}

/// side=both（默认）：双方消息全部写入。
#[tokio::test]
async fn write_l0_side_both_keeps_all() {
    let pool = test_pool().await;
    let sessions = vec![make_side_session("我的发言", "对方发言")];

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        Some("user-0001"),
        Some("char-0001"),
        "SELF_UID",
        ImportSide::Both,
    )
    .await
    .unwrap();

    assert_eq!(outcome.sessions_written, 1);
    assert_eq!(outcome.messages_written, 2, "both 模式双方消息全部入库");
    assert_eq!(
        msg_persona_uids(&pool).await,
        vec!["user-0001".to_string(), "char-0001".to_string()]
    );
}

/// 单侧模式下 session 内全部为跳过侧消息 → 不创建空 session（零消息零 session）。
#[tokio::test]
async fn write_l0_side_skips_empty_session() {
    let pool = test_pool().await;
    // 只有 self 消息的 session，side=Other → 全部过滤 → session 不创建
    let sessions = vec![make_side_session("我的发言", "对方发言")];
    let mut only_self = sessions;
    only_self[0].messages.retain(|m| m.sender_uid == "SELF_UID");

    let outcome = ImportWriter::write_l0(
        &pool,
        &only_self,
        None,
        Some("char-0001"),
        "SELF_UID",
        ImportSide::Other,
    )
    .await
    .unwrap();

    assert_eq!(outcome.sessions_written, 0, "全过滤 session 不应创建");
    assert_eq!(outcome.messages_written, 0);
    assert_eq!(msg_count(&pool).await, 0);
    let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(session_count, 0);
}

/// 构造只含一条 self 消息、可指定指纹的 session。
fn make_dedup_session(self_content: &str, fingerprint: &str) -> crate::traits::ImportedSession {
    crate::traits::ImportedSession {
        messages: vec![crate::traits::ParsedMessage {
            role: "user".to_string(),
            content: self_content.to_string(),
            created_at: 1100,
            fingerprint: fingerprint.to_string(),
            sender_uid: "SELF_UID".to_string(),
            sender_uin: Some("10001".to_string()),
            sender_name: "我".to_string(),
        }],
        started_at: 1000,
        ended_at: 2000,
    }
}

/// 预插一条 fingerprint 记录到库中（session_id/id 用合法 UUID，便于 find_by_fingerprint 反解）。
async fn preseed_fingerprint(pool: &sqlx::SqlitePool, fp: &str) {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source, import_fingerprint, persona_uid) \
         VALUES (?, ?, 'user', '预插内容', 100, 'local', ?, 'user-0001')",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(fp)
    .execute(pool)
    .await
    .expect("预插指纹失败");
}

/// 指纹已在库中的消息被跨文件去重跳过（messages_written=0，不触发 UNIQUE）。
#[tokio::test]
async fn write_l0_skips_existing_fingerprint() {
    let pool = test_pool().await;
    preseed_fingerprint(&pool, "fp-existing").await;
    let sessions = vec![make_dedup_session("我的发言", "fp-existing")];

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        Some("user-0001"),
        None,
        "SELF_UID",
        ImportSide::Me,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.sessions_written, 1,
        "session 仍会创建（去重只跳过消息）"
    );
    assert_eq!(outcome.messages_written, 0, "同指纹消息应被跳过");
    // 库中仍只有预插的那一条
    assert_eq!(msg_count(&pool).await, 1);
}

/// 同一 session 内两条指纹相同的消息（如同一通话记录被导出两次）→ 只写一条，
/// 不触发 messages.import_fingerprint 全局 UNIQUE（回归 T-V20-8-001 首次导入失败）。
#[tokio::test]
async fn write_l0_dedups_within_batch_same_fingerprint() {
    let pool = test_pool().await;
    let mut session = make_dedup_session("通话 - 通话时长 26:41", "fp-dup");
    // 再压入一条指纹完全相同、content/时间相同的消息（QQChatExporter 重复导出形态）
    session.messages.push(crate::traits::ParsedMessage {
        role: "user".to_string(),
        content: "通话 - 通话时长 26:41".to_string(),
        created_at: 1100,
        fingerprint: "fp-dup".to_string(),
        sender_uid: "SELF_UID".to_string(),
        sender_uin: Some("10001".to_string()),
        sender_name: "我".to_string(),
    });

    let outcome = ImportWriter::write_l0(
        &pool,
        &[session],
        Some("user-0001"),
        None,
        "SELF_UID",
        ImportSide::Me,
    )
    .await
    .unwrap();

    assert_eq!(outcome.sessions_written, 1, "session 正常创建");
    assert_eq!(
        outcome.messages_written, 1,
        "同批重复指纹只写一条，不撞 UNIQUE"
    );
    assert_eq!(msg_count(&pool).await, 1);
}

/// 跨 session 重复指纹：第二个 session 仅含已在本批首见指纹的消息 →
/// 去重后 kept 为空 → 不创建空 session（与"全过滤 session 不创建"一致）。
#[tokio::test]
async fn write_l0_dedups_across_sessions_same_fingerprint() {
    let pool = test_pool().await;
    let s1 = make_dedup_session("我的发言", "fp-shared");
    let s2 = make_dedup_session("我的发言", "fp-shared");

    let outcome = ImportWriter::write_l0(
        &pool,
        &[s1, s2],
        Some("user-0001"),
        None,
        "SELF_UID",
        ImportSide::Me,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.sessions_written, 1,
        "第二个全重复 session 不创建空 session"
    );
    assert_eq!(
        outcome.messages_written, 1,
        "首 session 写入一条，重复被跳过"
    );
    assert_eq!(msg_count(&pool).await, 1);
}

/// 不同指纹正常写入，不被跨文件去重误杀。
#[tokio::test]
async fn write_l0_writes_distinct_fingerprint() {
    let pool = test_pool().await;
    preseed_fingerprint(&pool, "fp-existing").await;
    let sessions = vec![make_dedup_session("我的发言", "fp-new")];

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        Some("user-0001"),
        None,
        "SELF_UID",
        ImportSide::Me,
    )
    .await
    .unwrap();

    assert_eq!(outcome.sessions_written, 1);
    assert_eq!(outcome.messages_written, 1, "不同指纹应正常写入");
    assert_eq!(msg_count(&pool).await, 2);
}

/// 批量写入失败（同批两条空指纹消息撞 import_fingerprint UNIQUE）→
/// 返回 Err，且补偿删除本批刚创建的 session（不留半成品会话）。
#[tokio::test]
async fn write_l0_batch_failure_removes_created_session() {
    let pool = test_pool().await;
    // 空指纹绕过批内去重与跨文件查重，两条消息落入同一 batch 触发 UNIQUE 冲突
    let mut session = make_dedup_session("第一条", "");
    session.messages.push(crate::traits::ParsedMessage {
        role: "user".to_string(),
        content: "第二条".to_string(),
        created_at: 1200,
        fingerprint: String::new(),
        sender_uid: "SELF_UID".to_string(),
        sender_uin: Some("10001".to_string()),
        sender_name: "我".to_string(),
    });

    let result = ImportWriter::write_l0(
        &pool,
        &[session],
        Some("user-0001"),
        None,
        "SELF_UID",
        ImportSide::Me,
    )
    .await;

    assert!(result.is_err(), "批量写入失败应中止本批导入");
    // 补偿删除：失败时既不留 session，也不留消息（批量事务整体回滚）
    let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(session_count, 0, "失败批次创建的 session 应被补偿删除");
    assert_eq!(msg_count(&pool).await, 0, "失败批次的批量写入应整体回滚");
}

/// 归属侧画像缺失（side=Me 且我方画像未创建）→ 该 session 不入库，
/// 已过滤待写消息全部计入 messages_dropped（不静默丢失统计）。
#[tokio::test]
async fn write_l0_owner_persona_missing_counts_dropped() {
    let pool = test_pool().await;
    let sessions = vec![make_side_session("我的发言", "对方发言")];

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        None, // side=Me：我方画像未创建（防御场景）
        Some("char-0001"),
        "SELF_UID",
        ImportSide::Me,
    )
    .await
    .unwrap();

    assert_eq!(outcome.sessions_written, 0);
    assert_eq!(outcome.messages_written, 0);
    assert!(
        outcome.messages_dropped > 0,
        "归属画像缺失时消息必须计入丢弃数"
    );
    assert_eq!(msg_count(&pool).await, 0);
}

/// side=Both 且我方画像缺失（防御场景）→ session 按既有口径归属对方，
/// 对方消息入库，我方消息在消息级逐条计入 messages_dropped。
#[tokio::test]
async fn write_l0_missing_self_persona_drops_self_messages() {
    let pool = test_pool().await;
    let sessions = vec![make_side_session("我的发言", "对方发言")];

    let outcome = ImportWriter::write_l0(
        &pool,
        &sessions,
        None, // 我方画像缺失（防御场景）
        Some("char-0001"),
        "SELF_UID",
        ImportSide::Both,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.sessions_written, 1,
        "Both 模式 session 归属对方画像"
    );
    assert_eq!(outcome.messages_written, 1, "对方消息应正常入库");
    assert_eq!(outcome.messages_dropped, 1, "我方消息应计入丢弃数");
    assert_eq!(msg_count(&pool).await, 1);
}

/// ImportSide::parse_cli 解析（self|other|both；非法值报错）。
#[test]
fn import_side_parse_cli() {
    assert_eq!(ImportSide::parse_cli(None).unwrap(), ImportSide::Both);
    assert_eq!(
        ImportSide::parse_cli(Some("both")).unwrap(),
        ImportSide::Both
    );
    assert_eq!(ImportSide::parse_cli(Some("SELF")).unwrap(), ImportSide::Me);
    assert_eq!(ImportSide::parse_cli(Some("me")).unwrap(), ImportSide::Me);
    assert_eq!(
        ImportSide::parse_cli(Some("other")).unwrap(),
        ImportSide::Other
    );
    assert!(ImportSide::parse_cli(Some("all")).is_err());
}
