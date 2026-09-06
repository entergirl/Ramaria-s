//! crates/ramaria-importer/src/writer.rs - 导入源无关的会话/消息写入层
//!
//! 设计特点:
//! - `ImportWriter` 将"解析后的标准化聊天记录写入存储"这一层从导入源中抽离出来，
//!   供 QQ 及未来的微信/Telegram 等人对人导入源复用。
//! - 接收导入源无关的中间数据（`ImportedSession` / `ParsedMessage`）与画像归属参数，
//!   负责：按导入侧过滤消息、创建历史 session、按发送方归属 persona、批量写入、
//!   以及跨文件去重查重（指纹已在库中的消息跳过）。
//! - 只关心 L0（messages/sessions）写入；L1/L2/L3 深度处理由调用方在拿到
//!   返回的 `session_ids` 后自行触发，writer 不做任何 LLM 相关衔接。
//! - 平台特有逻辑（QQ 号/QQ UID → persona UID、source 归属等）不在此层，
//!   由各导入源在调用前解析好 persona_uid 再传入。
//! - 跨文件去重复用 `ramaria_storage::repo::messages::find_by_fingerprint` 同一路径，
//!   本层不重复实现去重规则。

use ramaria_core::error::RamariaResult;
use sqlx::SqlitePool;

use crate::traits::{ImportSide, ImportedSession};

// =========================================================
// 通用写入器
// =========================================================

///
/// 导入源无关的 L0 会话/消息写入器。
///
/// 职责:
/// - 把一组已解析、已做画像归属的 session 批量写入存储（仅 L0）。
/// - 处理导入侧过滤（`ImportSide`）、会话归属画像、跨文件指纹去重。
///
/// 说明:
/// - 通过关联方法 `write_l0` 调用，不持有跨调用状态。
pub struct ImportWriter;

