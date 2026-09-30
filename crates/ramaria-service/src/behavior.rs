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
mod tests {
    use super::*;
    use crate::test_support::{MockLlm, engine_with_db, engine_with_llm_and_config, seed_persona};
    use ramaria_core::behavior::{BehaviorEvidence, BehaviorParams};
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use ramaria_core::types::MemoryEvent;
    use ramaria_storage::SqliteStorage;

    /// 构造一条 Auto 规则（测试造数）。
    fn auto_rule(persona: &str, reaction: &str) -> BehaviorRule {
        BehaviorRule::new(
            persona,
            BehaviorSituation::empty(),
            Some(reaction.to_string()),
            BehaviorParams::default(),
            RuleSource::Auto,
        )
    }

    /// 造一条事件（测试造数，载荷带脱敏字段供证据链断言）。
    async fn seed_event(storage: &SqliteStorage, persona: &str, title: &str) -> i64 {
        let mut event = MemoryEvent::new(
            persona.to_string(),
            title.to_string(),
            format!("{title}的摘要说明"),
            1_000,
            2_000,
        );
        event.paraphrase = Some(format!("{title}的态度重述"));
        event.keywords = Some("测试,证据".to_string());
        storage.save_event(&event).await.expect("写入事件应成功")
    }

    /// 列表与启停：禁用写 S1 反馈日志（weight=1.0），启用不写（非干预）。
    #[tokio::test]
    async fn list_and_toggle_writes_feedback_on_disable() {
        let (engine, storage, dir) = engine_with_db("behavior-toggle").await;
        seed_persona(&storage, "char-0001").await;
        let rule_id = storage
            .save_behavior_rule(&auto_rule("char-0001", "先自嘲一句再聊具体事"))
            .await
            .expect("写入规则应成功");

        let rules = engine
            .behavior_list_rules("char-0001")
            .await
            .expect("规则列表应成功");
        assert_eq!(rules.len(), 1, "应列出刚写入的规则");
        assert_eq!(rules[0].id, rule_id);
        assert!(rules[0].enabled, "新规则默认启用");

        engine
            .behavior_set_rule_enabled(rule_id, false, None)
            .await
            .expect("禁用应成功");
        let disabled = storage
            .get_behavior_rule(rule_id)
            .await
            .expect("查询规则应成功")
            .expect("规则应存在");
        assert!(!disabled.enabled, "禁用后 enabled=false");

        let logs = storage
            .list_feedback_logs_by_persona("char-0001")
            .await
            .expect("查询反馈日志应成功");
        assert_eq!(logs.len(), 1, "禁用应写一条反馈日志");
        assert_eq!(logs[0].signal_type, SignalType::Disable);
        assert_eq!(logs[0].target_type, TargetType::BehaviorRule);
        assert_eq!(logs[0].target_id, rule_id.to_string());
        assert_eq!(logs[0].weight, 1.0, "S1 强信号 weight=1.0");

        engine
            .behavior_set_rule_enabled(rule_id, true, None)
            .await
            .expect("启用应成功");
        let logs = storage
            .list_feedback_logs_by_persona("char-0001")
            .await
            .expect("查询反馈日志应成功");
        assert_eq!(logs.len(), 1, "启用非干预信号，不写反馈日志");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 编辑：转 Manual 强锚点并写编辑前后快照反馈（detail 不含原文）。
    #[tokio::test]
    async fn edit_converts_to_manual_and_writes_snapshot() {
        let (engine, storage, dir) = engine_with_db("behavior-edit").await;
        seed_persona(&storage, "char-0001").await;
        let rule_id = storage
            .save_behavior_rule(&auto_rule("char-0001", "原规则文本"))
            .await
            .expect("写入规则应成功");

        let mut rule = engine
            .behavior_get_rule(rule_id)
            .await
            .expect("查询规则应成功")
            .expect("规则应存在");
        rule.reaction = Some("改为先倾听再回应".to_string());
        engine
            .behavior_edit_rule(&mut rule, Some("sess-1"))
            .await
            .expect("编辑应成功");

        let stored = storage
            .get_behavior_rule(rule_id)
            .await
            .expect("查询规则应成功")
            .expect("规则应存在");
        assert_eq!(stored.source, RuleSource::Manual, "编辑后应转为 Manual");
        assert_eq!(stored.reaction.as_deref(), Some("改为先倾听再回应"));

        let logs = storage
            .list_feedback_logs_by_persona("char-0001")
            .await
            .expect("查询反馈日志应成功");
        assert_eq!(logs.len(), 1, "编辑应写一条反馈日志");
        assert_eq!(logs[0].signal_type, SignalType::Edit);
        assert_eq!(logs[0].session_id.as_deref(), Some("sess-1"));
        let detail: serde_json::Value =
            serde_json::from_str(logs[0].detail.as_deref().expect("编辑反馈应含快照"))
                .expect("快照应为合法 JSON");
        assert_eq!(detail["before"]["reaction"], "原规则文本");
        assert_eq!(detail["after"]["reaction"], "改为先倾听再回应");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 删除：规则从库中消失（get 返回 None）。
    #[tokio::test]
    async fn delete_removes_rule() {
        let (engine, storage, dir) = engine_with_db("behavior-delete").await;
        seed_persona(&storage, "char-0001").await;
        let rule_id = storage
            .save_behavior_rule(&auto_rule("char-0001", "待删除规则"))
            .await
            .expect("写入规则应成功");

        engine
            .behavior_delete_rule(rule_id)
            .await
            .expect("删除应成功");
        assert!(
            engine
                .behavior_get_rule(rule_id)
                .await
                .expect("查询规则应成功")
                .is_none(),
            "删除后规则应不存在"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 导入校验：非法 JSON / 缺 situation / 空情境 / 空规则均拒绝，且不落库。
    #[tokio::test]
    async fn import_rejects_invalid_payloads() {
        let (engine, storage, dir) = engine_with_db("behavior-import-reject").await;
        seed_persona(&storage, "char-0001").await;

        let err = engine
            .behavior_import_rule("char-0001", "{ not json")
            .await
            .expect_err("非法 JSON 应拒绝");
        assert!(
            err.to_string().contains("规则 JSON 非法"),
            "错误应指明 JSON 非法: {err}"
        );

        let err = engine
            .behavior_import_rule("char-0001", r#"{"reaction": "没有情境"}"#)
            .await
            .expect_err("缺 situation 应拒绝");
        assert!(
            err.to_string().contains("缺少 situation"),
            "错误应指明缺 situation: {err}"
        );

        let err = engine
            .behavior_import_rule("char-0001", r#"{"situation": {}, "reaction": "回应"}"#)
            .await
            .expect_err("空情境应拒绝");
        assert!(
            err.to_string().contains("空情境拒绝导入"),
            "错误应指明空情境: {err}"
        );

        let err = engine
            .behavior_import_rule("char-0001", r#"{"situation": {"keywords": ["难过"]}}"#)
            .await
            .expect_err("空规则应拒绝");
        assert!(
            err.to_string().contains("至少一项"),
            "错误应指明空规则: {err}"
        );

        let rules = engine
            .behavior_list_rules("char-0001")
            .await
            .expect("规则列表应成功");
        assert!(rules.is_empty(), "拒绝导入不应落库");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 导入成功：宽松 JSON 落库为 Manual 规则（confidence / stability=1.0）。
    #[tokio::test]
    async fn import_success_persists_manual_rule() {
        let (engine, storage, dir) = engine_with_db("behavior-import-ok").await;
        seed_persona(&storage, "char-0001").await;

        let id = engine
            .behavior_import_rule(
                "char-0001",
                r#"{
                    "situation": {"keywords": ["难过", "低落"]},
                    "reaction": "用轻快的语气回应",
                    "avoid": ["说教"]
                }"#,
            )
            .await
            .expect("合法 JSON 应导入成功");

        let rule = engine
            .behavior_get_rule(id)
            .await
            .expect("查询规则应成功")
            .expect("导入的规则应存在");
        assert_eq!(rule.persona_uid, "char-0001");
        assert_eq!(rule.source, RuleSource::Manual);
        assert!(rule.enabled, "手工导入自动生效");
        assert_eq!(rule.reaction.as_deref(), Some("用轻快的语气回应"));
        assert_eq!(rule.avoid, vec!["说教"]);
        assert_eq!(rule.situation.keywords, vec!["难过", "低落"]);
        assert_eq!(rule.confidence, 1.0);
        assert_eq!(rule.stability, 1.0);
        // 宽松默认：缺失统计字段用默认值组装
        assert_eq!(rule.situation.valence_mean, 0.0);
        assert_eq!(rule.situation.situation_strength_mean, 3.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 证据链：权重降序；脏引用（事件已不存在）跳过。
    #[tokio::test]
    async fn rule_evidence_orders_by_weight_desc_and_skips_dangling() {
        let (engine, storage, dir) = engine_with_db("behavior-evidence").await;
        seed_persona(&storage, "char-0001").await;

        let first = seed_event(&storage, "char-0001", "事件一").await;
        let second = seed_event(&storage, "char-0001", "事件二").await;

        let mut rule = auto_rule("char-0001", "携带证据的规则");
        rule.evidence = vec![
            BehaviorEvidence {
                event_id: first,
                weight: 0.3,
            },
            // 脏引用：事件不存在，应跳过
            BehaviorEvidence {
                event_id: 99_999,
                weight: 0.99,
            },
            BehaviorEvidence {
                event_id: second,
                weight: 0.9,
            },
        ];
        let rule_id = storage
            .save_behavior_rule(&rule)
            .await
            .expect("写入规则应成功");

        let items = engine
            .behavior_rule_evidence(rule_id)
            .await
            .expect("证据链应成功");
        assert_eq!(items.len(), 2, "脏引用应跳过");
        assert_eq!(items[0].weight, 0.9, "权重降序");
        assert_eq!(items[0].event_id, second);
        assert_eq!(items[0].title, "事件二");
        assert_eq!(
            items[0].paraphrase.as_deref(),
            Some("事件二的态度重述"),
            "证据项应携带脱敏态度字段"
        );
        assert_eq!(items[1].weight, 0.3);
        assert_eq!(items[1].event_id, first);
        assert_eq!(items[1].title, "事件一");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 学习：`[behavior].enabled=false` 时返回空统计（不读取事件、不替换规则）。
    #[tokio::test]
    async fn learn_with_behavior_disabled_returns_empty_outcome() {
        let mut config = RamariaConfig::default();
        config.behavior.enabled = false;
        let (engine, storage, dir) =
            engine_with_llm_and_config("behavior-learn-off", MockLlm::local(), config).await;
        seed_persona(&storage, "char-0001").await;
        // 库内有事件与规则：关闭时学习应直接跳过（统计保持空）
        seed_event(&storage, "char-0001", "既有事件").await;
        storage
            .save_behavior_rule(&auto_rule("char-0001", "既有规则"))
            .await
            .expect("写入规则应成功");

        let outcome = engine
            .behavior_learn("char-0001")
            .await
            .expect("关闭时学习应成功返回");
        assert_eq!(outcome.event_count, 0, "关闭时不应读取事件");
        assert_eq!(outcome.cluster_count, 0);
        assert_eq!(outcome.full_rule_count, 0);
        assert_eq!(outcome.candidate_rule_count, 0);
        assert_eq!(outcome.replaced_rule_count, 0, "关闭时不应替换旧 Auto 规则");

        let rules = engine
            .behavior_list_rules("char-0001")
            .await
            .expect("规则列表应成功");
        assert_eq!(rules.len(), 1, "既有规则应保持原状");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 学习：无事件 → 返回空统计（不报错，手动补跑入口幂等）。
    #[tokio::test]
    async fn learn_without_events_returns_empty_outcome() {
        let (engine, storage, dir) = engine_with_db("behavior-learn-empty").await;
        seed_persona(&storage, "char-0001").await;

        let outcome = engine
            .behavior_learn("char-0001")
            .await
            .expect("无事件时学习应成功返回");
        assert_eq!(outcome.event_count, 0);
        assert_eq!(outcome.cluster_count, 0);
        assert_eq!(outcome.replaced_rule_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 增量更新门面：`[behavior].enabled=false` 时直接返回（不触达存储）。
    #[tokio::test]
    async fn incremental_update_disabled_returns_ok() {
        let mut config = RamariaConfig::default();
        config.behavior.enabled = false;
        let (engine, _storage, dir) =
            engine_with_llm_and_config("behavior-incr-off", MockLlm::local(), config).await;

        engine
            .behavior_incremental_update("char-0001")
            .await
            .expect("关闭时应静默返回");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
