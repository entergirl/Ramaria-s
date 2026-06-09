//! rust/crates/ramaria-storage/src/repo/graph.rs - GraphNodes / GraphEdges CRUD
//!
//! 设计特点:
//! - 管理知识图谱实体节点和关系边
//! - insert_node 使用 INSERT OR IGNORE 幂等创建
//! - insert_edge 使用 AUTOINCREMENT 主键

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;
use uuid::Uuid;

use super::last_insert_id;

pub async fn insert_node(
    pool: &SqlitePool,
    entity_name: &str,
    entity_type: &str,
    source_l1_id: Option<Uuid>,
) -> RamariaResult<i64> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("INSERT OR IGNORE INTO graph_nodes (entity_name, entity_type, source_l1_id, created_at) VALUES (?, ?, ?, ?)")
        .bind(entity_name).bind(entity_type)
        .bind(source_l1_id.map(|u| u.to_string())).bind(now)
        .execute(pool).await
        .map_err(|e| RamariaError::storage_with_source("插入图谱节点失败", e))?;

    let id = sqlx::query_scalar::<_, i64>("SELECT id FROM graph_nodes WHERE entity_name = ?")
        .bind(entity_name)
        .fetch_one(pool)
        .await
        .map_err(|e| RamariaError::storage_with_source("查询图谱节点 id 失败", e))?;
    Ok(id)
}

pub async fn get_node(
    pool: &SqlitePool,
    entity_name: &str,
) -> RamariaResult<Option<(i64, String, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        entity_name: String,
        entity_type: String,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT id, entity_name, entity_type FROM graph_nodes WHERE entity_name = ?",
    )
    .bind(entity_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("查询图谱节点失败", e))?;
    Ok(row.map(|r| (r.id, r.entity_name, r.entity_type)))
}

pub async fn insert_edge(
    pool: &SqlitePool,
    source_id: i64,
    target_id: i64,
    relation_type: &str,
    detail: Option<&str>,
    source_l1_id: Option<Uuid>,
) -> RamariaResult<i64> {
    let now = ramaria_core::types::now_ms();
    sqlx::query(
        "INSERT INTO graph_edges (source_node_id, target_node_id, relation_type, relation_detail, source_l1_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?)"
    ).bind(source_id).bind(target_id).bind(relation_type)
      .bind(detail).bind(source_l1_id.map(|u| u.to_string())).bind(now)
      .execute(pool).await
    .map_err(|e| RamariaError::storage_with_source("插入图谱边失败", e))?;

    last_insert_id(pool).await
}

pub async fn list_edges(
    pool: &SqlitePool,
    source_id: i64,
) -> RamariaResult<Vec<(i64, i64, i64, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        source_node_id: i64,
        target_node_id: i64,
        relation_type: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, source_node_id, target_node_id, relation_type FROM graph_edges WHERE source_node_id = ?"
    ).bind(source_id).fetch_all(pool).await
        .map_err(|e| RamariaError::storage_with_source("查询图谱边失败", e))?;
    Ok(rows
        .into_iter()
        .map(|r| (r.id, r.source_node_id, r.target_node_id, r.relation_type))
        .collect())
}
