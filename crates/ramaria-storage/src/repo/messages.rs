//! rust/crates/ramaria-storage/src/repo/messages.rs - L0 原始消息存取模块
//!
//! 设计特点:
//! - id 使用 UUID v4（TEXT 主键），与 sessions 保持 ID 类型一致
//! - 支持按 session_id 查询完整对话历史、按 persona_uid 过滤发言人消息
//! - find_by_fingerprint 用于历史导入去重（SHA-256 前 16 位 hex）
//! - role/source 解析失败时记录 WARNING 日志并回退到安全默认值
//! - UUID 解析异常时记录 WARNING，不静默吞错

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
        let id = ramaria_core::types::uuid_from_db(&self.id)
            .inspect_err(|_| tracing::warn!(raw_id = %self.id, "messages.id UUID 解析失败"))?;
        let session_id = ramaria_core::types::uuid_from_db(&self.session_id).inspect_err(
            |_| tracing::warn!(raw_id = %self.session_id, "messages.session_id UUID 解析失败"),
        )?;

        let role = match self.role.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "system" => MessageRole::System,
            "tool" => MessageRole::Tool,
            other => {
                tracing::warn!(%other, "messages.role 值非法，回退为 Tool");
                MessageRole::Tool
            }
        };

        let source = match self.source.as_str() {
            "online" => MessageSource::Online,
            "local" => MessageSource::Local,
            other => {
                tracing::warn!(%other, "messages.source 值非法，回退为 Local");
                MessageSource::Local
            }
        };

        Ok(Message {
            id,
            session_id,
            role,
            content: self.content,
            created_at: self.created_at,
            source,
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

pub async fn list_by_persona(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Vec<Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid
         FROM messages WHERE persona_uid = ? ORDER BY created_at DESC LIMIT 200",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("按 persona 查询消息失败", e))?;
    rows.into_iter()
        .map(|r| r.into_message())
        .collect::<RamariaResult<Vec<_>>>()
}
