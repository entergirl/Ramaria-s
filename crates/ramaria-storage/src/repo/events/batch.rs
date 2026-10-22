//! crates/ramaria-storage/src/repo/events/batch.rs - 事件批次单事务写入
//!
//! 设计特点:
//! - 事件、来源、关系、L1 吸收标记在同一事务内写入，失败整体回滚
//! - 来源/关系下标指向批次事件数组位置，越界返回 Validation
//! - 自引用关系（落库 id 相同）直接跳过
//! - L1 吸收标记在事务内直接 UPDATE（不嵌套自有事务的 mark_absorbed）

use crate::repo::StorageResultExt;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::EventBatchWrite;
use sqlx::SqlitePool;

// =========================================================
// 事件批次单事务写入
// =========================================================

/// 单事务写入事件批次（事件 + 来源 + 关系 + L1 吸收标记）。
///
/// 事务语义:
/// - 事件 INSERT、来源/关系 INSERT、L1 吸收 UPDATE 全部在同一事务内执行，
///   任一步失败整体回滚，杜绝"事件半写入 / 证据链缺失"；
///   调用方失败重试不会产生半批数据。
///
/// 下标约定:
/// - `batch.sources` / `batch.relations` 中的下标均指向 `batch.events` 数组位置
///   （事件 id 由事务内自增分配，写入前未知）；
/// - 越界下标返回 `Validation` 错误（仅含下标与批次长度，不含事件文本）；
/// - 自引用关系（落库 id 相同）直接跳过，与提取侧"丢弃自引用"口径一致。
///
/// L1 吸收标记:
/// - 不调用 `memory_l1::mark_absorbed`（其内部自建事务，嵌套会失败），
///   改为在本事务内分批 UPDATE，命名占位符避免 SQL 参数超限。
///
/// 返回:
/// - 按 `batch.events` 顺序一一对应的事件 id；
/// - 空事件批次返回空列表（此时仍执行 `absorbed_l1_ids` 的 UPDATE）。
pub async fn save_event_batch(
    pool: &SqlitePool,
    batch: &EventBatchWrite,
) -> RamariaResult<Vec<i64>> {
    let mut tx = pool.begin().await.storage_err("开启事件批次写入事务失败")?;

    // 事件主表：逐条 INSERT，返回的自增 id 供来源/关系按下标解析
    let mut event_ids: Vec<i64> = Vec::with_capacity(batch.events.len());
    for ev in &batch.events {
        let pres = ev.presentation.as_str();
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO memory_events (persona_uid, title, summary, keywords, participants, start, \"end\",
             confidence, salience, valence, presentation, share, attitude, paraphrase,
             absorbed, situation_strength, motives, created_at, last_accessed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&ev.persona_uid).bind(&ev.title).bind(&ev.summary)
        .bind(&ev.keywords).bind(&ev.participants).bind(ev.start).bind(ev.end)
        .bind(ev.confidence).bind(ev.salience).bind(ev.valence).bind(pres)
        .bind(ev.share).bind(&ev.attitude).bind(&ev.paraphrase)
        .bind(ev.absorbed).bind(ev.situation_strength.map(|v| v as i64))
        .bind(&ev.motives)
        .bind(ev.created_at).bind(ev.last_accessed_at)
        .fetch_one(&mut *tx)
        .await
        .storage_err("批次写入事件失败")?;
        event_ids.push(id);
    }

    // 事件来源：越界下标返回 Validation（整体回滚）
    for (event_index, l1_id, weight) in &batch.sources {
        let event_id = *event_ids.get(*event_index).ok_or_else(|| {
            RamariaError::validation(format!(
                "事件来源下标越界: {event_index}（批次事件数 {}）",
                event_ids.len()
            ))
        })?;
        sqlx::query(
            "INSERT INTO event_sources (event_id, l1_id, weight) VALUES (?, ?, ?)
             ON CONFLICT(event_id, l1_id) DO UPDATE SET weight = excluded.weight",
        )
        .bind(event_id)
        .bind(l1_id.to_string())
        .bind(weight)
        .execute(&mut *tx)
        .await
        .storage_err("批次写入事件来源失败")?;
    }

    // 事件关系：越界下标返回 Validation；自引用跳过
    let created_at = ramaria_core::types::now_ms();
    for (from_index, to_index, kind, weight) in &batch.relations {
        let from_id = *event_ids.get(*from_index).ok_or_else(|| {
            RamariaError::validation(format!(
                "事件关系 from 下标越界: {from_index}（批次事件数 {}）",
                event_ids.len()
            ))
        })?;
        let to_id = *event_ids.get(*to_index).ok_or_else(|| {
            RamariaError::validation(format!(
                "事件关系 to 下标越界: {to_index}（批次事件数 {}）",
                event_ids.len()
            ))
        })?;
        if from_id == to_id {
            continue;
        }
        sqlx::query(
            "INSERT INTO event_relations (from_id, to_id, kind, weight, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(from_id)
        .bind(to_id)
        .bind(kind.as_str())
        .bind(weight)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .storage_err("批次写入事件关系失败")?;
    }

    // L1 吸收标记：同一事务内直接 UPDATE（分批 + 命名占位符）
    if !batch.absorbed_l1_ids.is_empty() {
        const BATCH_SIZE: usize = 100;
        for chunk in batch.absorbed_l1_ids.chunks(BATCH_SIZE) {
            let placeholders: Vec<String> =
                (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
            let sql = format!(
                "UPDATE memory_l1 SET absorbed = 1 WHERE id IN ({})",
                placeholders.join(", ")
            );
            let mut query = sqlx::query(&sql);
            for id in chunk {
                query = query.bind(id.to_string());
            }
            query
                .execute(&mut *tx)
                .await
                .storage_err(format!("批次标记 {} 条 L1 已吸收失败", chunk.len()))?;
        }
    }

    tx.commit().await.storage_err("提交事件批次写入事务失败")?;

    tracing::info!(
        event_count = event_ids.len(),
        source_count = batch.sources.len(),
        relation_count = batch.relations.len(),
        absorbed_l1_count = batch.absorbed_l1_ids.len(),
        "事件批次单事务写入完成"
    );

    Ok(event_ids)
}
