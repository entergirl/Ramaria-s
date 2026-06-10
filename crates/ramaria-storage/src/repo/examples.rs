//! rust/crates/ramaria-storage/src/repo/examples.rs - PersonaExample CRUD
//!
//! 设计特点:
//! - 管理对话 Few-shot 示例（partner→reply），用于 System Prompt 注入
//! - list_selected 仅返回 selected=1 的示例，最多 5 条

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::PersonaExample;
use sqlx::SqlitePool;

use super::last_insert_id;

#[derive(sqlx::FromRow)]
struct ExampleRow {
    id: i64,
    persona_uid: String,
    partner: String,
    reply: String,
    session_id: Option<String>,
    context: Option<String>,
    valence: f64,
    tags: Option<String>,
    selected: i64,
    length: i64,
    created_at: i64,
}

impl ExampleRow {
    fn into_example(self) -> RamariaResult<PersonaExample> {
        let session_id = self
            .session_id
            .as_deref()
            .map(ramaria_core::types::uuid_from_db)
            .transpose()
            .inspect_err(|_| tracing::warn!(raw_id = %self.session_id.as_deref().unwrap_or("nil"), "persona_examples.session_id UUID 解析失败"))?;
        Ok(PersonaExample {
            id: self.id,
            persona_uid: self.persona_uid,
            partner: self.partner,
            reply: self.reply,
            session_id,
            context: self.context,
            valence: self.valence,
            tags: self.tags,
            selected: self.selected != 0,
            length: self.length as i32,
            created_at: self.created_at,
        })
    }
}

pub async fn save(pool: &SqlitePool, e: &PersonaExample) -> RamariaResult<i64> {
    sqlx::query(
        "INSERT INTO persona_examples (persona_uid, partner, reply, session_id, context, valence, tags, selected, length, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&e.persona_uid).bind(&e.partner).bind(&e.reply)
    .bind(e.session_id.map(|u| u.to_string())).bind(&e.context)
    .bind(e.valence).bind(&e.tags).bind(e.selected as i64).bind(e.length).bind(e.created_at)
    .execute(pool).await
    .map_err(|e| RamariaError::storage_with_source("保存示例失败", e))?;

    last_insert_id(pool).await
}

pub async fn list_selected(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<PersonaExample>> {
    let rows = sqlx::query_as::<_, ExampleRow>(
        "SELECT id, persona_uid, partner, reply, session_id, context, valence, tags, selected, length, created_at
         FROM persona_examples WHERE persona_uid = ? AND selected = 1 ORDER BY created_at DESC LIMIT 5"
    ).bind(persona_uid).fetch_all(pool).await
        .map_err(|e| RamariaError::storage_with_source("查询示例列表失败", e))?;
    rows.into_iter()
        .map(|r| r.into_example())
        .collect::<RamariaResult<Vec<_>>>()
}
