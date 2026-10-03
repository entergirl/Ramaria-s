//! crates/ramaria-service/src/browse/view.rs - Ramaria 浏览视图组装与输入归一辅助
//!
//! 设计特点:
//! - 跨域共享：浏览口径默认值与上限、persona 归一 / 校验、时间戳转换与各域视图组装
//! - 视图映射逐字段透传（伴随字段保留原始空值），不引入存储层之外的口径
//! - 归一规则：空白 persona 视为未提供；必填 persona 空白拒绝（业务校验错误）
//! - 时间兜底：非法毫秒时间戳回退 Unix 纪元

use chrono::{DateTime, Utc};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{MemoryEvent, MemoryL1, Message, PersonaFact, PersonalityTrait};

use crate::types::{
    FactEntryView, L1MemoryView, L2EventView, L3TraitView, SessionMessageView, TraitDetailView,
};

// =========================================================
// 跨域共享的浏览口径常量
// =========================================================

/// 浏览类用例的默认返回条数（L1 / L2 缺省取该值）。
pub(super) const DEFAULT_BROWSE_LIMIT: u32 = 200;

/// 浏览类用例的单次返回条数上限（按会话收集口径的 L1 与 L2 超过时截断）。
pub(super) const MAX_BROWSE_LIMIT: u32 = 1000;

// =========================================================
// 输入归一
// =========================================================

/// 归一化人格 uid（空白视为未提供）。
pub(super) fn normalize_persona(persona: Option<&str>) -> Option<String> {
    persona
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// 校验人格 uid（空白拒绝），返回去除首尾空白后的值。
pub(super) fn require_persona_uid(raw: &str) -> RamariaResult<String> {
    let uid = raw.trim().to_string();
    if uid.is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }
    Ok(uid)
}

/// 毫秒时间戳转换为 UTC 时间（非法值回退 Unix 纪元）。
pub(super) fn to_datetime(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

// =========================================================
// 视图组装
// =========================================================

/// 组装 L1 摘要浏览视图（伴随字段保留原始空值）。
pub(super) fn l1_view(m: &MemoryL1) -> L1MemoryView {
    L1MemoryView {
        id: m.id,
        session_id: m.session_id,
        summary: m.summary.clone(),
        keywords: m.keywords.clone(),
        atmosphere: m.atmosphere.clone(),
        time_period: m.time_period.clone(),
        context_json: m.context_json.clone(),
        valence: m.valence,
        salience: m.salience,
        persona_uid: m.persona_uid.clone(),
        created_at: m.created_at,
    }
}

/// 组装 L2 事件浏览视图。
pub(super) fn l2_view(e: &MemoryEvent) -> L2EventView {
    L2EventView {
        id: e.id,
        persona_uid: e.persona_uid.clone(),
        title: e.title.clone(),
        summary: e.summary.clone(),
        keywords: e.keywords.clone(),
        valence: e.valence,
        confidence: e.confidence,
        presentation: e.presentation,
        share: e.share,
        attitude: e.attitude.clone(),
        salience: e.salience,
        created_at: e.created_at,
        start: e.start,
        end: e.end,
    }
}

/// 组装 L3 性格标签浏览视图。
pub(super) fn l3_view(t: &PersonalityTrait) -> L3TraitView {
    L3TraitView {
        id: t.id,
        persona_uid: t.persona_uid.clone(),
        layer: t.layer,
        label: t.trait_label.clone(),
        meaning: t.meaning.clone(),
        confidence: t.confidence,
        evidence: t.evidence,
        consistency: t.consistency,
        status: t.status,
        created_at: t.created_at,
    }
}

/// 组装三层画像的标签详细视图。
pub(super) fn trait_detail_view(t: &PersonalityTrait) -> TraitDetailView {
    TraitDetailView {
        id: t.id,
        label: t.trait_label.clone(),
        meaning: t.meaning.clone(),
        confidence: t.confidence,
        evidence: t.evidence,
        consistency: t.consistency,
        layer: t.layer,
        not_meaning: t.not_meaning.clone(),
        trigger: t.trigger.clone(),
        suppress: t.suppress.clone(),
        related: t.related.clone(),
        seq: t.seq,
        source: t.source,
        status: t.status,
        created_at: t.created_at,
    }
}

/// 组装知识事实条目视图（全字段）。
pub(super) fn fact_view(f: &PersonaFact) -> FactEntryView {
    FactEntryView {
        id: f.id,
        persona_uid: f.persona_uid.clone(),
        field: f.field,
        content: f.content.clone(),
        source: f.source,
        status: f.status,
        tier: f.tier,
        version_of: f.version_of,
        confidence: f.confidence,
        keyword_hint: f.keyword_hint.clone(),
        ref_event_id: f.ref_event_id,
        ref_l1_id: f.ref_l1_id,
        created_at: f.created_at,
        updated_at: f.updated_at,
    }
}

/// 组装消息浏览条目视图。
pub(super) fn message_view(m: &Message) -> SessionMessageView {
    SessionMessageView {
        id: m.id,
        role: m.role,
        content: m.content.clone(),
        created_at: m.created_at,
        source: m.source,
        persona_uid: m.persona_uid.clone(),
        is_proactive: m.is_proactive,
    }
}
