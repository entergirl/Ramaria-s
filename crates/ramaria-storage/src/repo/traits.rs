//! rust/crates/ramaria-storage/src/repo/traits.rs - PersonalityTrait / TraitEvidence CRUD
//!
//! 设计特点:
//! - 管理 L3 性格标签（三层模型：base/primary/accent）及其证据链
//! - 所有枚举解析失败时回退到合理默认值并记录 WARNING
//! - list_traits_by_persona 仅返回 active 状态的标签（System Prompt 构建用）

use crate::repo::StorageResultExt;
use crate::repo::parse_uuid_optional;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{
    EvidenceDirection, PersonalityTrait, TraitEvidence, TraitLayer, TraitSource, TraitStatus,
};
use sqlx::SqlitePool;

// 使用共享宏生成枚举解析函数，消除 4 个手写 match 链的重复
parse_enum_fallback!(
    parse_layer, TraitLayer, TraitLayer::Base, "personality_traits", "layer",
    "base"    => Base,
    "primary" => Primary,
    "accent"  => Accent,
);
parse_enum_fallback!(
    parse_trait_source, TraitSource, TraitSource::L1, "personality_traits", "source",
    "l1"       => L1,
    "event"    => Event,
    "manual"   => Manual,
    "inferred" => Inferred,
);
parse_enum_fallback!(
    parse_trait_status, TraitStatus, TraitStatus::Active, "personality_traits", "status",
    "active"     => Active,
    "deprecated" => Deprecated,
    "historical" => Historical,
);
parse_enum_fallback!(
    parse_evidence_dir, EvidenceDirection, EvidenceDirection::Support, "trait_evidence", "direction",
    "support"    => Support,
    "contradict" => Contradict,
    "neutral"    => Neutral,
);

// =========================================================
// L3 性格标签
// =========================================================

#[derive(sqlx::FromRow)]
struct TraitRow {
    id: i64,
    persona_uid: String,
    layer: String,
    trait_label: String,
    meaning: String,
    not_meaning: Option<String>,
    trigger: Option<String>,
    suppress: Option<String>,
    related: Option<String>,
    seq: i64,
    source: String,
    ref_event_id: Option<i64>,
    ref_l1_id: Option<String>,
    confidence: f64,
    evidence: f64,
    consistency: f64,
    status: String,
    created_at: i64,
    updated_at: i64,
}

impl TraitRow {
    fn into_trait(self) -> RamariaResult<PersonalityTrait> {
        let ref_l1_id = parse_uuid_optional(&self.ref_l1_id, "personality_traits", "ref_l1_id")?;
        Ok(PersonalityTrait {
            id: self.id,
            persona_uid: self.persona_uid,
            layer: parse_layer(&self.layer),
            trait_label: self.trait_label,
            meaning: self.meaning,
            not_meaning: self.not_meaning,
            trigger: self.trigger,
            suppress: self.suppress,
            related: self.related,
            seq: self.seq as i32,
            source: parse_trait_source(&self.source),
            ref_event_id: self.ref_event_id,
            ref_l1_id,
            confidence: self.confidence,
            evidence: self.evidence,
            consistency: self.consistency,
            status: parse_trait_status(&self.status),
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

pub async fn save_trait(pool: &SqlitePool, t: &PersonalityTrait) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO personality_traits (persona_uid, layer, trait, meaning, not_meaning,
         trigger, suppress, related, seq, source, ref_event_id, ref_l1_id,
         confidence, evidence, consistency, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&t.persona_uid)
    .bind(t.layer.as_str())
    .bind(&t.trait_label)
    .bind(&t.meaning)
    .bind(&t.not_meaning)
    .bind(&t.trigger)
    .bind(&t.suppress)
    .bind(&t.related)
    .bind(t.seq)
    .bind(t.source.as_str())
    .bind(t.ref_event_id)
    .bind(t.ref_l1_id.map(|u| u.to_string()))
    .bind(t.confidence)
    .bind(t.evidence)
    .bind(t.consistency)
    .bind(t.status.as_str())
    .bind(t.created_at)
    .bind(t.updated_at)
    .fetch_one(pool)
    .await
    .storage_err("保存性格标签失败")
}

pub async fn list_traits_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<PersonalityTrait>> {
    let rows = sqlx::query_as::<_, TraitRow>(
        "SELECT id, persona_uid, layer, trait as trait_label, meaning, not_meaning, trigger, suppress,
         related, seq, source, ref_event_id, ref_l1_id, confidence, evidence, consistency,
         status, created_at, updated_at
         FROM personality_traits WHERE persona_uid = ? AND status = 'active' ORDER BY layer, seq"
    )    .bind(persona_uid).fetch_all(pool).await
        .storage_err("查询性格标签列表失败")?;
    rows.into_iter()
        .map(|r| r.into_trait())
        .collect::<RamariaResult<Vec<_>>>()
}

pub async fn update_confidence(
    pool: &SqlitePool,
    id: i64,
    confidence: f64,
    evidence: f64,
    consistency: f64,
) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("UPDATE personality_traits SET confidence = ?, evidence = ?, consistency = ?, updated_at = ? WHERE id = ?")
        .bind(confidence).bind(evidence).bind(consistency).bind(now).bind(id)
        .execute(pool).await
        .storage_err("更新性格置信度失败")?;
    Ok(())
}

pub async fn update_status(pool: &SqlitePool, id: i64, status: TraitStatus) -> RamariaResult<()> {
    let now = ramaria_core::types::now_ms();
    sqlx::query("UPDATE personality_traits SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status.as_str())
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .storage_err("更新性格状态失败")?;
    Ok(())
}

// =========================================================
// 性格证据链（trait_id/event_id 均为 i64）
// =========================================================

pub async fn save_evidence(pool: &SqlitePool, e: &TraitEvidence) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO trait_evidence (trait_id, event_id, direction, score, decay, created_at)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(e.trait_id)
    .bind(e.event_id)
    .bind(e.direction.as_str())
    .bind(e.score)
    .bind(e.decay)
    .bind(e.created_at)
    .fetch_one(pool)
    .await
    .storage_err("保存证据失败")
}

pub async fn list_evidence_by_trait(
    pool: &SqlitePool,
    trait_id: i64,
) -> RamariaResult<Vec<TraitEvidence>> {
    #[derive(sqlx::FromRow)]
    struct EvRow {
        id: i64,
        trait_id: i64,
        event_id: i64,
        direction: String,
        score: f64,
        decay: f64,
        created_at: i64,
    }
    let rows = sqlx::query_as::<_, EvRow>(
        "SELECT id, trait_id, event_id, direction, score, decay, created_at
         FROM trait_evidence WHERE trait_id = ? ORDER BY created_at DESC",
    )
    .bind(trait_id)
    .fetch_all(pool)
    .await
    .storage_err("查询证据列表失败")?;
    Ok(rows
        .into_iter()
        .map(|r| TraitEvidence {
            id: r.id,
            trait_id: r.trait_id,
            event_id: r.event_id,
            direction: parse_evidence_dir(&r.direction),
            score: r.score,
            decay: r.decay,
            created_at: r.created_at,
        })
        .collect())
}
