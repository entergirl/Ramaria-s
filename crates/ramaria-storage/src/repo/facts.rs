//! rust/crates/ramaria-storage/src/repo/facts.rs - PersonaFact CRUD
//!
//! 设计特点:
//! - 管理原子化人物事实（替代旧 user_profile 表）
//! - field 和 source 解析失败时回退到合理默认值并记录 WARNING
//! - ref_event_id 和 ref_l1_id 为独立可空列，避免一列指两张表

use crate::parse_enum_fallback;
use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_optional;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{FactSource, PersonaFact, ProfileField};
use sqlx::SqlitePool;

parse_enum_fallback!(
    parse_field, ProfileField, ProfileField::SpeakingStyle, "persona_facts", "field",
    "basic_info"      => BasicInfo,
    "personal_status" => PersonalStatus,
    "interests"       => Interests,
    "social"          => Social,
    "history"         => History,
    "recent_context"  => RecentContext,
    "speaking_style"  => SpeakingStyle,
);
parse_enum_fallback!(
    parse_fact_source, FactSource, FactSource::L1, "persona_facts", "source",
    "event"  => Event,
    "manual" => Manual,
    "l1"     => L1,
);

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
    fn into_fact(self) -> RamariaResult<PersonaFact> {
        let ref_l1_id = parse_uuid_optional(&self.ref_l1_id, "persona_facts", "ref_l1_id")?;
        Ok(PersonaFact {
            id: self.id,
            persona_uid: self.persona_uid,
            field: parse_field(&self.field),
            content: self.content,
            source: parse_fact_source(&self.source),
            ref_event_id: self.ref_event_id,
            ref_l1_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

pub async fn save(pool: &SqlitePool, f: &PersonaFact) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO persona_facts (persona_uid, field, content, source, ref_event_id, ref_l1_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&f.persona_uid).bind(f.field.as_str()).bind(&f.content)
    .bind(f.source.as_str()).bind(f.ref_event_id).bind(f.ref_l1_id.map(|u| u.to_string()))
    .bind(f.created_at).bind(f.updated_at)
    .fetch_one(pool).await
    .storage_err("保存事实失败")
}

pub async fn list_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    field: ProfileField,
) -> RamariaResult<Vec<PersonaFact>> {
    let rows = sqlx::query_as::<_, FactRow>(
        "SELECT id, persona_uid, field, content, source, ref_event_id, ref_l1_id, created_at, updated_at
         FROM persona_facts WHERE persona_uid = ? AND field = ? ORDER BY created_at DESC"
    ).bind(persona_uid).bind(field.as_str())
        .fetch_all(pool).await
        .storage_err("查询事实列表失败")?;
    rows.into_iter()
        .map(|r| r.into_fact())
        .collect::<RamariaResult<Vec<_>>>()
}

/// 按 persona_uid 一次性统计所有 ProfileField 的 fact 数量。
///
/// 使用单条 GROUP BY 查询替代 N+1 循环（v1.3 P-4 修复）。
///
/// 返回:
/// - `Vec<(ProfileField, usize)>`：每个字段的 fact 数量和。
/// - 某个字段无记录时返回 `(field, 0)`。
pub async fn count_by_persona_grouped(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<(ProfileField, usize)>> {
    use crate::repo::StorageResultExt;

    #[derive(sqlx::FromRow)]
    struct FieldCount {
        field: String,
        cnt: i64,
    }

    let rows = sqlx::query_as::<_, FieldCount>(
        "SELECT field, COUNT(*) AS cnt FROM persona_facts
         WHERE persona_uid = ?
         GROUP BY field",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("统计事实数量失败")?;

    // 构建所有字段的计数映射
    let count_map: std::collections::HashMap<String, usize> = rows
        .into_iter()
        .map(|r| (r.field, r.cnt as usize))
        .collect();

    // 返回全部 7 个 ProfileField 的结果（缺失字段为 0）
    let fields = [
        ("basic_info", ProfileField::BasicInfo),
        ("personal_status", ProfileField::PersonalStatus),
        ("interests", ProfileField::Interests),
        ("social", ProfileField::Social),
        ("history", ProfileField::History),
        ("recent_context", ProfileField::RecentContext),
        ("speaking_style", ProfileField::SpeakingStyle),
    ];

    let result = fields
        .iter()
        .map(|(key, field)| (*field, count_map.get(*key).copied().unwrap_or(0)))
        .collect();

    Ok(result)
}
