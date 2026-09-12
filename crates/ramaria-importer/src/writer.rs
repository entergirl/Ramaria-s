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
use ramaria_core::privacy::mask_id;
use sqlx::SqlitePool;

use crate::traits::{ImportSide, ImportedSession};

// =========================================================
// 写入结果
// =========================================================

/// L0 写入结果，供调用方展示统计并触发后续深度处理。
///
/// 字段约定:
/// - `session_ids`: 已创建的历史 session UUID 列表，供调用方触发 L1 摘要等深度处理。
/// - `messages_dropped`: 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息），
///   调用方应将其作为"非静默降级"提示给用户，而不是仅存在于日志中。
pub struct WriteOutcome {
    /// 成功写入的 session 数
    pub sessions_written: usize,
    /// 成功写入的消息数
    pub messages_written: usize,
    /// 创建的 session UUID 列表（供调用方触发 L1 摘要等深度处理）
    pub session_ids: Vec<uuid::Uuid>,
    /// 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息）
    pub messages_dropped: usize,
}

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
    /// - 本批（write_l0 调用内）已见指纹集合去重：同一批次内指纹相同的消息
    ///   （如同一通话记录被导出两次）跳过，避免撞 messages.import_fingerprint 全局 UNIQUE。
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
    /// - `WriteOutcome`: 写入统计、创建的 session UUID 列表与画像缺失丢弃计数。
    ///
    /// 说明:
    /// - 每个 session 创建为已关闭的历史 session。
    /// - 消息使用 `save_import_batch` 批量写入，绕过 session 活跃状态检查。
    /// - 返回的 session_ids 供调用方触发 L1 摘要等深度处理。
    /// - 查重或批量写入失败时中止本批导入，并补偿删除刚创建的 session（不留半成品会话）。
    pub async fn write_l0(
        pool: &SqlitePool,
        sessions: &[ImportedSession],
        self_persona_uid: Option<&str>,
        other_persona_uid: Option<&str>,
        self_uid: &str,
        side: ImportSide,
    ) -> RamariaResult<WriteOutcome> {
        let mut sessions_written = 0usize;
        let mut messages_written = 0usize;
        let mut session_ids: Vec<uuid::Uuid> = Vec::new();
        // 因画像缺失被丢弃的消息数（含整段 session 被丢弃的消息）
        let mut messages_dropped = 0usize;
        // 分别统计双方消息数，用于日志输出
        let mut self_msg_count = 0usize;
        let mut other_msg_count = 0usize;
        // 跨文件去重统计：指纹已在库中被跳过的消息数（不记内容）
        let mut dedup_skipped = 0usize;
        // 本批（write_l0 调用内、跨全部 session）已见的指纹集合：
        // 数据库查重只覆盖"已提交历史"，同一批次内两条指纹相同的消息（如同一通话记录被
        // 导出两次）若不在此拦截，会在 save_import_batch 撞 messages.import_fingerprint 的
        // 全局 UNIQUE。此处与跨文件去重同语义，仅把去重范围扩到"本批已见"。
        let mut seen_fingerprints: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for session in sessions {
            // 过滤本 session 消息（按 side）：跳过侧消息不入库
            let mut kept: Vec<(bool, &crate::traits::ParsedMessage)> = Vec::new();
            for parsed in &session.messages {
                let is_self = parsed.sender_uid == self_uid;
                match (side, is_self) {
                    (ImportSide::Me, false) | (ImportSide::Other, true) => continue,
                    _ => {}
                }
                // 本批已见指纹去重：重复 → 跳过（不记内容）
                if !parsed.fingerprint.is_empty()
                    && !seen_fingerprints.insert(parsed.fingerprint.clone())
                {
                    dedup_skipped += 1;
                    tracing::debug!(
                        fp_tail = %&parsed.fingerprint[parsed.fingerprint.len().saturating_sub(4)..],
                        "消息在本批内指纹重复，跳过"
                    );
                    continue;
                }
                kept.push((is_self, parsed));
            }

            // 全部消息被过滤（单侧无该侧消息）→ 跳过该 session（不创建空 session）
            if kept.is_empty() {
                continue;
            }

            // 创建历史 session（已关闭）；归属为处理侧画像（Both 模式归属对方）。
            let owner = match side {
                ImportSide::Me => self_persona_uid,
                ImportSide::Other | ImportSide::Both => other_persona_uid,
            };
            let Some(owner_uid) = owner else {
                // 防御：处理侧归属画像必须已创建（调用方保证）
                messages_dropped += kept.len();
                tracing::warn!(
                    kept = kept.len(),
                    side = ?side,
                    "session 归属画像未创建，跳过该 session 并将其消息计入丢弃数（导入侧过滤不一致）"
                );
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
                if !parsed.fingerprint.is_empty() {
                    match ramaria_storage::repo::messages::find_by_fingerprint(
                        pool,
                        &parsed.fingerprint,
                    )
                    .await
                    {
                        Ok(Some(existing)) => {
                            dedup_skipped += 1;
                            tracing::debug!(
                                existing_id = %existing.id,
                                fp_tail = %&parsed.fingerprint[parsed.fingerprint.len().saturating_sub(4)..],
                                "消息已在库中，跨文件去重跳过"
                            );
                            continue;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            // 查重失败若继续导入，后续 UNIQUE 冲突会留下语义不明的错误；
                            // 明确中止本批并补偿删除刚建 session（不留半成品会话）。
                            tracing::error!(
                                session_id = %db_session.id,
                                error = %e,
                                "跨文件指纹查重失败，中止本批导入"
                            );
                            rollback_created_session(pool, db_session.id).await;
                            return Err(e);
                        }
                    }
                }

                let persona_for_msg = if is_self {
                    self_msg_count += 1;
                    self_persona_uid
                } else {
                    other_msg_count += 1;
                    other_persona_uid
                };
                let Some(persona_uid) = persona_for_msg else {
                    // 防御：单侧模式下不应出现跳过侧消息（已过滤），出现则丢弃记 warn；
                    // sender 为个人标识，日志只记掩码。
                    messages_dropped += 1;
                    tracing::warn!(
                        sender = %mask_id(&parsed.sender_uid),
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

            // 单事务批量写入（替代逐条 INSERT，显著降低大文件导入的 fsync 开销）；
            // 失败时事务整体回滚，并补偿删除刚建 session（不留半成品会话）。
            let msg_count =
                match ramaria_storage::repo::messages::save_import_batch(pool, &batch).await {
                    Ok(written) => written,
                    Err(e) => {
                        tracing::error!(
                            session_id = %db_session.id,
                            error = %e,
                            "批量写入导入消息失败，中止本批导入"
                        );
                        rollback_created_session(pool, db_session.id).await;
                        return Err(e);
                    }
                };

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
            messages_dropped = messages_dropped,
            self_persona = ?self_persona_uid,
            other_persona = ?other_persona_uid,
            side = ?side,
            "双画像导入统计"
        );

        if messages_dropped > 0 {
            tracing::warn!(
                messages_dropped = messages_dropped,
                "本次导入存在因画像缺失被丢弃的消息（详见上方逐条警告，不记录消息内容）"
            );
        }

        Ok(WriteOutcome {
            sessions_written,
            messages_written,
            session_ids,
            messages_dropped,
        })
    }
}

/// 补偿删除本批新建的 session（写入失败回滚）。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `session_id`: 需要删除的 session UUID。
///
/// 说明:
/// - 供 `write_l0` 在查重或批量写入失败时调用，保持"失败不留半成品会话"。
/// - 删除失败仅记录 error 日志（提示重跑导入可能重复创建该会话），不覆盖调用方的原始错误。
async fn rollback_created_session(pool: &sqlx::SqlitePool, session_id: uuid::Uuid) {
    if let Err(del_err) = ramaria_storage::repo::sessions::delete(pool, session_id).await {
        tracing::error!(
            session_id = %session_id,
            error = %del_err,
            "补偿删除已创建 session 失败，重跑导入可能重复创建该会话"
        );
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
}
