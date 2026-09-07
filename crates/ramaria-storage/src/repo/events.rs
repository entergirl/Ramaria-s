//! crates/ramaria-storage/src/repo/events.rs - MemoryEvent / EventRelation / EventSource CRUD
//!
//! 设计特点:
//! - 管理 L2 事件主表及其关系和溯源
//! - MemoryEvent 使用 AUTOINCREMENT id；EventRelation/EventSource 同理
//! - presentation 解析失败时回退为 Mixed 并记录 WARNING
//! - event_sources 使用 ON CONFLICT 幂等写入（同一 (event_id, l1_id) 不重复）

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{
    EventRelation, EventSource, MemoryEvent, PersonaEventAggregate, Presentation,
};
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

/// 按 id 查询单条事件（证据链溯源用）。
///
/// 返回:
/// - `Ok(Some(event))`: 命中。
/// - `Ok(None)`: 未命中（不视为错误）。
pub async fn get(pool: &SqlitePool, id: i64) -> RamariaResult<Option<MemoryEvent>> {
    let row = sqlx::query_as::<_, EventRow>(
        "SELECT id, persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, situation_strength, motives, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .storage_err("查询事件失败")?;
    Ok(row.map(|r| r.into_event()))
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

/// 查询 persona 最近事件（按 created_at 倒序），供新事件相似度去重比对。
///
/// 参数:
/// - `limit`: 最多返回条数。
pub async fn list_recent_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    limit: u32,
) -> RamariaResult<Vec<MemoryEvent>> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, situation_strength, motives, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE persona_uid = ? ORDER BY created_at DESC LIMIT ?",
    )
    .bind(persona_uid)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .storage_err("查询最近事件失败")?;
    Ok(rows.into_iter().map(|r| r.into_event()).collect())
}

/// 标记事件已被 L3 推断吸收。
///
/// 将 `absorbed` 设为 1，使这些事件不再出现在 `list_unabsorbed_events` 中。
/// 使用批量 UPDATE 以支持大批量事件。
///
/// 事务语义（决策 D-V17-014-23）:
/// - 与 L1 版 `memory_l1::mark_absorbed` 对齐为事务化执行，杜绝"事件半吸收"：
///   任一批次失败时整体回滚，已吸收标记不会部分落库。
/// - 批次内使用命名占位符（`?1, ?2, ...`），避免 SQL 过长（SQLite 默认参数限制 999 个）。
pub async fn mark_absorbed(pool: &SqlitePool, event_ids: &[i64]) -> RamariaResult<()> {
    if event_ids.is_empty() {
        return Ok(());
    }

    // 分批处理：每批最多 100 个 ID，避免 SQL 过长（与 memory_l1::mark_absorbed 对齐）
    const BATCH_SIZE: usize = 100;

    // 事务包裹：全部成功或全部回滚，杜绝事件半吸收
    let mut tx = pool.begin().await.storage_err("开启事件吸收标记事务失败")?;

    for chunk in event_ids.chunks(BATCH_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "UPDATE memory_events SET absorbed = 1 WHERE id IN ({})",
            placeholders.join(", ")
        );

        let mut query = sqlx::query(&sql);
        for id in chunk {
            query = query.bind(*id);
        }
        query
            .execute(&mut *tx)
            .await
            .storage_err(format!("标记 {} 条事件已吸收失败", chunk.len()))?;
    }

    tx.commit().await.storage_err("提交事件吸收标记事务失败")?;

    tracing::info!(
        total = event_ids.len(),
        batches = event_ids.len().div_ceil(BATCH_SIZE),
        "批量标记事件已吸收完成"
    );

    Ok(())
}

// =========================================================
// 跨用户事件级经验聚合（L3 冷启动先验数据源）
// =========================================================

/// SQL 聚合行的私有中间结构（避免 PersonaEventAggregate 依赖 sqlx）。
#[derive(sqlx::FromRow)]
struct PersonaEventAggregateRow {
    persona_uid: String,
    n_events: i64,
    valence_mean: f64,
    share_mean: f64,
    obj_ratio: f64,
    sub_ratio: f64,
    mix_ratio: f64,
}

impl PersonaEventAggregateRow {
    fn into_aggregate(self) -> PersonaEventAggregate {
        PersonaEventAggregate::new(
            self.persona_uid,
            self.n_events.max(0) as u64,
            self.valence_mean,
            self.share_mean,
            self.obj_ratio,
            self.sub_ratio,
            self.mix_ratio,
        )
    }
}

/// 聚合除目标 persona 外各 persona 的事件级经验分布（跨用户冷启动先验数据源）。
///
/// SQL 口径:
/// - `n_events = COUNT(*)`：该 persona 的事件原始条数。
/// - `valence_mean / share_mean = AVG(...)`：事件级简单均值（不含 salience 加权，
///   与分类内 `CategoryStats` 口径不同，仅作跨 persona 经验方向锚点）。
/// - presentation 三态占比 = 各态计数 / 总计数，和恒为 1
///   （`memory_events.presentation` 存储为小写字符串 objective/subjective/mixed）。
///
/// 过滤语义:
/// - 排除目标 persona（`exclude_persona_uid`）自身事件，避免自身样本污染"跨用户"先验。
/// - 仅返回至少含 1 条事件的 persona 行；无事件的行无经验意义，直接剔除。
///
/// 说明:
/// - 本函数只做原始 SQL 聚合；样本量阈值判定与加权合并由 ramaria-memory 负责。
///
/// 返回:
/// - 按 persona_uid 升序的各已有人格画像聚合行；空库 / 无其他 persona 时返回空列表。
pub async fn aggregate_persona_event_priors(
    pool: &SqlitePool,
    exclude_persona_uid: &str,
) -> RamariaResult<Vec<PersonaEventAggregate>> {
    let rows = sqlx::query_as::<_, PersonaEventAggregateRow>(
        "SELECT persona_uid,
                COUNT(*) AS n_events,
                AVG(valence) AS valence_mean,
                AVG(share) AS share_mean,
                SUM(CASE WHEN presentation = 'objective' THEN 1.0 ELSE 0.0 END)
                    / CAST(COUNT(*) AS REAL) AS obj_ratio,
                SUM(CASE WHEN presentation = 'subjective' THEN 1.0 ELSE 0.0 END)
                    / CAST(COUNT(*) AS REAL) AS sub_ratio,
                SUM(CASE WHEN presentation = 'mixed' THEN 1.0 ELSE 0.0 END)
                    / CAST(COUNT(*) AS REAL) AS mix_ratio
         FROM memory_events
         WHERE persona_uid <> ?
         GROUP BY persona_uid
         HAVING COUNT(*) > 0
         ORDER BY persona_uid ASC",
    )
    .bind(exclude_persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("聚合跨用户事件经验分布失败")?;
    Ok(rows.into_iter().map(|r| r.into_aggregate()).collect())
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
