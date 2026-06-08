//! rust/crates/ramaria-storage/src/repo/messages.rs - L0 消息 CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Message, MessageRole, MessageSource};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    session_id: String,
    role: String,
    content: String,
    created_at: i64,
    source: String,
    import_fingerprint: Option<String>,
    persona_uid: Option<String>,
}

impl MessageRow {
    fn into_message(self) -> RamariaResult<Message> {
        Ok(Message {
            id: ramaria_core::types::uuid_from_db(&self.id),
            session_id: ramaria_core::types::uuid_from_db(&self.session_id),
            role: match self.role.as_str() {
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "system" => MessageRole::System,
                _ => MessageRole::Tool,
            },
            content: self.content,
            created_at: self.created_at,
            source: if self.source == "online" {
                MessageSource::Online
            } else {
                MessageSource::Local
            },
            fingerprint: self.import_fingerprint,
            persona_uid: self.persona_uid,
        })
    }
}

pub async fn save(pool: &SqlitePool, msg: &Message) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, created_at, source, import_fingerprint, persona_uid)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
        .bind(msg.id.to_string())
        .bind(msg.session_id.to_string())
        .bind(msg.role.as_str())
        .bind(&msg.content)
        .bind(msg.created_at)
        .bind(msg.source.to_string())
        .bind(&msg.fingerprint)
        .bind(&msg.persona_uid)
        .execute(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("保存消息失败", e))?;
    Ok(())
}

pub async fn list_by_session(pool: &SqlitePool, session_id: Uuid) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
         FROM messages WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id.to_string())
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询消息列表失败", e))?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}

pub async fn find_by_fingerprint(
    pool: &SqlitePool,
    fingerprint: &str,
) -> RamariaResult<Option<Message>> {
    let row = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
         FROM messages WHERE import_fingerprint = ? LIMIT 1",
    )
    .bind(fingerprint)
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("指纹查询失败", e))?;
    row.map(|r| r.into_message()).transpose()
}
