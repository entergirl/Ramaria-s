//! crates/ramaria-service/src/recall/layers.rs - 各辅助分层读取与条目映射
//!
//! 设计特点:
//! - 逐层读取与渲染：行为 / 知识 / 表达（风格）/ 脉络 / 画像（L3），每层一个独立函数
//! - 单层固定上界 [`MAX_AUX_LAYER_ITEMS`]，避免单层挤占总预算（最终仍受 max_items 约束）
//! - 静默降级：层关闭 / 无数据 / 读取失败 → 空（记 warn），不阻塞召回
//! - 结构化条目映射：事实 / 性格标签 → [`RecallItem`]，时间统一经 [`iso_time`] 转换
//! - 脉络层只贡献叙事段落不产出条目（素材已由记忆层以带分数条目返回，避免重复占预算）

use chrono::{DateTime, Utc};
use ramaria_core::types::{PersonaFact, PersonalityTrait, StyleStatsStatus};
use ramaria_memory::prompt::builder::build_cross_session_narrative;

use crate::engine::Engine;
use crate::types::{RecallItem, RecallLayer};

// =========================================================
// 常量
// =========================================================

/// 单层最多返回的条目数（辅助层：行为 / 知识 / 风格 / 画像 / 脉络）。
///
/// 说明:
/// - 记忆层条目上限由 `[retrieval].rag_max_memories` 控制（共用实现内截断）；
/// - 辅助层用固定上界避免单层挤占预算（最终仍受 `max_items` 总预算约束）。
pub(super) const MAX_AUX_LAYER_ITEMS: usize = 5;

/// 脉络层读取的近期摘要条数（跨会话叙事素材）。
const NARRATIVE_RECENT_L1: u32 = 5;

// =========================================================
// 各分层读取与渲染
// =========================================================

/// 行为层：渲染启用的行为规则（情境 → 反应）。
///
/// 降级:
/// - `[behavior].enabled=false` / 无启用规则 / 读取失败 → 空（不注入）。
pub(super) async fn behavior_layer(
    engine: &Engine,
    persona: &str,
) -> (Option<String>, Vec<RecallItem>) {
    if !engine.config().behavior.enabled {
        return (None, Vec::new());
    }
    let rules = match engine
        .storage_ref()
        .list_behavior_rules_by_persona(persona)
        .await
    {
        Ok(rules) => rules,
        Err(e) => {
            tracing::warn!(persona, error = %e, "行为层读取失败，本次不注入");
            return (None, Vec::new());
        }
    };

    let active: Vec<_> = rules
        .iter()
        .filter(|rule| rule.enabled)
        .filter(|rule| {
            rule.reaction
                .as_deref()
                .map(|text| !text.trim().is_empty())
                .unwrap_or(false)
        })
        .take(MAX_AUX_LAYER_ITEMS)
        .collect();
    if active.is_empty() {
        return (None, Vec::new());
    }

    let mut lines = vec!["# 行为（行为层）".to_string()];
    let mut items = Vec::with_capacity(active.len());
    for rule in active {
        let situation = rule.situation.keywords.join("、");
        let reaction = rule.reaction.clone().unwrap_or_default();
        lines.push(format!("- 情境「{situation}」→ {reaction}"));
        items.push(RecallItem {
            layer: RecallLayer::Behavior,
            id: rule.id.to_string(),
            text: reaction,
            score: Some(rule.confidence),
            time: iso_time(rule.updated_at),
        });
    }
    (Some(lines.join("\n")), items)
}

/// 知识层：判定器命中后渲染知识卡片。
///
/// 降级:
/// - 判定器关闭 / 未命中 / 读取失败 → 空（不注入）。
pub(super) async fn knowledge_layer(
    engine: &Engine,
    persona: &str,
    query: &str,
) -> (Option<String>, Vec<RecallItem>) {
    let config = engine.config().knowledge.clone();
    let facts = ramaria_memory::fact::retriever::load_knowledge_facts_for_query(
        engine.storage_ref().as_ref(),
        &config,
        persona,
        query,
    )
    .await;
    if facts.is_empty() {
        return (None, Vec::new());
    }

    let cards = ramaria_memory::fact::retriever::render_knowledge_cards(&facts);
    if cards.trim().is_empty() {
        return (None, Vec::new());
    }
    let text = format!("# 知识（知识层）\n{cards}");
    let items = facts.iter().map(fact_item).collect();
    (Some(text), items)
}

/// 概览用的知识摘要条目（不做判定器命中判断，直接取 active 事实前若干条）。
pub(super) async fn knowledge_overview_items(
    engine: &Engine,
    persona: &str,
) -> (Option<String>, Vec<RecallItem>) {
    let facts = match engine
        .storage_ref()
        .list_active_facts_by_persona(persona)
        .await
    {
        Ok(facts) => facts,
        Err(e) => {
            tracing::warn!(persona, error = %e, "概览：读取 active 事实失败，跳过");
            return (None, Vec::new());
        }
    };
    let items: Vec<RecallItem> = facts
        .iter()
        .take(MAX_AUX_LAYER_ITEMS)
        .map(fact_item)
        .collect();
    (None, items)
}

