//! crates/ramaria-service/src/persona.rs - 人格读取用例（persona_list / persona_get）
//!
//! 设计特点:
//! - 纯读取：人格摘要列表与人格卡片（性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度）
//! - 严格按 persona_uid 隔离：卡片只读取目标人格的记录，不跨人格聚合
//! - 逐段独立降级：任一段读取失败记 warn 并返回空段，不阻塞整张卡片
//! - 条目上限：各段最多返回 [`MAX_CARD_ITEMS`] 条（避免大库把整张卡片撑爆）
//! - 隐私：卡片不含 utt 原文块（原文是最高敏感层，按召回策略单独控制）

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{ProfileField, TraitStatus};

use crate::engine::Engine;
use crate::types::{
    BehaviorRuleView, DataMaturityView, FactView, PersonaCardRequest, PersonaCardView,
    PersonaSection, PersonaSummaryView, StyleView, TraitView,
};

/// 人格卡片各段最多返回的条目数。
const MAX_CARD_ITEMS: usize = 20;

/// 数据成熟度计数查询上限（诊断用途，超量按上限计）。
const MATURITY_COUNT_LIMIT: u32 = 1_000;

// =========================================================
// persona_list
// =========================================================

/// 列出全部人格摘要（uid / 名称 / 类型 / 来源 / 简介 / 启用状态）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 按存储层顺序返回全部人格（含停用项，调用方可按 `active` 过滤）。
pub(crate) async fn list(engine: &Engine) -> RamariaResult<Vec<PersonaSummaryView>> {
    let personas = engine.storage_ref().list_personas().await?;
    let views = personas
        .into_iter()
        .map(|p| PersonaSummaryView {
            uid: p.uid,
            name: p.name,
            kind: p.kind,
            source: p.source,
            description: p.description,
            active: p.active,
        })
        .collect();
    Ok(views)
}

// =========================================================
// persona_get（人格卡片）
// =========================================================

