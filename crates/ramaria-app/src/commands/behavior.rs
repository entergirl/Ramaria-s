//! crates/ramaria-app/src/commands/behavior.rs - 行为层用例（D7 + H1，v1.5 M5）
//!
//! 设计特点:
//! - 学习管线：事件 → 聚类（含 Manual 强锚点）→ 规则生成 → 替换旧 Auto 规则落库
//! - 增量更新：封存时新事件归簇 / 待定池 / 证据衰减 / 漂移检测（落库由本层执行）
//! - 情境路由：读规则 + 查询构造 → 路由决策（供 M6 prompt 注入）
//! - 规则管理：list/show/edit/enable/disable/delete/import/evidence（D7）
//! - 反馈环 H1：edit/disable 写 feedback_log（S1，weight=1.0，detail 编辑快照）；
//!   edit 后规则转为 Manual（强锚点，v3.1 §9.3，簇中心向 Manual 规则偏移）
//! - 手工导入：JSON 校验（非法拒绝），导入规则 source=Manual
//! - 证据链：规则 → 事件 → 原文可溯源（只返回结构化字段，原文不落日志）
//!
//! 安全约束:
//! - 不记录完整用户消息 / 原文 / 完整 prompt
//! - evidence 命令返回事件的 paraphrase/summary 等脱敏字段，不返回原始对话

use ramaria_core::behavior::{
    BehaviorRule, BehaviorSituation, FeedbackLog, RuleSource, SignalType, TargetType,
};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{MemoryEvent, Message, now_ms};

use crate::app::App;

/// 一次学习管线的输出统计。
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

/// 规则证据链的一项（规则 → 事件 → 原文溯源，v3.1 §9.5）。
#[derive(Debug, Clone, serde::Serialize)]
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
// Manual 强锚点（v3.1 §9.3）
// =========================================================

