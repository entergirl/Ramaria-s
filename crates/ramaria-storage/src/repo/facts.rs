//! crates/ramaria-storage/src/repo/facts.rs - PersonaFact CRUD
//!
//! 设计特点:
//! - 管理原子化人物事实（替代旧 user_profile 表）
//! - field 和 source 解析失败时回退到合理默认值并记录 WARNING
//! - ref_event_id 和 ref_l1_id 为独立可空列，避免一列指两张表

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_optional;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{FactSource, FactStatus, FactTier, PersonaFact, ProfileField};
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
parse_enum_fallback!(
    parse_fact_status, FactStatus, FactStatus::Active, "persona_facts", "status",
    "active"      => Active,
    "superseded"  => Superseded,
    "candidate"   => Candidate,
);
parse_enum_fallback!(
    parse_fact_tier, FactTier, FactTier::Stable, "persona_facts", "tier",
    "stable"      => Stable,
    "volatile"    => Volatile,
    "historical"  => Historical,
);

#[derive(sqlx::FromRow)]
struct FactRow {
    id: i64,
    persona_uid: String,
    field: String,
    content: String,
    source: String,
    status: String,
    tier: String,
    version_of: Option<i64>,
    confidence: f64,
    keyword_hint: Option<String>,
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
            status: parse_fact_status(&self.status),
            tier: parse_fact_tier(&self.tier),
            version_of: self.version_of,
            confidence: self.confidence,
            keyword_hint: self.keyword_hint,
            ref_event_id: self.ref_event_id,
            ref_l1_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// 所有 fact 列（查询投影统一复用）。
const FACT_COLUMNS: &str = "id, persona_uid, field, content, source, status, tier, \
     version_of, confidence, keyword_hint, ref_event_id, ref_l1_id, created_at, updated_at";

pub async fn save(pool: &SqlitePool, f: &PersonaFact) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO persona_facts (persona_uid, field, content, source, status, tier, \
             version_of, confidence, keyword_hint, ref_event_id, ref_l1_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&f.persona_uid)
    .bind(f.field.as_str())
    .bind(&f.content)
    .bind(f.source.as_str())
    .bind(f.status.as_str())
    .bind(f.tier.as_str())
    .bind(f.version_of)
    .bind(f.confidence)
    .bind(f.keyword_hint.as_ref())
    .bind(f.ref_event_id)
    .bind(f.ref_l1_id.map(|u| u.to_string()))
    .bind(f.created_at)
    .bind(f.updated_at)
    .fetch_one(pool)
    .await
    .storage_err("保存事实失败")
}

/// 按 persona 查询指定 field 的**全部**版本（历史 superseded 含内，供版本链展示）。
pub async fn list_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
    field: ProfileField,
) -> RamariaResult<Vec<PersonaFact>> {
    let rows = sqlx::query_as::<_, FactRow>(&format!(
        "SELECT {FACT_COLUMNS} FROM persona_facts WHERE persona_uid = ? AND field = ? \
             ORDER BY created_at DESC"
    ))
    .bind(persona_uid)
    .bind(field.as_str())
    .fetch_all(pool)
    .await
    .storage_err("查询事实列表失败")?;
    rows.into_iter().map(|r| r.into_fact()).collect()
}

/// 按 persona 查询**全部字段**的**active** 事实（检索/注入/画像展示用）。
///
/// 说明:
/// - 只取 status = 'active'（版本链中仅当前生效参与检索与注入）。
/// - 跨字段集合返回，供知识卡片分组与规则判定器检索。
pub async fn list_active_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<PersonaFact>> {
    let rows = sqlx::query_as::<_, FactRow>(&format!(
        "SELECT {FACT_COLUMNS} FROM persona_facts \
             WHERE persona_uid = ? AND status = 'active' \
             ORDER BY field, created_at DESC"
    ))
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询 active 事实失败")?;
    rows.into_iter().map(|r| r.into_fact()).collect()
}

/// 按 persona + field 查询 **active** 事实（同 field 召回，判重候选读取）。
pub async fn list_active_by_field(
    pool: &SqlitePool,
    persona_uid: &str,
    field: ProfileField,
) -> RamariaResult<Vec<PersonaFact>> {
    let rows = sqlx::query_as::<_, FactRow>(&format!(
        "SELECT {FACT_COLUMNS} FROM persona_facts \
             WHERE persona_uid = ? AND field = ? AND status = 'active' \
             ORDER BY created_at DESC"
    ))
    .bind(persona_uid)
    .bind(field.as_str())
    .fetch_all(pool)
    .await
    .storage_err("查询同 field active 事实失败")?;
    rows.into_iter().map(|r| r.into_fact()).collect()
}

