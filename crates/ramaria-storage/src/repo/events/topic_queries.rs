//! crates/ramaria-storage/src/repo/events/topic_queries.rs - 主动对话选题事件查询
//!
//! 设计特点:
//! - 承载主动对话选题的"高显著事件"与"时间窗事件"两条查询
//! - 时间窗统一以事件结束时间 `end` 为比较键（闭区间下界）
//! - 排序到 `id` 级保证确定性（同一秒事件在多次查询间顺序稳定）
//! - 仅做参数绑定查询，不含选题业务语义（门槛与窗口由调用方配置提供）

use super::*;

/// 查询高显著事件（显著性门槛 + 事件结束时间窗）。
///
/// 口径:
/// - `end >= since_ms` 且 `salience >= min_salience`（均为闭区间）；
/// - 按 `salience` 降序、同分按 `end` 降序、再按 `id` 降序（确定性）；
/// - 取前 `limit` 条。
///
/// 参数:
/// - `since_ms`: 时间窗下界（Unix 毫秒，比较事件 `end`）。
/// - `min_salience`: 显著性门槛（闭区间下界）。
/// - `limit`: 最多返回条数。
pub async fn list_events_by_salience(
    pool: &SqlitePool,
    persona_uid: &str,
    since_ms: i64,
    min_salience: f64,
    limit: u32,
) -> RamariaResult<Vec<MemoryEvent>> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, situation_strength, motives, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE persona_uid = ? AND \"end\" >= ? AND salience >= ?
         ORDER BY salience DESC, \"end\" DESC, id DESC LIMIT ?",
    )
    .bind(persona_uid)
    .bind(since_ms)
    .bind(min_salience)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .storage_err("查询高显著事件失败")?;
    Ok(rows.into_iter().map(|r| r.into_event()).collect())
}

/// 查询时间窗内的近期事件（事件结束时间倒序）。
///
/// 口径:
/// - `end >= since_ms`（闭区间）；
/// - 按 `end` 降序、同分按 `id` 降序（确定性）；
/// - 取前 `limit` 条。
///
/// 参数:
/// - `since_ms`: 时间窗下界（Unix 毫秒，比较事件 `end`）。
/// - `limit`: 最多返回条数。
pub async fn list_events_since(
    pool: &SqlitePool,
    persona_uid: &str,
    since_ms: i64,
    limit: u32,
) -> RamariaResult<Vec<MemoryEvent>> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, persona_uid, title, summary, keywords, participants, start, \"end\",
         confidence, salience, valence, presentation, share, attitude, paraphrase,
         absorbed, situation_strength, motives, created_at, last_accessed_at, indexed_at, index_version
         FROM memory_events WHERE persona_uid = ? AND \"end\" >= ?
         ORDER BY \"end\" DESC, id DESC LIMIT ?",
    )
    .bind(persona_uid)
    .bind(since_ms)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .storage_err("查询时间窗内事件失败")?;
    Ok(rows.into_iter().map(|r| r.into_event()).collect())
}