impl ImportWriter {
    /// 写入已解析的会话与消息（仅 L0，即 messages/sessions 表）。
    ///
    /// 双画像归属:
    /// - 根据每条消息的发送者（`sender_uid == self_uid`）区分画像归属。
    /// - 导出者本人的消息关联 `self_persona_uid`，对方消息关联 `other_persona_uid`。
    ///
    /// 导入侧过滤:
    /// - `side` 控制只处理某一侧：`Me` 只写我方消息、`Other` 只写对方消息、
    ///   `Both` 全部写入（默认）。跳过侧消息不入库；该侧 persona 由调用方不创建。
    /// - 单侧模式下，跳过侧的 `persona_uid` 传 `None`（不会在消息中出现）；
    ///   session 归属为处理侧画像。
    ///
    /// 去重:
    /// - 复用存储层指纹查重，指纹已在库中的消息跨文件去重跳过。
    /// - 只记计数与指纹尾段，不记消息内容/昵称/QQ 号。
    ///
    /// 参数:
    /// - `pool`: 数据库连接池。
    /// - `sessions`: 解析后的 session 列表。
    /// - `self_persona_uid`: 导出者本人的画像标识（`side=Other` 时为 None）。
    /// - `other_persona_uid`: 对话对方的画像标识（`side=Me` 时为 None）。
    /// - `self_uid`: 导出者的平台内部 UID（用于与消息的 sender_uid 比较）。
    /// - `side`: 导入侧过滤（self|other|both）。
    ///
    /// 返回:
    /// - `(sessions_written, messages_written, session_ids)`: 写入统计及创建的 session UUID 列表。
    ///
    /// 说明:
    /// - 每个 session 创建为已关闭的历史 session。
    /// - 消息使用 `save_import_batch` 批量写入，绕过 session 活跃状态检查。
    /// - 返回的 session_ids 供调用方触发 L1 摘要等深度处理。
    pub async fn write_l0(
        pool: &SqlitePool,
        sessions: &[ImportedSession],
        self_persona_uid: Option<&str>,
        other_persona_uid: Option<&str>,
        self_uid: &str,
        side: ImportSide,
    ) -> RamariaResult<(usize, usize, Vec<uuid::Uuid>)> {
        let mut sessions_written = 0usize;
        let mut messages_written = 0usize;
        let mut session_ids: Vec<uuid::Uuid> = Vec::new();
        // 分别统计双方消息数，用于日志输出
        let mut self_msg_count = 0usize;
        let mut other_msg_count = 0usize;
        // 跨文件去重统计：指纹已在库中被跳过的消息数（不记内容）
        let mut dedup_skipped = 0usize;

        for session in sessions {
            // 过滤本 session 消息（按 side）：跳过侧消息不入库
            let mut kept: Vec<(bool, &crate::traits::ParsedMessage)> = Vec::new();
            for parsed in &session.messages {
                let is_self = parsed.sender_uid == self_uid;
                match (side, is_self) {
                    (ImportSide::Me, false) | (ImportSide::Other, true) => continue,
                    _ => {}
                }
                kept.push((is_self, parsed));
            }

            // 全部消息被过滤（单侧无该侧消息）→ 跳过该 session（不创建空 session）
            if kept.is_empty() {
                continue;
            }

            // 创建历史 session（已关闭）；归属为处理侧画像
            let owner = match side {
                ImportSide::Me => self_persona_uid,
                _ => other_persona_uid,
            };
            let Some(owner_uid) = owner else {
                // 防御：单侧模式下归属侧画像必须已创建（调用方保证）
                tracing::warn!("session 归属画像未创建，跳过该 session");
                continue;
            };

            let db_session = ramaria_storage::repo::sessions::create_historical(
                pool,
                session.started_at,
                session.ended_at,
                owner_uid,
            )
            .await
            .map_err(|e| {
                tracing::error!(session_start = %session.started_at, error = %e, "创建历史 session 失败");
                e
            })?;

            // 构造消息（按发送者分配 persona_uid；单侧模式下跳过侧不会出现）
            // 写入前按指纹查重：已入库的消息跨文件去重跳过，避免 UNIQUE 冲突与重复入库。
            let mut batch: Vec<ramaria_core::types::Message> = Vec::with_capacity(kept.len());
            for (is_self, parsed) in kept {
                // 跨文件去重：指纹已在库中 → 跳过（只记计数与指纹尾段，不记内容/昵称/QQ 号）
                if !parsed.fingerprint.is_empty()
                    && let Some(existing) = ramaria_storage::repo::messages::find_by_fingerprint(
                        pool,
                        &parsed.fingerprint,
                    )
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e, "跨文件指纹查重失败，跳过查重继续导入");
                        e
                    })?
                {
                    dedup_skipped += 1;
                    tracing::debug!(
                        existing_id = %existing.id,
                        fp_tail = %&parsed.fingerprint[parsed.fingerprint.len().saturating_sub(4)..],
                        "消息已在库中，跨文件去重跳过"
                    );
                    continue;
                }

                let persona_for_msg = if is_self {
                    self_msg_count += 1;
                    self_persona_uid
                } else {
                    other_msg_count += 1;
                    other_persona_uid
                };
                let Some(persona_uid) = persona_for_msg else {
                    // 防御：单侧模式下不应出现跳过侧消息（已过滤），出现则丢弃记 warn
                    tracing::warn!(
                        sender = %parsed.sender_uid,
                        "消息发送侧画像未创建，丢弃该消息（导入侧过滤不一致）"
                    );
                    continue;
                };

