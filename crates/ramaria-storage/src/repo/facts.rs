//! rust/crates/ramaria-storage/src/repo/facts.rs - PersonaFact CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{FactSource, PersonaFact, ProfileField};
use sqlx::SqlitePool;

fn parse_field(s: &str) -> ProfileField {
    match s {
        "basic_info" => ProfileField::BasicInfo,
        "personal_status" => ProfileField::PersonalStatus,
        "interests" => ProfileField::Interests,
        "social" => ProfileField::Social,
        "history" => ProfileField::History,
        "recent_context" => ProfileField::RecentContext,
        _ => ProfileField::SpeakingStyle,
    }
}

fn parse_fact_source(s: &str) -> FactSource {
    match s {
        "event" => FactSource::Event,
        "manual" => FactSource::Manual,
        _ => FactSource::L1,
    }
}

#[derive(sqlx::FromRow)]
struct FactRow {
    id: i64,
    persona_uid: String,
    field: String,
    content: String,
    source: String,
    ref_event_id: Option<i64>,
    ref_l1_id: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl FactRow {
    fn into_fact(self) -> PersonaFact {
        PersonaFact {
            id: self.id,
            persona_uid: self.persona_uid,
            field: parse_field(&self.field),
            content: self.content,
            source: parse_fact_source(&self.source),
            ref_event_id: self.ref_event_id,
            ref_l1_id: self
                .ref_l1_id
                .map(|s| ramaria_core::types::uuid_from_db(&s)),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

pub async fn save(pool: &SqlitePool, f: &PersonaFact) -> RamariaResult<i64> {
    sqlx::query(
        "INSERT INTO persona_facts (persona_uid, field, content, source, ref_event_id, ref_l1_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&f.persona_uid).bind(f.field.as_str()).bind(&f.content)
    .bind(f.source.as_str()).bind(f.ref_event_id).bind(f.ref_l1_id.map(|u| u.to_string()))
    .bind(f.created_at).bind(f.updated_at)
    .execute(pool).await
    .map_err(|e| RamariaError::storage_with_source("保存事实失败", e))?;

    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("获取事实 id 失败", e))?;
    Ok(id)
}

pub async fn list_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    field: &str,
) -> RamariaResult<Vec<PersonaFact>> {
    let rows = sqlx::query_as::<_, FactRow>(
        "SELECT id, persona_uid, field, content, source, ref_event_id, ref_l1_id, created_at, updated_at
         FROM persona_facts WHERE persona_uid = ? AND field = ? ORDER BY created_at DESC"
    ).bind(persona_uid).bind(field)
        .fetch_all(pool).await
        .map_err(|e| RamariaError::storage_with_source("查询事实列表失败", e))?;
    Ok(rows.into_iter().map(|r| r.into_fact()).collect())
}
