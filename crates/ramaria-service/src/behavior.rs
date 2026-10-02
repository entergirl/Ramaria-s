//! crates/ramaria-service/src/behavior.rs - 行为规则用例（管理 / 学习 / 增量更新）
//!
//! 设计特点:
//! - 学习管线：事件 → 聚类（含 Manual 强锚点）→ 规则生成 → 替换旧 Auto 规则落库
//! - 增量更新：封存触发的编排入口（归簇 / 待定池推进 / 证据衰减 / 漂移检测在
//!   `ramaria_memory::behavior::orchestrate`），供封存钩子与宿主手动补跑共用
//! - 规则管理：list / get / edit / enable / disable / delete / import / evidence
//! - 反馈环：edit / disable 写 feedback_log（S1 强信号，weight=1.0，detail 编辑快照）；
//!   edit 后规则转为 Manual（聚类强锚点，簇中心向 Manual 规则偏移）
//! - 手工导入：JSON 宽松校验（非法拒绝），导入规则 source=Manual、enabled=true
//! - 证据链：规则 → 事件 → 脱敏视图（权重降序，脏引用跳过；原文不落日志）
//!
//! 安全约束:
//! - 不记录完整用户消息 / 原文 / 完整 prompt
//! - evidence 返回事件的 paraphrase / summary 等脱敏字段，不返回原始对话

use std::sync::{Arc, Mutex};

use ramaria_core::behavior::{
    BehaviorRule, BehaviorSituation, FeedbackLog, RuleSource, SignalType, TargetType,
};
use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::{Presentation, now_ms};
use ramaria_memory::behavior::{
    BehaviorClusterer, BehaviorRuleGenerator, BehaviorSample, PendingPool, RuleGenConfig,
    sample_from_event,
};
use serde::{Deserialize, Serialize};

use crate::engine::Engine;

// =========================================================
// 类型
// =========================================================

/// 一次行为规则学习管线的输出统计。
#[derive(Debug, Clone, Default)]
pub struct BehaviorLearnOutcome {
    /// 输入事件数
    pub event_count: usize,
    /// 生成簇数
    pub cluster_count: usize,
    /// 完整规则数（含规则文本）
    pub full_rule_count: usize,
    /// 候选规则数（降级，仅参数注入）
    pub candidate_rule_count: usize,
    /// 被替换的旧 Auto 规则数
    pub replaced_rule_count: usize,
}

/// 规则证据链的一项（规则 → 事件 → 原文溯源）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleEvidenceItem {
    /// 事件 id
    pub event_id: i64,
    /// 证据权重
    pub weight: f64,
    /// 事件标题
    pub title: String,
    /// 事件摘要（2-3 句）
    pub summary: String,
    /// 去情境化态度（脱敏）
    pub paraphrase: Option<String>,
    /// 事件关键词
    pub keywords: Option<String>,
}

// =========================================================
// Manual 强锚点
// =========================================================

/// 将启用中的 Manual 规则转为聚类锚点样本（簇中心向 Manual 规则偏移）。
///
/// 说明:
/// - 锚点样本 event_id 用负值标记（非真实事件，不写入规则 evidence）。
/// - salience=1.0（强锚点，比重高于普通事件）。
/// - 向量直接取自规则的簇中心（无需重新 embedding）。
fn manual_anchor_samples(rules: &[BehaviorRule]) -> Vec<BehaviorSample> {
    rules
        .iter()
        .filter(|r| r.source == RuleSource::Manual && r.enabled)
        .map(|r| BehaviorSample {
            event_id: -r.id,
            situation_keywords: r.situation.keywords.clone(),
            situation_vector: r.situation.centroid.clone(),
            reaction_vector: r.situation.response_centroid.clone(),
            valence: r.situation.valence_mean,
            presentation: Presentation::Mixed,
            salience: 1.0,
            situation_strength: Some(3),
            start_ms: r.created_at,
        })
        .collect()
}

// =========================================================
// 学习管线
// =========================================================