/// 将启用中的 Manual 规则转为聚类锚点样本（簇中心向 Manual 规则偏移）。
///
/// 说明:
/// - 锚点样本 event_id 用负值标记（非真实事件，不写入规则 evidence）。
/// - salience=1.0（强锚点，比重高于普通事件）。
/// - 向量直接取自规则的簇中心（无需重新 embedding）。
fn manual_anchor_samples(rules: &[BehaviorRule]) -> Vec<ramaria_memory::behavior::BehaviorSample> {
    rules
        .iter()
        .filter(|r| r.source == RuleSource::Manual && r.enabled)
        .map(|r| ramaria_memory::behavior::BehaviorSample {
            event_id: -r.id,
            situation_keywords: r.situation.keywords.clone(),
            situation_vector: r.situation.centroid.clone(),
            reaction_vector: r.situation.response_centroid.clone(),
            valence: r.situation.valence_mean,
            presentation: ramaria_core::types::Presentation::Mixed,
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
/// 4. 逐簇规则生成（质控/极性校验降级链，LLM 不可用自动降级）。
/// 5. 删除该 persona 全部旧 Auto 规则 → 批量保存新规则（Auto 自动生效）。
///
/// 返回:
/// - 学习统计；行为配置关闭时返回空统计（等同 v1.4 行为）。
pub async fn behavior_learn(app: &App, persona_uid: &str) -> RamariaResult<BehaviorLearnOutcome> {
    let mut outcome = BehaviorLearnOutcome::default();
    if !app.config.behavior.enabled {
        tracing::info!("行为层已关闭（[behavior].enabled=false），跳过学习");
        return Ok(outcome);
    }

    let llm = app.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let embedding = app.embedding_provider();
    let config = app.config.behavior.clone();
    let storage = app.storage.as_ref();

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
    let mut samples: Vec<ramaria_memory::behavior::BehaviorSample> = events
        .iter()
        .map(ramaria_memory::behavior::sample_from_event)
        .collect();
    samples.extend(manual_anchor_samples(&manual_rules));
    let clusterer = ramaria_memory::behavior::BehaviorClusterer::new(&config, embedding.as_deref());
    let clusters = clusterer.cluster_samples(&events, &mut samples).await?;
    outcome.cluster_count = clusters.len();
    if clusters.is_empty() {
        return Ok(outcome);
    }

    // 4. 规则生成
    let generator = ramaria_memory::behavior::BehaviorRuleGenerator::new(
        ramaria_memory::behavior::RuleGenConfig::from(&config),
        llm.as_ref(),
    );
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
// 情境路由
// =========================================================

/// 情境路由（对话时）：读规则 + 查询构造 → 路由决策。
///
/// 说明:
/// - 仅启用中的规则参与路由；全低于阈值 → 静默降级（matched=false，等同 v1.4）。
/// - 返回结果由 M6（F 任务）注入 prompt 行为块。
pub async fn behavior_route(
    app: &App,
    persona_uid: &str,
    messages: &[Message],
) -> RamariaResult<ramaria_memory::behavior::RoutingResult> {
    if !app.config.behavior.enabled || messages.is_empty() {
        return Ok(ramaria_memory::behavior::RoutingResult {
            matched: false,
            primary: None,
            secondary: Vec::new(),
        });
    }
    let rules = app
        .storage
        .list_behavior_rules_by_persona(persona_uid)
        .await?;
    let embedding = app.embedding_provider();
    let config = app.config.behavior.clone();

    let query =
        ramaria_memory::behavior::build_query_context(messages, embedding.as_deref()).await?;
    let params = ramaria_memory::behavior::RoutingParams::from(&config);
    Ok(ramaria_memory::behavior::route_rules(
        &rules, &query, &params,
    ))
}

// =========================================================
// 规则管理（D7）+ 反馈环 S1（H1）
// =========================================================

/// 列出 persona 的全部规则（含禁用项）。
pub async fn behavior_list_rules(app: &App, persona_uid: &str) -> RamariaResult<Vec<BehaviorRule>> {
    app.storage
        .list_behavior_rules_by_persona(persona_uid)
        .await
}

/// 查看单条规则。
pub async fn behavior_get_rule(app: &App, id: i64) -> RamariaResult<Option<BehaviorRule>> {
    app.storage.get_behavior_rule(id).await
}

/// 编辑规则（H1 S1：edit 写 feedback_log + 规则转为 Manual 强锚点）。
///
/// 参数:
/// - `rule`: 编辑后的完整规则（id 定位，reaction/params/avoid/situation 全量覆盖）。
/// - `session_id`: 干预发生的会话（可选，审计关联）。
///
/// 说明:
/// - 编辑即显式干预（S1 强信号）→ 规则 source 转为 Manual（优先级高于 Auto，
///   后续学习作为聚类强锚点）。
/// - feedback_log 记录编辑前后快照（只存规则字段 JSON，不含原文）。
pub async fn behavior_edit_rule(
    app: &App,
    rule: &mut BehaviorRule,
    session_id: Option<&str>,
) -> RamariaResult<()> {
    let storage = app.storage.as_ref();
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

/// 启用/禁用规则（H1 S1：disable 写 feedback_log；enable 不写——非干预）。
pub async fn behavior_set_rule_enabled(
    app: &App,
    id: i64,
    enabled: bool,
    session_id: Option<&str>,
) -> RamariaResult<()> {
    let storage = app.storage.as_ref();
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

/// 删除规则（破坏性操作，调用方负责确认）。
pub async fn behavior_delete_rule(app: &App, id: i64) -> RamariaResult<()> {
    app.storage.delete_behavior_rule(id).await
}

/// 手工导入规则（D7，JSON 校验）。
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
pub async fn behavior_import_rule(app: &App, persona_uid: &str, json: &str) -> RamariaResult<i64> {
    #[derive(serde::Deserialize)]
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

    #[derive(serde::Deserialize)]
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
    rule.confidence = 1.0; // 手工规则可信度最高（v3.1 §5 手工事实口径一致）
    rule.stability = 1.0;

    app.storage.save_behavior_rule(&rule).await
}

/// 规则证据链（规则 → 事件 → 原文溯源）。
///
/// 返回:
/// - 每条证据的事件摘要（title/summary/paraphrase/keywords），按权重降序。
/// - 事件缺失（已删除）时跳过该条并记 debug（证据链容忍脏引用）。
pub async fn behavior_rule_evidence(app: &App, id: i64) -> RamariaResult<Vec<RuleEvidenceItem>> {
    let rule = app
        .storage
        .get_behavior_rule(id)
        .await?
        .ok_or_else(|| RamariaError::validation(format!("行为规则 {} 不存在", id)))?;

    let mut items = Vec::with_capacity(rule.evidence.len());
    for ev in &rule.evidence {
        if let Some(event) = app.storage.get_event(ev.event_id).await? {
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
// 增量更新（D6：封存钩子，注册式接入不阻塞封存）
// =========================================================

/// 执行一次封存触发的增量更新（会话封存时调用）。
///
/// 说明:
/// - 行为配置关闭 → 直接返回（等同 v1.4 行为）。
/// - 待定池为内存态（跨会话保存在 App 内），重启后重建为空——
///   未归入事件仍在事件表中，全量重学会重新聚类（见完成记录注记）。
pub async fn behavior_incremental_update(app: &App, persona_uid: &str) -> RamariaResult<()> {
    if !app.config.behavior.enabled {
        return Ok(());
    }
    let storage = app.storage.clone();
    let llm = app.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let embedding = app.embedding_provider();
    let config = app.config.behavior.clone();
    let pending = app.behavior_pending.clone();
    behavior_incremental_update_core(
        storage.as_ref(),
        llm.as_ref(),
        embedding.as_deref(),
        &config,
        &pending,
        persona_uid,
    )
    .await
}

/// 封存钩子核心逻辑（供 App 方法 / 生命周期钩子闭包共用）。
///
/// 流程:
/// 1. 读取 persona 未吸收事件（本会话新提取）。
/// 2. 读取现有规则 + 待定池。
/// 3. `compute_incremental_update`：归簇 / 待定池推进 / 证据衰减 / 漂移检测。
/// 4. 落库：
///    - 归入规则 → 追加证据（滚动更新）。
///    - 待定池成簇 → 读事件详情 → 规则生成 → 落库（Auto 自动生效）。
///    - 证据衰减失效 → enabled=false（降级，保留审计）。
///    - 漂移触发 → 记 warn（v1.5 仅告警，全量重学由用户触发）。
pub async fn behavior_incremental_update_core(
    storage: &dyn StorageBackend,
    llm: &dyn ramaria_core::traits::LlmProvider,
    embedding: Option<&dyn ramaria_core::traits::EmbeddingProvider>,
    config: &ramaria_core::config::BehaviorConfig,
    pending: &std::sync::Mutex<ramaria_memory::behavior::PendingPool>,
    persona_uid: &str,
) -> RamariaResult<()> {
    // 1. 未吸收事件（本会话新提取）
    let new_events = storage.list_unabsorbed_events(persona_uid).await?;
    if new_events.is_empty() {
        return Ok(());
    }

    // 2. 现有规则 + 待定池（克隆进出锁，避免 MutexGuard 跨 await）
    let mut rules = storage.list_behavior_rules_by_persona(persona_uid).await?;
    let mut pool = pending.lock().unwrap_or_else(|e| e.into_inner()).clone();

    // 3. 计算增量更新指令
    let outcome = ramaria_memory::behavior::compute_incremental_update(
        &new_events,
        &mut rules,
        &mut pool,
        config,
        embedding,
        now_ms(),
    )
    .await?;

    // 计算完成后写回待定池（跨 await 期间锁已释放）
    *pending.lock().unwrap_or_else(|e| e.into_inner()) = pool;

    // 4a. 归入规则 → 追加证据
    if !outcome.assigned.is_empty() {
        // 归簇后"滚动更新簇统计 → 规则参数微调"：v1.5 做证据追加 + updated_at 刷新
        // （完整参数重算留待全量重学，见完成记录）
        let by_rule: std::collections::HashMap<i64, Vec<i64>> = outcome.assigned.iter().fold(
            std::collections::HashMap::new(),
            |mut m, &(event_id, rule_id)| {
                m.entry(rule_id).or_default().push(event_id);
                m
            },
        );
        for (rule_id, event_ids) in by_rule {
            if let Some(rule) = rules.iter_mut().find(|r| r.id == rule_id) {
                for eid in event_ids {
                    rule.evidence
                        .push(ramaria_core::behavior::BehaviorEvidence {
                            event_id: eid,
                            weight: 0.5,
                        });
                }
                rule.updated_at = now_ms();
                storage.update_behavior_rule(rule).await?;
            }
        }
    }

    // 4b. 待定池成簇 → 生成新规则
    if !outcome.new_cluster_event_ids.is_empty() {
        for group in &outcome.new_cluster_event_ids {
            // 读事件详情 → 样本 → 簇提炼 → 规则生成
            let mut events: Vec<MemoryEvent> = Vec::new();
            for &eid in group {
                if let Some(ev) = storage.get_event(eid).await? {
                    events.push(ev);
                }
            }
            if events.is_empty() {
                continue;
            }
            let mut samples: Vec<ramaria_memory::behavior::BehaviorSample> = events
                .iter()
                .map(ramaria_memory::behavior::sample_from_event)
                .collect();
            let clusterer = ramaria_memory::behavior::BehaviorClusterer::new(config, embedding);
            let clusters = clusterer.cluster_samples(&events, &mut samples).await?;
            for cluster in clusters {
                let generator = ramaria_memory::behavior::BehaviorRuleGenerator::new(
                    ramaria_memory::behavior::RuleGenConfig::from(config),
                    llm,
                );
                let generated = generator.generate_rule(&cluster).await;
                let mut rule = generated.rule;
                rule.persona_uid = persona_uid.to_string();
                rule.evidence.retain(|e| e.event_id > 0);
                if rule.evidence.is_empty() && rule.has_reaction() {
                    tracing::warn!("待定池成簇无真实证据，跳过规则生成");
                    continue;
                }
                storage.save_behavior_rule(&rule).await?;
                tracing::info!(
                    rule_id = rule.id,
                    reaction = rule.has_reaction(),
                    "待定池成簇生成新行为规则"
                );
            }
        }
    }

    // 4c. 证据衰减失效 → 降级（enabled=false，不删除——保留审计）
    if !outcome.decayed_rule_ids.is_empty() {
        for &rule_id in &outcome.decayed_rule_ids {
            if let Some(rule) = rules.iter_mut().find(|r| r.id == rule_id) {
                // 衰减后的证据权重已由 compute_incremental_update 原地修改
                rule.enabled = false;
                rule.updated_at = now_ms();
                storage.update_behavior_rule(rule).await?;
                tracing::warn!(
                    rule_id,
                    "行为规则证据衰减低于阈值，已降级为禁用（保留审计）"
                );
            }
        }
    }

    // 4d. 漂移检测 → 告警（v1.5 仅日志；规则重构由全量重学承担）
    if outcome.drift_triggered {
        tracing::warn!(
            persona_uid,
            "检测到反应模式系统性漂移，建议执行行为规则全量重学（behavior learn）"
        );
    }

    Ok(())
}