                batch.push(ramaria_core::types::Message {
                    id: ramaria_core::types::new_id(),
                    session_id: db_session.id,
                    role: if parsed.role == "user" {
                        ramaria_core::types::MessageRole::User
                    } else {
                        ramaria_core::types::MessageRole::Assistant
                    },
                    content: parsed.content.clone(),
                    created_at: parsed.created_at,
                    source: ramaria_core::types::MessageSource::Local,
                    fingerprint: Some(parsed.fingerprint.clone()),
                    persona_uid: Some(persona_uid.to_string()),
                });
            }

            // 单事务批量写入（替代逐条 INSERT，显著降低大文件导入的 fsync 开销）
            let written = ramaria_storage::repo::messages::save_import_batch(pool, &batch)
                .await
                .map_err(|e| {
                    tracing::error!(session_id = %db_session.id, error = %e, "批量写入导入消息失败");
                    e
                })?;
            let msg_count = written;

            session_ids.push(db_session.id);
            sessions_written += 1;
            messages_written += msg_count;

            if sessions_written.is_multiple_of(10) {
                tracing::info!(
                    sessions_written = sessions_written,
                    total_sessions = sessions.len(),
                    "快速导入进度"
                );
            }
        }

        tracing::info!(
            self_messages = self_msg_count,
            other_messages = other_msg_count,
            dedup_skipped = dedup_skipped,
            self_persona = ?self_persona_uid,
            other_persona = ?other_persona_uid,
            side = ?side,
            "双画像导入统计"
        );

        Ok((sessions_written, messages_written, session_ids))
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个含 self + other 各 1 条消息的 session。
    fn make_side_session(
        self_content: &str,
        other_content: &str,
    ) -> crate::traits::ImportedSession {
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
        sqlx::query(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                persona_uid TEXT
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
                persona_uid TEXT
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

        let (sessions_written, messages_written, _) = ImportWriter::write_l0(
            &pool,
            &sessions,
            Some("user-0001"),
            None, // side=Me：对方画像不创建
            "SELF_UID",
            ImportSide::Me,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1);
        assert_eq!(messages_written, 1, "跳过侧消息必须不入库");
        assert_eq!(msg_count(&pool).await, 1);
        assert_eq!(msg_persona_uids(&pool).await, vec!["user-0001".to_string()]);
        assert_eq!(session_owner(&pool).await.as_deref(), Some("user-0001"));
    }

    /// side=other：只写对方消息，我方画像不创建；session 归属对方。
    #[tokio::test]
    async fn write_l0_side_other_filters_self() {
        let pool = test_pool().await;
        let sessions = vec![make_side_session("我的发言", "对方发言")];

        let (sessions_written, messages_written, _) = ImportWriter::write_l0(
            &pool,
            &sessions,
            None, // side=Other：我方画像不创建
            Some("char-0001"),
            "SELF_UID",
            ImportSide::Other,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1);
        assert_eq!(messages_written, 1, "我方消息必须被过滤");
        assert_eq!(msg_count(&pool).await, 1);
        assert_eq!(msg_persona_uids(&pool).await, vec!["char-0001".to_string()]);
        assert_eq!(session_owner(&pool).await.as_deref(), Some("char-0001"));
    }

    /// side=both（默认）：双方消息全部写入。
    #[tokio::test]
    async fn write_l0_side_both_keeps_all() {
        let pool = test_pool().await;
        let sessions = vec![make_side_session("我的发言", "对方发言")];

        let (sessions_written, messages_written, _) = ImportWriter::write_l0(
            &pool,
            &sessions,
            Some("user-0001"),
            Some("char-0001"),
            "SELF_UID",
            ImportSide::Both,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1);
        assert_eq!(messages_written, 2, "both 模式双方消息全部入库");
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

        let (sessions_written, messages_written, _) = ImportWriter::write_l0(
            &pool,
            &only_self,
            None,
            Some("char-0001"),
            "SELF_UID",
            ImportSide::Other,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 0, "全过滤 session 不应创建");
        assert_eq!(messages_written, 0);
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

        let (sessions_written, messages_written, _) = ImportWriter::write_l0(
            &pool,
            &sessions,
            Some("user-0001"),
            None,
            "SELF_UID",
            ImportSide::Me,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1, "session 仍会创建（去重只跳过消息）");
        assert_eq!(messages_written, 0, "同指纹消息应被跳过");
        // 库中仍只有预插的那一条
        assert_eq!(msg_count(&pool).await, 1);
    }

    /// 不同指纹正常写入，不被跨文件去重误杀。
    #[tokio::test]
    async fn write_l0_writes_distinct_fingerprint() {
        let pool = test_pool().await;
        preseed_fingerprint(&pool, "fp-existing").await;
        let sessions = vec![make_dedup_session("我的发言", "fp-new")];

        let (sessions_written, messages_written, _) = ImportWriter::write_l0(
            &pool,
            &sessions,
            Some("user-0001"),
            None,
            "SELF_UID",
            ImportSide::Me,
        )
        .await
        .unwrap();

        assert_eq!(sessions_written, 1);
        assert_eq!(messages_written, 1, "不同指纹应正常写入");
        assert_eq!(msg_count(&pool).await, 2);
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
}
