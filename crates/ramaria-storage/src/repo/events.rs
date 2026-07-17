//! rust/crates/ramaria-storage/src/repo/events.rs - MemoryEvent / EventRelation / EventSource CRUD
//!
//! 设计特点:
//! - 管理 L2 事件主表及其关系和溯源
//! - MemoryEvent 使用 AUTOINCREMENT id；EventRelation/EventSource 同理
//! - presentation 解析失败时回退为 Mixed 并记录 WARNING
//! - event_sources 使用 ON CONFLICT 幂等写入（同一 (event_id, l1_id) 不重复）

use crate::parse_enum_fallback;
use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{EventRelation, EventSource, MemoryEvent, Presentation};
use sqlx::SqlitePool;
use uuid::Uuid;

// =========================================================
// MemoryEvent（事件主表）
// =========================================================

parse_enum_fallback!(
    parse_presentation, Presentation, Presentation::Mixed, "memory_events", "presentation",
    "objective"  => Objective,
    "subjective" => Subjective,
    "mixed"      => Mixed,
);

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
    situation_strength: Option<i64>,
    motives: Option<String>,
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
            situation_strength: self.situation_strength.map(|v| v as i32),
            motives: self.motives,
            created_at: self.created_at,
            last_accessed_at: self.last_accessed_at,
            indexed_at: self.indexed_at,
            index_version: self.index_version,
        }
    }
}

pub async fn save_event(pool: &SqlitePool, ev: &MemoryEvent) -> RamariaResult<i64> {
    let pres = ev.presentation.as_str();
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO memory_events (persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, situation_strength, motives, created_at, last_accessed_at, indexed_at, index_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&ev.persona_uid).bind(&ev.title).bind(&ev.summary)
    .bind(&ev.keywords).bind(&ev.participants).bind(ev.start).bind(ev.end)
    .bind(ev.confidence).bind(ev.salience).bind(ev.valence).bind(pres)
    .bind(ev.share).bind(&ev.attitude).bind(&ev.paraphrase)
    .bind(ev.absorbed).bind(ev.situation_strength.map(|v| v as i64))
    .bind(&ev.motives)
    .bind(ev.created_at).bind(ev.last_accessed_at)
    .bind(ev.indexed_at).bind(ev.index_version)
    .fetch_one(pool).await
    .storage_err("保存事件失败")
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
         absorbed, situation_strength, motives, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE persona_uid = ? ORDER BY start DESC LIMIT ? OFFSET ?",
    )
    .bind(persona_uid)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .storage_err("查询事件列表失败")?;
    Ok(rows.into_iter().map(|r| r.into_event()).collect())
}

pub async fn list_unabsorbed_events(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<MemoryEvent>> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, situation_strength, motives, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE persona_uid = ? AND absorbed = 0 ORDER BY start ASC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询未吸收事件失败")?;
    Ok(rows.into_iter().map(|r| r.into_event()).collect())
}

/// 标记事件已被 L3 推断吸收。
///
/// 将 `absorbed` 设为 1，使这些事件不再出现在 `list_unabsorbed_events` 中。
/// 使用批量 UPDATE 以支持大批量事件。
pub async fn mark_absorbed(pool: &SqlitePool, event_ids: &[i64]) -> RamariaResult<()> {
    if event_ids.is_empty() {
        return Ok(());
    }

    // 分批处理，每批最多 100 个 ID，避免 SQL 过长
    for chunk in event_ids.chunks(100) {
        let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "UPDATE memory_events SET absorbed = 1 WHERE id IN ({})",
            placeholders.join(",")
        );

        let mut query = sqlx::query(&sql);
        for id in chunk {
            query = query.bind(*id);
        }
        query
            .execute(pool)
            .await
            .storage_err("标记事件已吸收失败")?;
    }

    Ok(())
}

// =========================================================
// 事件关系（from_id/to_id 均为 i64）
// =========================================================