/// 组装人格卡片（按 `sections` 选择分段；缺省全部分段）。
///
/// 流程:
/// 1. 读取人格行（不存在 → Validation 错误，错误可见）；
/// 2. 按分段读取关联数据（每段独立降级，失败记 warn 并置空）；
/// 3. 组装视图（各段受 [`MAX_CARD_ITEMS`] 截断；成熟度计数为全量计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 卡片请求（uid 必填；sections 可选）。
///
/// 返回:
/// - 成功时返回人格卡片视图；人格不存在时返回 `Validation` 错误。
pub(crate) async fn card(
    engine: &Engine,
    req: PersonaCardRequest,
) -> RamariaResult<PersonaCardView> {
    let storage = engine.storage_ref();
    let uid = req.uid.trim().to_string();
    if uid.is_empty() {
        return Err(RamariaError::validation("uid 不能为空"));
    }

    let persona = match storage.get_persona_by_uid(&uid).await? {
        Some(persona) => persona,
        None => {
            return Err(RamariaError::validation(format!("人格不存在: {uid}")));
        }
    };

    let sections = req.effective_sections();
    let wants = |section: PersonaSection| sections.contains(&section);

    // ---- 性格画像（L3 三层标签，仅 Active 参与展示） ----
    let traits = if wants(PersonaSection::Traits) || wants(PersonaSection::Maturity) {
        match storage.list_traits_by_persona(&uid).await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取性格标签失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let trait_views: Vec<TraitView> = if wants(PersonaSection::Traits) {
        traits
            .iter()
            .filter(|t| t.status == TraitStatus::Active)
            .take(MAX_CARD_ITEMS)
            .map(|t| TraitView {
                layer: t.layer,
                label: t.trait_label.clone(),
                meaning: t.meaning.clone(),
                trigger: t.trigger.clone(),
                confidence: t.confidence,
            })
            .collect()
    } else {
        Vec::new()
    };

    // ---- 行为规则（含停用项，`enabled` 字段供调用方判断） ----
    let behaviors: Vec<BehaviorRuleView> = if wants(PersonaSection::Behaviors) {
        match storage.list_behavior_rules_by_persona(&uid).await {
            Ok(rules) => rules
                .iter()
                .take(MAX_CARD_ITEMS)
                .map(|rule| BehaviorRuleView {
                    id: rule.id,
                    situation: rule.situation.keywords.join("、"),
                    reaction: rule.reaction.clone(),
                    avoid: rule.avoid.clone(),
                    confidence: rule.confidence,
                    enabled: rule.enabled,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取行为规则失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // ---- 表达风格（自动风格规则，仅 Ready 有文本） ----
    let style = if wants(PersonaSection::Style) {
        match storage.get_style_stats(&uid).await {
            Ok(Some(stats)) => Some(StyleView {
                rule_text: stats.rule_text.clone().filter(|t| !t.trim().is_empty()),
                status: stats.status,
                sample_count: stats.sample_count,
            }),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取风格统计失败，卡片该段为空");
                None
            }
        }
    } else {
        None
    };

    // ---- 知识事实（active；SpeakingStyle 由表达层单独呈现，此处不重复） ----
    let facts = if wants(PersonaSection::Facts) || wants(PersonaSection::Maturity) {
        match storage.list_active_facts_by_persona(&uid).await {
            Ok(facts) => facts,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取知识事实失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let fact_views: Vec<FactView> = if wants(PersonaSection::Facts) {
        facts
            .iter()
            .take(MAX_CARD_ITEMS)
            .map(|f| FactView {
                field: f.field,
                content: f.content.clone(),
                tier: f.tier,
                confidence: f.confidence,
            })
            .collect()
    } else {
        Vec::new()
    };

    // ---- 数据成熟度（各层数据量计数；诊断用，受查询上限约束） ----
    let maturity = if wants(PersonaSection::Maturity) {
        maturity_view(engine, &uid, &traits, &facts).await
    } else {
        DataMaturityView::default()
    };

    tracing::debug!(
        uid = %uid,
        traits = trait_views.len(),
        behaviors = behaviors.len(),
        facts = fact_views.len(),
        "人格卡片已组装"
    );

    Ok(PersonaCardView {
        uid: persona.uid,
        name: persona.name,
        kind: persona.kind,
        source: persona.source,
        description: persona.description,
        active: persona.active,
        traits: trait_views,
        behaviors,
        style,
        facts: fact_views,
        maturity,
    })
}

/// 组装数据成熟度视图（各层数据量计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标人格。
/// - `traits` / `facts`: 已读取的画像数据（复用，避免重复查询）。
///
/// 返回:
/// - 计数视图；单项查询失败记 warn 并按 0 计（不阻塞卡片）。
async fn maturity_view(
    engine: &Engine,
    uid: &str,
    traits: &[ramaria_core::types::PersonalityTrait],
    facts: &[ramaria_core::types::PersonaFact],
) -> DataMaturityView {
    let storage = engine.storage_ref();

    // L1 摘要条数（按 persona 取最近 N 条计数，超量按上限计）
    let l1_count = match storage
        .list_recent_l1_by_persona(uid, MATURITY_COUNT_LIMIT)
        .await
    {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取 L1 计数失败，按 0 计");
            0
        }
    };

    // L2 事件条数
    let event_count = match storage
        .list_events_by_persona(uid, 0, MATURITY_COUNT_LIMIT as i64)
        .await
    {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取事件计数失败，按 0 计");
            0
        }
    };

    // 对话示例条数（候选池全量）
    let example_count = match storage.list_all_examples(uid).await {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取示例计数失败，按 0 计");
            0
        }
    };

    // 知识事实条数：排除 SpeakingStyle（表达层单独呈现，不计入知识卡片成熟度）
    let fact_count = facts
        .iter()
        .filter(|f| f.field != ProfileField::SpeakingStyle)
        .count();

    DataMaturityView {
        l1_count,
        event_count,
        trait_count: traits
            .iter()
            .filter(|t| t.status == TraitStatus::Active)
            .count(),
        fact_count,
        example_count,
    }
}