/// 按 id 查询单条事实（CLI show / 版本链跳转）。
pub async fn get_by_id(pool: &SqlitePool, id: i64) -> RamariaResult<Option<PersonaFact>> {
    let row = sqlx::query_as::<_, FactRow>(&format!(
        "SELECT {FACT_COLUMNS} FROM persona_facts WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .storage_err("查询事实失败")?;
    row.map(FactRow::into_fact).transpose()
}

/// 按 persona 查询**全部**事实（含 superseded/candidate，供 CLI list 与 UI 版本链统计）。
pub async fn list_all_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<PersonaFact>> {
    let rows = sqlx::query_as::<_, FactRow>(&format!(
        "SELECT {FACT_COLUMNS} FROM persona_facts WHERE persona_uid = ? \
             ORDER BY field, created_at DESC"
    ))
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询全部事实失败")?;
    rows.into_iter().map(|r| r.into_fact()).collect()
}

/// 事务化版本链覆盖写入：旧事实置 superseded + 新事实写入（version_of 指向旧 id）。
///
/// 说明:
/// - 同一事务内完成"旧 superseded + 新 insert"，避免中间态（覆盖写原子化）。
/// - `old` 为被覆盖的 active 事实；`f` 为新事实（id 应为 0，由本函数回填）。
/// - 返回新事实 id。
pub async fn save_with_version(
    pool: &SqlitePool,
    old: &PersonaFact,
    f: &PersonaFact,
) -> RamariaResult<i64> {
    let mut tx = pool.begin().await.storage_err("开启事实覆盖事务失败")?;

    // 1. 旧事实置 superseded
    sqlx::query("UPDATE persona_facts SET status = 'superseded', updated_at = ? WHERE id = ?")
        .bind(f.updated_at)
        .bind(old.id)
        .execute(&mut *tx)
        .await
        .storage_err("覆盖旧事实失败")?;

    // 2. 新事实写入并回填 id
    let new_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO persona_facts (persona_uid, field, content, source, status, tier, \
             version_of, confidence, keyword_hint, ref_event_id, ref_l1_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&f.persona_uid)
    .bind(f.field.as_str())
    .bind(&f.content)
    .bind(f.source.as_str())
    // 新事实始终 active
    .bind(FactStatus::Active.as_str())
    .bind(f.tier.as_str())
    .bind(Some(old.id))
    .bind(f.confidence)
    .bind(f.keyword_hint.as_ref())
    .bind(f.ref_event_id)
    .bind(f.ref_l1_id.map(|u| u.to_string()))
    .bind(f.created_at)
    .bind(f.updated_at)
    .fetch_one(&mut *tx)
    .await
    .storage_err("写入新事实失败")?;

    tx.commit().await.storage_err("提交事实覆盖事务失败")?;
    Ok(new_id)
}

/// 升级 candidate → active（互证通过后提升）。
pub async fn promote_to_active(pool: &SqlitePool, id: i64) -> RamariaResult<()> {
    sqlx::query("UPDATE persona_facts SET status = 'active', updated_at = ? WHERE id = ?")
        .bind(ramaria_core::types::now_ms())
        .bind(id)
        .execute(pool)
        .await
        .storage_err("升级事实状态失败")?;
    Ok(())
}

/// 查询某事实的完整版本链（含自身，按 created_at 升序）。
///
/// 说明:
/// - 从指定事实出发，沿 version_of 链回溯到最早版本。
/// - 用于 CLI `fact show <id>` 与前端历史版本折叠展示。
pub async fn list_versions(pool: &SqlitePool, seed_id: i64) -> RamariaResult<Vec<PersonaFact>> {
    // 先查 seed 事实，确认存在
    let seed = get_by_id(pool, seed_id).await?;
    let Some(seed) = seed else {
        return Ok(Vec::new());
    };

    let mut chain = vec![seed];
    let mut current_id = chain[0].version_of;
    let mut guard = 0u32;
    // 沿 version_of 链回溯到最早版本（防御环，最多 64 跳）
    while let Some(pid) = current_id {
        if guard >= 64 {
            break;
        }
        guard += 1;
        match get_by_id(pool, pid).await? {
            Some(fact) => {
                current_id = fact.version_of;
                chain.push(fact);
            }
            None => break,
        }
    }
    // 链头（最早版本）在前，当前版本在后
    chain.reverse();
    Ok(chain)
}

/// 将单条事实置 superseded（独立覆盖写；不开启事务，供上层仲裁原子化调用）。
pub async fn supersede(pool: &SqlitePool, id: i64, at: i64) -> RamariaResult<()> {
    sqlx::query("UPDATE persona_facts SET status = 'superseded', updated_at = ? WHERE id = ?")
        .bind(at)
        .bind(id)
        .execute(pool)
        .await
        .storage_err("覆盖事实失败")?;
    Ok(())
}

/// 按 persona_uid 一次性统计所有 ProfileField 的 fact 数量。
///
/// 使用单条 GROUP BY 查询替代 N+1 循环。
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
