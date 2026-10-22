//! crates/ramaria-storage/src/repo/events/trace.rs - 事件溯源与来源会话映射
//!
//! 设计特点:
//! - event_sources 以 (event_id, l1_id) 唯一，重复写入只更新权重
//! - 溯源行的 l1_id 非 UUID 时返回存储错误（不静默丢弃）
//! - 批量会话映射取权重最高、l1_id 字典序最小的确定性来源
//! - IN 查询按 500 分片，规避 SQLite 变量上限

use std::collections::HashMap;

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::EventSource;
use sqlx::SqlitePool;
use uuid::Uuid;

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

/// 批量查询事件所属会话映射（`event_sources → memory_l1.session_id`）。
///
/// 口径:
/// - 一个事件可能有多条溯源 L1（跨会话）：取来源权重最高者的会话；
///   同权重时取 `l1_id` 字典序最小的一条（确定性，脏数据下结果稳定）。
/// - 事件无来源、或来源会话 UUID 解析失败时，该事件不出现在映射中（记 warn）。
///
/// 参数:
/// - `event_ids`: 目标事件 id 列表（空列表直接返回空映射）。
///
/// 返回:
/// - event_id → session_id 映射。
///
/// 说明:
/// - `IN` 查询按 500 分片（SQLite 默认变量上限 999，留出余量），
///   避免大批量事件时超限报错。
pub async fn list_session_map(
    pool: &SqlitePool,
    event_ids: &[i64],
) -> RamariaResult<HashMap<i64, Uuid>> {
    if event_ids.is_empty() {
        return Ok(HashMap::new());
    }

    #[derive(sqlx::FromRow)]
    struct EventSessionRow {
        event_id: i64,
        session_id: String,
    }

    const CHUNK_SIZE: usize = 500;

    let mut map: HashMap<i64, Uuid> = HashMap::with_capacity(event_ids.len());
    for chunk in event_ids.chunks(CHUNK_SIZE) {
        // 命名占位符与绑定顺序解耦；排序保证同一事件取"权重最高、l1_id 最小"的行
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT es.event_id AS event_id, l.session_id AS session_id \
             FROM event_sources es \
             JOIN memory_l1 l ON l.id = es.l1_id \
             WHERE es.event_id IN ({}) \
             ORDER BY es.event_id ASC, es.weight DESC, es.l1_id ASC",
            placeholders.join(", ")
        );
        let mut query = sqlx::query_as::<_, EventSessionRow>(&sql);
        for id in chunk {
            query = query.bind(*id);
        }
        let rows = query
            .fetch_all(pool)
            .await
            .storage_err("批量查询事件所属会话失败")?;

        for row in rows {
            // 同一事件的首行即"权重最高 / l1_id 最小"的确定性选择
            if map.contains_key(&row.event_id) {
                continue;
            }
            match ramaria_core::types::uuid_from_db(&row.session_id) {
                Ok(session_id) => {
                    map.insert(row.event_id, session_id);
                }
                Err(_) => {
                    tracing::warn!(
                        event_id = row.event_id,
                        "event_sources 所属会话 UUID 解析失败，已跳过该事件"
                    );
                }
            }
        }
    }

    Ok(map)
}
