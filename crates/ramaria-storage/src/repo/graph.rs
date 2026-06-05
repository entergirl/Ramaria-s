//! rust/crates/ramaria-storage/src/repo/graph.rs - 知识图谱节点和边 CRUD
//!
//! 设计特点:
//! - 节点按 label + node_type + layer 分层存储
//! - 边按 source/target 关联，带权重
//! - v1.0 仅做基础 CRUD，供关键词共现图谱使用

use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::Row;
use sqlx::SqlitePool;
use uuid::Uuid;

/// 图谱节点。
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: Uuid,
    pub label: String,
    pub node_type: String,
    pub layer: String,
    pub created_at: i64,
}

/// 图谱边。
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub id: Uuid,
    pub source_node_id: Uuid,
    pub target_node_id: Uuid,
    pub weight: f64,
    pub edge_type: String,
    pub created_at: i64,
}

// =========================================================
// 节点 CRUD
// =========================================================

/// 保存节点。
pub async fn save_node(pool: &SqlitePool, node: &GraphNode) -> RamariaResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO graph_nodes (id, label, node_type, layer, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(node.id.to_string())
    .bind(&node.label)
    .bind(&node.node_type)
    .bind(&node.layer)
    .bind(node.created_at)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存图谱节点失败", e))?;
    Ok(())
}

/// 按类型列出节点。
pub async fn list_nodes_by_type(
    pool: &SqlitePool,
    node_type: &str,
) -> RamariaResult<Vec<GraphNode>> {
    let rows = sqlx::query(
        "SELECT id, label, node_type, layer, created_at FROM graph_nodes WHERE node_type = ?",
    )
    .bind(node_type)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出图谱节点失败", e))?;

    rows.iter().map(row_to_node).collect()
}

/// 按 layer 列出节点。
pub async fn list_nodes_by_layer(pool: &SqlitePool, layer: &str) -> RamariaResult<Vec<GraphNode>> {
    let rows = sqlx::query(
        "SELECT id, label, node_type, layer, created_at FROM graph_nodes WHERE layer = ?",
    )
    .bind(layer)
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出图谱节点失败", e))?;

    rows.iter().map(row_to_node).collect()
}

// =========================================================
// 边 CRUD
// =========================================================

/// 保存边。
pub async fn save_edge(pool: &SqlitePool, edge: &GraphEdge) -> RamariaResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO graph_edges (id, source_node_id, target_node_id, weight, edge_type, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(edge.id.to_string())
    .bind(edge.source_node_id.to_string())
    .bind(edge.target_node_id.to_string())
    .bind(edge.weight)
    .bind(&edge.edge_type)
    .bind(edge.created_at)
    .execute(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("保存图谱边失败", e))?;
    Ok(())
}

/// 根据源节点查询所有出边。
pub async fn list_edges_from(
    pool: &SqlitePool,
    source_node_id: Uuid,
) -> RamariaResult<Vec<GraphEdge>> {
    let rows = sqlx::query(
        "SELECT id, source_node_id, target_node_id, weight, edge_type, created_at \
         FROM graph_edges WHERE source_node_id = ?",
    )
    .bind(source_node_id.to_string())
    .fetch_all(pool)
    .await
    .map_err(|e| RamariaError::storage_with_source("列出图谱边失败", e))?;

    rows.iter().map(row_to_edge).collect()
}

// =========================================================
// 行映射
// =========================================================

fn row_to_node(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<GraphNode> {
    let id_str: String = row.get("id");
    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("图谱节点 ID 格式非法", e))?;

    Ok(GraphNode {
        id,
        label: row.get("label"),
        node_type: row.get("node_type"),
        layer: row.get("layer"),
        created_at: row.get("created_at"),
    })
}

fn row_to_edge(row: &sqlx::sqlite::SqliteRow) -> RamariaResult<GraphEdge> {
    let id_str: String = row.get("id");
    let src_str: String = row.get("source_node_id");
    let tgt_str: String = row.get("target_node_id");

    let id = Uuid::parse_str(&id_str)
        .map_err(|e| RamariaError::storage_with_source("图谱边 ID 格式非法", e))?;
    let source_node_id = Uuid::parse_str(&src_str)
        .map_err(|e| RamariaError::storage_with_source("图谱边 source ID 格式非法", e))?;
    let target_node_id = Uuid::parse_str(&tgt_str)
        .map_err(|e| RamariaError::storage_with_source("图谱边 target ID 格式非法", e))?;

    Ok(GraphEdge {
        id,
        source_node_id,
        target_node_id,
        weight: row.get("weight"),
        edge_type: row.get("edge_type"),
        created_at: row.get("created_at"),
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_pool;

    #[tokio::test]
    async fn save_and_list_nodes() {
        let pool = test_pool().await.unwrap();

        let node = GraphNode {
            id: Uuid::new_v4(),
            label: "Rust".into(),
            node_type: "keyword".into(),
            layer: "l1".into(),
            created_at: ramaria_core::types::now_ms(),
        };
        save_node(&pool, &node).await.unwrap();

        let keywords = list_nodes_by_type(&pool, "keyword").await.unwrap();
        assert_eq!(keywords.len(), 1);
        assert_eq!(keywords[0].label, "Rust");
    }

    #[tokio::test]
    async fn save_and_list_edges() {
        let pool = test_pool().await.unwrap();
        let now = ramaria_core::types::now_ms();

        let n1 = Uuid::new_v4();
        let n2 = Uuid::new_v4();

        // 创建节点
        for (id, label) in [(n1, "Rust"), (n2, "编程")] {
            save_node(
                &pool,
                &GraphNode {
                    id,
                    label: label.into(),
                    node_type: "keyword".into(),
                    layer: "l1".into(),
                    created_at: now,
                },
            )
            .await
            .unwrap();
        }

        // 创建边
        let edge = GraphEdge {
            id: Uuid::new_v4(),
            source_node_id: n1,
            target_node_id: n2,
            weight: 0.8,
            edge_type: "cooccurrence".into(),
            created_at: now,
        };
        save_edge(&pool, &edge).await.unwrap();

        let edges = list_edges_from(&pool, n1).await.unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_node_id, n2);
        assert!((edges[0].weight - 0.8).abs() < f64::EPSILON);
    }
}
