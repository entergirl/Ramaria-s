//! crates/ramaria-storage/src/repo/events/relations.rs - 事件关系存取
//!
//! 设计特点:
//! - 关系两端均为 memory_events 的 INTEGER 主键
//! - 关系类型以文本落库，读取时按变体解析、未知类型降级 RelatedTo
//! - 按 persona 查询时 JOIN 事件主表，保证关系至少一端属于目标人格

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::EventRelation;
use sqlx::SqlitePool;

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
