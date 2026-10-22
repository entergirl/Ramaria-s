//! crates/ramaria-service/src/persona/import/write.rs - Ramaria 预设条目写入
//!
//! 设计特点:
//! - 条目逐条独立：单条写入失败计数后继续，不中断其余条目
//! - 现有画像快照供合并裁决，读取失败降级为空快照
//! - 隐私：日志不含预设原文，个人标识经脱敏

use std::sync::Arc;

use ramaria_core::behavior::{BehaviorRule, BehaviorSituation, RuleSource};
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    FactSource, PersonaExample, PersonaFact, PersonalityTrait, TraitSource, TraitStatus,
};

use ramaria_memory::fact::tier_for_field;
use ramaria_memory::preset::{EntryDecision, ExistingProfile, ImportPlan};
use ramaria_memory::prompt::example_selector::extract_keywords;

use crate::engine::Engine;
use crate::types::ImportWriteStats;

use super::parse::layer_index;

// =========================================================
// L3 条目写入
// =========================================================

/// 写入四类 L3 条目（仅裁决为写入的条目；逐条独立，失败计数不中断）。
///
/// 说明:
/// - traits 分层续号：起始 = 库侧各层最大 seq，按计划顺序逐条 +1（层内声明顺序）；
/// - 落库默认值：traits `Manual / Active / 0.5 / 1.0 / 0.5`；
///   facts `Manual / tier 按字段分层 / 1.0`；examples `selected` + 关键词 tags；
///   rules `Manual / 1.0 / 1.0 / enabled`。
pub(super) async fn write_entries(
    storage: &Arc<dyn StorageBackend>,
    uid: &str,
    plan: &ImportPlan,
    trait_max_seq: [i32; 3],
) -> ImportWriteStats {
    let mut stats = ImportWriteStats::default();
    let mut trait_seq = trait_max_seq;

    for planned in plan
        .traits
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let index = layer_index(entry.layer);
        trait_seq[index] += 1;

        let mut trait_row = PersonalityTrait::new(
            uid.to_string(),
            entry.layer,
            entry.label.clone(),
            entry.meaning.clone(),
            TraitSource::Manual,
            trait_seq[index],
        );
        trait_row.not_meaning = entry.not_meaning.clone();
        trait_row.trigger = entry.trigger.clone();
        trait_row.suppress = entry.suppress.clone();
        trait_row.related = entry.related.clone();
        trait_row.status = TraitStatus::Active;
        trait_row.confidence = 0.5;
        trait_row.evidence = 1.0;
        trait_row.consistency = 0.5;

        match storage.save_trait(&trait_row).await {
            Ok(_) => stats.traits += 1,
            Err(e) => {
                tracing::warn!(label = %entry.label, error = %e, "写入预设 trait 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    for planned in plan
        .facts
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let mut fact = PersonaFact::new(
            uid.to_string(),
            entry.field,
            entry.content.clone(),
            FactSource::Manual,
        );
        fact.tier = tier_for_field(entry.field);

        match storage.save_fact(&fact).await {
            Ok(_) => stats.facts += 1,
            Err(e) => {
                tracing::warn!(field = %entry.field, error = %e, "写入预设 fact 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    for planned in plan
        .examples
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let mut example =
            PersonaExample::new(uid.to_string(), entry.user.clone(), entry.assistant.clone());
        example.selected = true;
        example.tags =
            Some(extract_keywords(&format!("{} {}", entry.user, entry.assistant)).join(","));

        match storage.save_example(&example).await {
            Ok(_) => stats.examples += 1,
            Err(e) => {
                tracing::warn!(error = %e, "写入预设 example 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    for planned in plan
        .rules
        .iter()
        .filter(|planned| planned.decision == EntryDecision::Write)
    {
        let entry = &planned.entry;
        let situation = BehaviorSituation {
            keywords: entry.keywords.clone(),
            ..BehaviorSituation::empty()
        };
        let mut rule = BehaviorRule::new(
            uid.to_string(),
            situation,
            entry.reaction.clone(),
            entry.params,
            RuleSource::Manual,
        );
        rule.avoid = entry.avoid.clone();
        rule.confidence = 1.0;
        rule.stability = 1.0;

        match storage.save_behavior_rule(&rule).await {
            Ok(_) => stats.behavior_rules += 1,
            Err(e) => {
                tracing::warn!(error = %e, "写入预设 behavior_rules 失败，跳过该条");
                stats.failed += 1;
            }
        }
    }

    stats
}

// =========================================================
// 库侧现状快照
// =========================================================

/// 读取目标 uid 的库侧现状快照（目标不存在时返回 exists=false；`all_uids` 总是填充）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标 uid（生效注册决议）。
/// - `all_uids`: 库内全部 active persona 的 uid（与注册决议同源，避免重复读取）。
///
/// 返回:
/// - 现状快照：注册列 / traits label 与各层最大 seq / facts / examples / rules 判重键。
pub(super) async fn read_existing_profile(
    engine: &Engine,
    uid: &str,
    all_uids: Vec<String>,
) -> RamariaResult<ExistingProfile> {
    let storage = engine.storage_ref();
    let mut profile = ExistingProfile {
        all_uids,
        ..ExistingProfile::default()
    };

    let Some(persona) = storage.get_persona_by_uid(uid).await? else {
        return Ok(profile);
    };

    profile.exists = true;
    profile.kind = Some(persona.kind);
    profile.name = Some(persona.name);
    profile.avatar = persona.avatar;
    profile.description = persona.description;
    profile.has_config = persona
        .config
        .as_deref()
        .is_some_and(|config| !config.trim().is_empty());

    for trait_row in storage.list_traits_by_persona(uid).await? {
        let index = layer_index(trait_row.layer);
        if trait_row.seq > profile.trait_max_seq[index] {
            profile.trait_max_seq[index] = trait_row.seq;
        }
        profile.trait_labels.push(trait_row.trait_label);
    }
    for fact in storage.list_all_facts_by_persona(uid).await? {
        profile.fact_pairs.push((fact.field, fact.content));
    }
    for example in storage.list_all_examples(uid).await? {
        profile.example_pairs.push((example.partner, example.reply));
    }
    for rule in storage.list_behavior_rules_by_persona(uid).await? {
        profile
            .rule_keys
            .push((rule.situation.keywords, rule.reaction));
    }

    Ok(profile)
}

/// 库内全部 persona 的 uid 列表（注册决议与自动生成序号来源）。
pub(super) async fn list_persona_uids(engine: &Engine) -> RamariaResult<Vec<String>> {
    Ok(engine
        .storage_ref()
        .list_personas()
        .await?
        .into_iter()
        .map(|persona| persona.uid)
        .collect())
}
