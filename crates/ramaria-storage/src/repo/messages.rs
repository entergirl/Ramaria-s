//! rust/crates/ramaria-storage/src/repo/messages.rs - L0 消息 CRUD
//!
//! 设计特点:
//! - 按 session_id 查询，按 created_at 升序排列
//! - fingerprint 去重查询用于历史导入
//! - 支持 source（local/online）、role 枚举持久化

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Message, MessageRole, MessageSource};
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 保存消息。
pub async fn save_message(pool: &SqlitePool, message: &Message) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source, fingerprint) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(message.id.to_string())
    .bind(message.session_id.to_string())
    .bind(message.role.as_str())
    .bind(&message.content)
    .bind(message.created_at)
    .bind(message.source.to_string())
    .bind(&message.fingerprint)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存消息失败", e))?;
    Ok(())
}

/// 按 session_id 列出消息（时间升序）。
pub async fn list_messages(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query(
        "SELECT id, session_id, role, content, created_at, source, fingerprint \
         FROM messages WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出消息失败", e))?;

    rows.iter().map(row_to_message).collect()
}

/// 按指纹查找消息（去重用）。
pub async fn find_message_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: &str,
) -> RamariaResult<Option<Message>> {
    let row = sqlx::query(
        "SELECT id, session_id, role, content, created_at, source, fingerprint \
         FROM messages WHERE fingerprint = ? LIMIT 1",
    )
    .bind(fingerprint)
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("指纹查询消息失败", e))?;

    match row {
        Some(r) => Ok(Some(row_to_message(&r)?)),
        None => Ok(None),
    }
}

// =========================================================
// 行映射
// =========================================================

fn row_to_message(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<Message> {
    let id_str: String = row.get("id");
    let sid_str: String = row.get("session_id");
    let role_str: String = row.get("role");
    let source_str: String = row.get("source");

    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("message ID 格式非法", e))?;
    let session_id = Uuid::parse_str(&sid_str)
        .map_err(|e| RamariaError::storage_with_source("message session_id 格式非法", e))?;
    let role = match role_str.as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" => MessageRole::Tool,
        other => {
            return Err(RamariaError::storage(format!("未知消息角色: {other}")));
        }
    };
    let source = match source_str.as_str() {
        "local" => MessageSource::Local,
        "online" => MessageSource::Online,
        other => {
            return Err(RamariaError::storage(format!("未知消息来源: {other}")));
        }
    };

    Ok(Message {
        id,
        session_id,
        role,
        content: row.get("content"),
        created_at: row.get("created_at"),
        source,
        fingerprint: row.get("fingerprint"),
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;
    use crate::repo::sessions;

    #[tokio::test]
    async fn save_and_list_messages() {
        let pool = test_pool().await.unwrap();
        let session = ramaria_core::types::Session::new();
        sessions::create_session(&pool, &session).await.unwrap();

        let msg1 = Message::new(
            session.id,
            MessageRole::User,
            "你好".into(),
            MessageSource::Local,
        );
        let msg2 = Message::new(
            session.id,
            MessageRole::Assistant,
            "你好！有什么可以帮你的？".into(),
            MessageSource::Local,
        );

        save_message(&pool, &msg1).await.unwrap();
        save_message(&pool, &msg2).await.unwrap();

        let msgs = list_messages(&pool, session.id).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "你好");
        assert_eq!(msgs[1].content, "你好！有什么可以帮你的？");
        assert!(msgs[0].created_at <= msgs[1].created_at);
    }

    #[tokio::test]
    async fn fingerprint_dedup_works() {
        let pool = test_pool().await.unwrap();
        let session = ramaria_core::types::Session::new();
        sessions::create_session(&pool, &session).await.unwrap();

        let mut msg = Message::new(
            session.id,
            MessageRole::User,
            "去重测试".into(),
            MessageSource::Local,
        );
        msg.fingerprint = Some("abc123def456".into());

        save_message(&pool, &msg).await.unwrap();

        let found = find_message_by_fingerprint(&pool, "abc123def456")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().content, "去重测试");

        let not_found = find_message_by_fingerprint(&pool, "nonexistent")
            .await
            .unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn save_message_with_online_source() {
        let pool = test_pool().await.unwrap();
        let session = ramaria_core::types::Session::new();
        sessions::create_session(&pool, &session).await.unwrap();

        let msg = Message::new(
            session.id,
            MessageRole::Assistant,
            "线上回复".into(),
            MessageSource::Online,
        );

        save_message(&pool, &msg).await.unwrap();

        let msgs = list_messages(&pool, session.id).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].source, MessageSource::Online);
    }
}