/// 执行行为规则全量学习（事件 → 聚类 → 规则生成 → 替换旧 Auto 规则）。
///
/// 流程:
/// 1. 读取 persona 全部事件。
/// 2. 读取启用中的 Manual 规则 → 构造强锚点样本（簇中心向 Manual 偏移）。
/// 3. 聚类（双通道 + 关键词，embedding 不可用自动降级）。
/// 4. 逐簇规则生成（质控 / 极性校验降级链，LLM 不可用自动降级）。
/// 5. 删除该 persona 全部旧 Auto 规则 → 批量保存新规则（Auto 自动生效）。
///
/// 返回:
/// - 学习统计；行为配置关闭时返回空统计。
pub(crate) async fn learn(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<BehaviorLearnOutcome> {
    let mut outcome = BehaviorLearnOutcome::default();
    // 配置快照一次读取：开关判定与聚类 / 生成参数取自同一份配置
    let snapshot = engine.config();
    if !snapshot.behavior.enabled {
        tracing::info!("行为层已关闭（[behavior].enabled=false），跳过学习");
        return Ok(outcome);
    }

    let llm = engine.llm_ref();
    let embedding = engine.embedding_ref();
    let config = snapshot.behavior.clone();
    let storage = engine.storage_ref();

    // 1. 事件（全量，供学习聚类）
    let events = storage
        .list_events_by_persona(persona_uid, 0, i64::MAX)
        .await?;
    outcome.event_count = events.len();
    if events.is_empty() {
        return Ok(outcome);
    }

    // 2. 现有规则（Manual 锚点 + 旧 Auto 待替换）
    let existing = storage.list_behavior_rules_by_persona(persona_uid).await?;
    let manual_rules: Vec<BehaviorRule> = existing
        .iter()
        .filter(|r| r.source == RuleSource::Manual)
        .cloned()
        .collect();

    // 3. 聚类（含 Manual 强锚点）
    let mut samples: Vec<BehaviorSample> = events.iter().map(sample_from_event).collect();
    samples.extend(manual_anchor_samples(&manual_rules));
    let clusterer = BehaviorClusterer::new(&config, embedding.as_deref());
    let clusters = clusterer.cluster_samples(&events, &mut samples).await?;
    outcome.cluster_count = clusters.len();
    if clusters.is_empty() {
        return Ok(outcome);
    }

    // 4. 规则生成
    let generator = BehaviorRuleGenerator::new(RuleGenConfig::from(&config), llm.as_ref());
    let generated = generator.generate_rules(&clusters).await;

    // 5. 替换旧 Auto 规则并落库
    for rule in &existing {
        if rule.source == RuleSource::Auto {
            storage.delete_behavior_rule(rule.id).await?;
            outcome.replaced_rule_count += 1;
        }
    }
    for g in generated {
        let mut rule = g.rule;
        rule.persona_uid = persona_uid.to_string();
        // 过滤锚点证据（负 event_id 非真实事件）
        rule.evidence.retain(|e| e.event_id > 0);
        if rule.evidence.is_empty() && rule.has_reaction() {
            // 锚点驱动的簇（无真实证据）不应产生 Auto 规则（避免无据臆测）
            tracing::warn!(
                rule_id = rule.id,
                "行为规则簇仅由 Manual 锚点构成，跳过落库（无真实证据）"
            );
            continue;
        }
        if rule.has_reaction() {
            outcome.full_rule_count += 1;
        } else {
            outcome.candidate_rule_count += 1;
        }
        storage.save_behavior_rule(&rule).await?;
    }

    tracing::info!(
        persona_uid,
        clusters = outcome.cluster_count,
        full = outcome.full_rule_count,
        candidate = outcome.candidate_rule_count,
        replaced = outcome.replaced_rule_count,
        "行为规则学习完成"
    );
    Ok(outcome)
}

// =========================================================
// 规则管理
// =========================================================

/// 列出 persona 的全部规则（含禁用项）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 UID。
///
/// 返回:
/// - 该人格的全量规则列表（存储层稳定排序）。
pub(crate) async fn list_rules(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<Vec<BehaviorRule>> {
    engine
        .storage_ref()
        .list_behavior_rules_by_persona(persona_uid)
        .await
}

/// 查看单条规则。
///
/// 返回:
/// - `Ok(Some(rule))`: 规则存在；
/// - `Ok(None)`: 规则不存在（空态，非错误）。
pub(crate) async fn get_rule(engine: &Engine, id: i64) -> RamariaResult<Option<BehaviorRule>> {
    engine.storage_ref().get_behavior_rule(id).await
}

/// 启用/禁用规则（disable 写 feedback_log；enable 不写——非干预）。
///
/// 参数:
/// - `id`: 规则 id。
/// - `enabled`: true = 启用，false = 禁用。
/// - `session_id`: 干预发生的会话（可选，审计关联）。
pub(crate) async fn set_rule_enabled(
    engine: &Engine,
    id: i64,
    enabled: bool,
    session_id: Option<&str>,
) -> RamariaResult<()> {
    let storage = engine.storage_ref().as_ref();
    storage.set_rule_enabled(id, enabled).await?;
    if !enabled {
        // 禁用 = S1 强信号（用户显式干预）
        let rule = storage.get_behavior_rule(id).await?;
        if let Some(rule) = rule {
            let log = FeedbackLog::new(
                rule.persona_uid,
                TargetType::BehaviorRule,
                id.to_string(),
                SignalType::Disable,
                session_id.map(String::from),
                Some(serde_json::json!({ "enabled": false }).to_string()),
            );
            storage.save_feedback_log(&log).await?;
        }
    }
    Ok(())
}

/// 编辑规则（edit 写 feedback_log + 规则转为 Manual 强锚点）。
///
/// 参数:
/// - `rule`: 编辑后的完整规则（id 定位，reaction/params/avoid/situation 全量覆盖）。
/// - `session_id`: 干预发生的会话（可选，审计关联）。
///
/// 说明:
/// - 编辑即显式干预（S1 强信号）→ 规则 source 转为 Manual（优先级高于 Auto，
///   后续学习作为聚类强锚点）。
/// - feedback_log 记录编辑前后快照（只存规则字段 JSON，不含原文）。
pub(crate) async fn edit_rule(
    engine: &Engine,
    rule: &mut BehaviorRule,
    session_id: Option<&str>,
) -> RamariaResult<()> {
    let storage = engine.storage_ref().as_ref();
    let before = storage
        .get_behavior_rule(rule.id)
        .await?
        .ok_or_else(|| RamariaError::validation(format!("行为规则 {} 不存在", rule.id)))?;

    rule.source = RuleSource::Manual;
    rule.updated_at = now_ms();
    storage.update_behavior_rule(rule).await?;

    // S1 反馈日志（编辑前后快照）
    let detail = serde_json::json!({
        "before": {
            "reaction": before.reaction,
            "params": before.params,
            "avoid": before.avoid,
            "enabled": before.enabled,
        },
        "after": {
            "reaction": rule.reaction,
            "params": rule.params,
            "avoid": rule.avoid,
            "enabled": rule.enabled,
        },
    })
    .to_string();
    let log = FeedbackLog::new(
        rule.persona_uid.clone(),
        TargetType::BehaviorRule,
        rule.id.to_string(),
        SignalType::Edit,
        session_id.map(String::from),
        Some(detail),
    );
    storage.save_feedback_log(&log).await?;
    Ok(())
}

/// 删除规则（破坏性操作，调用方负责确认）。
pub(crate) async fn delete_rule(engine: &Engine, id: i64) -> RamariaResult<()> {
    engine.storage_ref().delete_behavior_rule(id).await
}

/// 手工导入规则（JSON 校验）。
///
/// 参数:
/// - `persona_uid`: 规则所属人格。
/// - `json`: 规则 JSON（含 situation / reaction / params / avoid 字段）。
///
/// 校验规则:
/// - situation 必须存在（keywords 或 centroid 至少一项）。
/// - reaction 与 params 至少一项（空规则拒绝）。
/// - situation 为宽松解析（缺失字段用默认值，手工导入无需写全统计字段）。
/// - 非法 JSON / 缺字段 → `Validation` 错误（拒绝导入）。
///
/// 返回:
/// - 新规则 id（source=Manual，enabled=true）。
pub(crate) async fn import_rule(
    engine: &Engine,
    persona_uid: &str,
    json: &str,
) -> RamariaResult<i64> {
    #[derive(Deserialize)]
    struct ImportSituation {
        keywords: Option<Vec<String>>,
        centroid: Option<Vec<f32>>,
        response_centroid: Option<Vec<f32>>,
        valence_mean: Option<f64>,
        valence_std: Option<f64>,
        sample_count: Option<usize>,
        presentation_dist: Option<Vec<ramaria_core::behavior::PresentationFreq>>,
        situation_strength_mean: Option<f64>,
        time_span_days: Option<f64>,
        trait_refs: Option<Vec<String>>,
    }

    #[derive(Deserialize)]
    struct ImportPayload {
        situation: Option<ImportSituation>,
        reaction: Option<String>,
        params: Option<ramaria_core::behavior::BehaviorParams>,
        avoid: Option<Vec<String>>,
    }

    let payload: ImportPayload = serde_json::from_str(json)
        .map_err(|e| RamariaError::validation(format!("规则 JSON 非法: {e}")))?;

    let situation = payload
        .situation
        .ok_or_else(|| RamariaError::validation("规则 JSON 缺少 situation 字段"))?;
    let keywords = situation.keywords.unwrap_or_default();
    if keywords.is_empty() && situation.centroid.is_none() {
        return Err(RamariaError::validation(
            "situation 必须含 keywords 或 centroid（空情境拒绝导入）",
        ));
    }
    // 宽松组装：缺失字段用默认（导入 JSON 无需写全统计字段）
    let situation = BehaviorSituation {
        keywords,
        centroid: situation.centroid,
        response_centroid: situation.response_centroid,
        valence_mean: situation.valence_mean.unwrap_or(0.0),
        valence_std: situation.valence_std.unwrap_or(0.0),
        sample_count: situation.sample_count.unwrap_or(0),
        presentation_dist: situation.presentation_dist.unwrap_or_default(),
        situation_strength_mean: situation.situation_strength_mean.unwrap_or(3.0),
        time_span_days: situation.time_span_days.unwrap_or(0.0),
        trait_refs: situation.trait_refs.unwrap_or_default(),
    };

    let reaction = payload
        .reaction
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    if reaction.is_none() && payload.params.is_none() {
        return Err(RamariaError::validation(
            "reaction 与 params 至少一项（空规则拒绝导入）",
        ));
    }

    let mut rule = BehaviorRule::new(
        persona_uid,
        situation,
        reaction,
        payload.params.unwrap_or_default(),
        RuleSource::Manual,
    );
    rule.avoid = payload.avoid.unwrap_or_default();
    rule.confidence = 1.0; // 手工规则可信度最高（与手工事实同一口径）
    rule.stability = 1.0;

    engine.storage_ref().save_behavior_rule(&rule).await
}

/// 规则证据链（规则 → 事件 → 原文溯源）。
///
/// 返回:
/// - 每条证据的事件摘要（title/summary/paraphrase/keywords），按权重降序。
/// - 事件缺失（已删除）时跳过该条并记 debug（证据链容忍脏引用）。
pub(crate) async fn rule_evidence(
    engine: &Engine,
    id: i64,
) -> RamariaResult<Vec<RuleEvidenceItem>> {
    let rule = engine
        .storage_ref()
        .get_behavior_rule(id)
        .await?
        .ok_or_else(|| RamariaError::validation(format!("行为规则 {} 不存在", id)))?;

    let mut items = Vec::with_capacity(rule.evidence.len());
    for ev in &rule.evidence {
        if let Some(event) = engine.storage_ref().get_event(ev.event_id).await? {
            items.push(RuleEvidenceItem {
                event_id: event.id,
                weight: ev.weight,
                title: event.title,
                summary: event.summary,
                paraphrase: event.paraphrase,
                keywords: event.keywords,
            });
        } else {
            tracing::debug!(event_id = ev.event_id, "规则证据引用的事件已不存在，跳过");
        }
    }
    items.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(items)
}

// =========================================================
// 增量更新（封存钩子，注册式接入不阻塞封存）
// =========================================================

/// 执行一次封存触发的增量更新（会话封存时调用）。
///
/// 说明:
/// - 行为配置关闭 → 直接返回（等同行为层未启用）。
/// - 待定池为内存态（跨会话保存在引擎内），重启后重建为空——
///   未归入事件仍在事件表中，全量重学会重新聚类。
pub(crate) async fn incremental_update(engine: &Engine, persona_uid: &str) -> RamariaResult<()> {
    let config = engine.config();
    if !config.behavior.enabled {
        return Ok(());
    }
    let storage = engine.storage_ref();
    let llm = engine.llm_ref();
    let embedding = engine.embedding_ref();
    let pending = Arc::clone(engine.behavior_pending_ref());
    incremental_update_core(
        storage.as_ref(),
        llm.as_ref(),
        embedding.as_deref(),
        &config.behavior,
        pending.as_ref(),
        persona_uid,
    )
    .await
}

/// 封存钩子核心逻辑（供宿主 / 生命周期钩子闭包共用）。
///
/// 流程:
/// 1. 读取 persona 未吸收事件（本会话新提取）。
/// 2. 读取现有规则 + 待定池。
/// 3. `compute_incremental_update`：归簇 / 待定池推进 / 证据衰减 / 漂移检测。
/// 4. 落库：
///    - 归入规则 → 追加证据（滚动更新）。
///    - 待定池成簇 → 读事件详情 → 规则生成 → 落库（Auto 自动生效）。
///    - 证据衰减失效 → enabled=false（降级，保留审计）。
///    - 漂移触发 → 记 warn（仅告警，全量重学由用户触发）。
pub(crate) async fn incremental_update_core(
    storage: &dyn StorageBackend,
    llm: &dyn LlmProvider,
    embedding: Option<&dyn EmbeddingProvider>,
    config: &BehaviorConfig,
    pending: &Mutex<PendingPool>,
    persona_uid: &str,
) -> RamariaResult<()> {
    ramaria_memory::behavior::orchestrate::incremental_update(
        storage,
        llm,
        embedding,
        config,
        pending,
        persona_uid,
    )
    .await
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
