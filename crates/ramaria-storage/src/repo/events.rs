//! rust/crates/ramaria-storage/src/repo/events.rs - MemoryEvent / EventRelation / EventSource CRUD

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{EventRelation, MemoryEvent, Presentation};
use sqlx::SqlitePool;
use uuid::Uuid;

// =========================================================
// MemoryEvent
// =========================================================

fn parse_presentation(s: &str) -> Presentation {
    match s {
        "objective" => Presentation::Objective,
        "subjective" => Presentation::Subjective,
        _ => Presentation::Mixed,
    }
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    persona_uid: String,
    title: String,
    summary: String,
    keywords: Option<String>,
    participants: Option<String>,
    start: i64,
    end: i64,
    confidence: f64,
    salience: f64,
    valence: f64,
    presentation: String,
    share: f64,
    attitude: Option<String>,
    paraphrase: Option<String>,
    absorbed: i64,
    created_at: i64,
    last_accessed_at: Option<i64>,
    indexed_at: Option<i64>,
    index_version: Option<i64>,
}

impl EventRow {
    fn into_event(self) -> MemoryEvent {
        MemoryEvent {
            id: self.id,
            persona_uid: self.persona_uid,
            title: self.title,
            summary: self.summary,
            keywords: self.keywords,
            participants: self.participants,
            start: self.start,
            end: self.end,
            confidence: self.confidence,
            salience: self.salience,
            valence: self.valence,
            presentation: parse_presentation(&self.presentation),
            share: self.share,
            attitude: self.attitude,
            paraphrase: self.paraphrase,
            absorbed: self.absorbed,
            created_at: self.created_at,
            last_accessed_at: self.last_accessed_at,
            indexed_at: self.indexed_at,
            index_version: self.index_version,
        }
    }
}

pub async fn save_event(pool: &SqlitePool, ev: &MemoryEvent) -> RamariaResult<i64> {
    let pres = ev.presentation.as_str();
    sqlx::query(
        "INSERT INTO memory_events (persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, created_at, last_accessed_at, indexed_at, index_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&ev.persona_uid).bind(&ev.title).bind(&ev.summary)
    .bind(&ev.keywords).bind(&ev.participants).bind(ev.start).bind(ev.end)
    .bind(ev.confidence).bind(ev.salience).bind(ev.valence).bind(pres)
    .bind(ev.share).bind(&ev.attitude).bind(&ev.paraphrase)
    .bind(ev.absorbed).bind(ev.created_at).bind(ev.last_accessed_at)
    .bind(ev.indexed_at).bind(ev.index_version)
    .execute(pool).await
    .map_err(|e| RamariaError::storage_with_source("保存事件失败", e))?;

    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("获取事件 id 失败", e))?;
    Ok(id)
}

pub async fn list_events_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    offset: i64,
    limit: i64,
) -> RamariaResult<Vec<MemoryEvent>> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE persona_uid = ? ORDER BY start DESC LIMIT ? OFFSET ?",
    )
    .bind(persona_uid)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询事件列表失败", e))?;
    Ok(rows.into_iter().map(|r| r.into_event()).collect())
}

pub async fn list_unabsorbed_events(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<MemoryEvent>> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE persona_uid = ? AND absorbed = 0 ORDER BY start ASC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询未吸收事件失败", e))?;
    Ok(rows.into_iter().map(|r| r.into_event()).collect())
}

// =========================================================
// EventRelation (from_id/to_id: i64)
// =========================================================

pub async fn save_relation(pool: &SqlitePool, rel: &EventRelation) -> RamariaResult<i64> {
    sqlx::query(
        "INSERT INTO event_relations (from_id, to_id, kind, weight, created_at) VALUES (?, ?, ?, ?, ?)"
    )
    .bind(rel.from_id).bind(rel.to_id).bind(rel.kind.as_str()).bind(rel.weight).bind(rel.created_at)
    .execute(pool).await
    .map_err(|e| RamariaError::storage_with_source("保存事件关系失败", e))?;

    let id = sqlx::query_scalar::<_, i64>("SELECT last_insert_rowid()")
        .fetch_one(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("获取关系 id 失败", e))?;
    Ok(id)
}

// =========================================================
// EventSource (event_id: i64, l1_id: Uuid)
// =========================================================

pub async fn save_source(
    pool: &SqlitePool,
    event_id: i64,
    l1_id: Uuid,
    weight: f64,
) -> RamariaResult<()> {
    sqlx::query(
        "INSERT INTO event_sources (event_id, l1_id, weight) VALUES (?, ?, ?)
         ON CONFLICT(event_id, l1_id) DO UPDATE SET weight = excluded.weight",
    )
    .bind(event_id)
    .bind(l1_id.to_string())
    .bind(weight)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存事件溯源失败", e))?;
    Ok(())
}