pub async fn save_relation(pool: &SqlitePool, rel: &EventRelation) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO event_relations (from_id, to_id, kind, weight, created_at) VALUES (?, ?, ?, ?, ?) RETURNING id"
    )
    .bind(rel.from_id).bind(rel.to_id).bind(rel.kind.as_str()).bind(rel.weight).bind(rel.created_at)
    .fetch_one(pool).await
    .storage_err("保存事件关系失败")
}

// =========================================================
// 事件关系查询
// =========================================================

/// 事件关系查询行。
#[derive(sqlx::FromRow)]
struct RelationRow {
    id: i64,
    from_id: i64,
    to_id: i64,
    kind: String,
    weight: f64,
    created_at: i64,
}

impl RelationRow {
    fn into_relation(self) -> EventRelation {
        use ramaria_core::types::EventRelationKind;
        let kind = match self.kind.as_str() {
            "CausedBy" => EventRelationKind::CausedBy,
            "PartOf" => EventRelationKind::PartOf,
            "RelatedTo" => EventRelationKind::RelatedTo,
            "ContinuedBy" => EventRelationKind::ContinuedBy,
            "Contradicts" => EventRelationKind::Contradicts,
            "Timeline" => EventRelationKind::Timeline,
            _ => EventRelationKind::RelatedTo, // 未知关系类型降级
        };
        EventRelation {
            id: self.id,
            from_id: self.from_id,
            to_id: self.to_id,
            kind,
            weight: self.weight,
            created_at: self.created_at,
        }
    }
}

/// 按 persona_uid 查询该角色相关的所有事件关系。
///
/// 通过 JOIN memory_events 过滤：仅返回 from_id 对应事件属于目标 persona 的关系。
/// 这样保证每条关系至少有一个端点属于该角色的事件。
pub async fn list_relations_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<EventRelation>> {
    let rows = sqlx::query_as::<_, RelationRow>(
        "SELECT er.id, er.from_id, er.to_id, er.kind, er.weight, er.created_at
         FROM event_relations er
         JOIN memory_events me ON er.from_id = me.id
         WHERE me.persona_uid = ?
         ORDER BY er.created_at ASC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询事件关系列表失败")?;

    Ok(rows.into_iter().map(|r| r.into_relation()).collect())
}

// =========================================================
// 事件溯源（event_id 为 i64，l1_id 为 Uuid）
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
    .storage_err("保存事件溯源失败")?;
    Ok(())
}

/// 事件溯源行映射（用于 sqlx::FromRow 自动反序列化）。
#[derive(sqlx::FromRow)]
struct SourceRow {
    id: i64,
    event_id: i64,
    l1_id: String,
    weight: f64,
}

impl SourceRow {
    fn into_source(self) -> RamariaResult<EventSource> {
        let l1_id = Uuid::parse_str(&self.l1_id).map_err(|e| {
            ramaria_core::RamariaError::storage(format!(
                "event_sources 中 l1_id 不是有效 UUID: {e}"
            ))
        })?;
        Ok(EventSource {
            id: self.id,
            event_id: self.event_id,
            l1_id,
            weight: self.weight,
        })
    }
}

/// 查询指定事件的所有溯源 L1 记录。
///
/// 用于前端性格画像证据链展开：事件 → L1 摘要 → evidence_notes。
pub async fn list_sources_by_event(
    pool: &SqlitePool,
    event_id: i64,
) -> RamariaResult<Vec<EventSource>> {
    let rows = sqlx::query_as::<_, SourceRow>(
        "SELECT id, event_id, l1_id, weight FROM event_sources WHERE event_id = ? ORDER BY weight DESC",
    )
    .bind(event_id)
    .fetch_all(pool)
    .await
    .storage_err("查询事件溯源列表失败")?;

    let mut sources = Vec::with_capacity(rows.len());
    for row in rows {
        sources.push(row.into_source()?);
    }
    Ok(sources)
}