/// 事实 → 结构化条目（渲染为 `字段：内容` 文本）。
fn fact_item(fact: &PersonaFact) -> RecallItem {
    let time = if fact.updated_at > 0 {
        fact.updated_at
    } else {
        fact.created_at
    };
    RecallItem {
        layer: RecallLayer::Knowledge,
        id: fact.id.to_string(),
        text: format!("{}：{}", fact.field.label(), fact.content.trim()),
        score: Some(fact.confidence),
        time: iso_time(time),
    }
}

/// 表达层：自动风格规则（仅 `Ready` 状态注入）。
///
/// 降级:
/// - `[style].enabled=false` / 未统计 / 样本不足（非 Ready）/ 读取失败 → 空（不注入）。
pub(super) async fn style_layer(
    engine: &Engine,
    persona: &str,
) -> (Option<String>, Vec<RecallItem>) {
    if !engine.config().style.enabled {
        return (None, Vec::new());
    }
    let stats = match engine.storage_ref().get_style_stats(persona).await {
        Ok(Some(stats)) => stats,
        Ok(None) => return (None, Vec::new()),
        Err(e) => {
            tracing::warn!(persona, error = %e, "表达层读取风格统计失败，本次不注入");
            return (None, Vec::new());
        }
    };
    if stats.status != StyleStatsStatus::Ready {
        return (None, Vec::new());
    }
    let Some(rule) = stats.rule_text.clone().filter(|t| !t.trim().is_empty()) else {
        return (None, Vec::new());
    };

    let text = format!("# 表达风格（表达层）\n- {rule}");
    let items = vec![RecallItem {
        layer: RecallLayer::Style,
        id: "style".to_string(),
        text: rule,
        score: None,
        time: iso_time(stats.updated_at),
    }];
    (Some(text), items)
}

/// 脉络层：近期 L1 摘要拼装跨会话叙事。
pub(super) async fn narrative_layer(
    engine: &Engine,
    persona: &str,
) -> (Option<String>, Vec<RecallItem>) {
    let recent = match engine
        .storage_ref()
        .list_recent_l1_by_persona(persona, NARRATIVE_RECENT_L1)
        .await
    {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(persona, error = %e, "脉络层读取近期摘要失败，本次不注入");
            return (None, Vec::new());
        }
    };
    if recent.is_empty() {
        return (None, Vec::new());
    }

    let summaries: Vec<String> = recent.iter().map(|l1| l1.summary.clone()).collect();
    let narrative = build_cross_session_narrative(&summaries);
    if narrative.trim().is_empty() {
        return (None, Vec::new());
    }

    // 脉络层只贡献叙事段落，不产出结构化条目：
    // 其素材（近期 L1）已由记忆层以带分数与时间的条目返回，此处再列会重复占用 items 预算。
    (Some(narrative), Vec::new())
}

/// 画像层（L3）：性格标签。
pub(super) async fn trait_layer(
    engine: &Engine,
    persona: &str,
) -> (Option<String>, Vec<RecallItem>) {
    let traits = match engine.storage_ref().list_traits_by_persona(persona).await {
        Ok(traits) => traits,
        Err(e) => {
            tracing::warn!(persona, error = %e, "画像层读取性格标签失败，本次不注入");
            return (None, Vec::new());
        }
    };
    let active: Vec<_> = traits
        .iter()
        .filter(|t| t.status == ramaria_core::types::TraitStatus::Active)
        .take(MAX_AUX_LAYER_ITEMS)
        .collect();
    if active.is_empty() {
        return (None, Vec::new());
    }

    let mut lines = vec!["# 性格画像（L3）".to_string()];
    let mut items = Vec::with_capacity(active.len());
    for t in active {
        lines.push(format!("- {}：{}", t.trait_label, t.meaning));
        items.push(trait_item(t));
    }
    (Some(lines.join("\n")), items)
}

/// 性格标签 → 结构化条目。
fn trait_item(t: &PersonalityTrait) -> RecallItem {
    RecallItem {
        layer: RecallLayer::L3,
        id: t.id.to_string(),
        text: format!("{}：{}", t.trait_label, t.meaning),
        score: Some(t.confidence),
        time: iso_time(t.updated_at),
    }
}

// =========================================================
// 辅助
// =========================================================

/// Unix 毫秒 → UTC 时间（非法值返回 None，不报错）。
pub(super) fn iso_time(ms: i64) -> Option<DateTime<Utc>> {
    if ms <= 0 {
        return None;
    }
    DateTime::from_timestamp_millis(ms)
}
