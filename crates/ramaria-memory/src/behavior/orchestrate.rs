//! crates/ramaria-memory/src/behavior/orchestrate.rs - 行为层增量编排与在线路由模块
//!
//! 设计特点:
//! - 封存钩子的行为层增量编排 + 在线情境路由（与传输无关，供 app 与 service 共用）
//! - 增量管线：未吸收事件 → 增量更新指令 → 落库（归簇证据追加 / 待定池成簇
//!   规则生成 / 证据衰减降级 / 漂移告警）
//! - 在线路由：读规则 + 查询构造 → 路由决策；关键词池词典装载失败/为空时
//!   退化为纯 bigram 词频（静默降级，不阻塞）
//! - 待定池为内存态（跨会话保存在调用方），以 `Mutex` 注入，克隆进出锁避免持锁跨 await
//! - LLM 与 embedding 均经 trait 注入，不可用时静默降级（不阻塞封存主流程）
//! - 不记录任何对话原文；规则生成只处理 paraphrase 与结构化字段

use ramaria_core::behavior::BehaviorEvidence;
use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::{MemoryEvent, Message, now_ms};

use super::{
    BehaviorClusterer, BehaviorRuleGenerator, BehaviorSample, PendingPool, QueryKeywordNormalizer,
    RoutingParams, RoutingResult, RuleGenConfig, build_query_context_with_normalizer,
    compute_incremental_update, route_rules, sample_from_event,
};

/// 封存钩子的行为层增量更新编排（与传输无关，供 app / service 共用）。
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
pub async fn incremental_update(
    storage: &dyn StorageBackend,
    llm: &dyn LlmProvider,
    embedding: Option<&dyn EmbeddingProvider>,
    config: &BehaviorConfig,
    pending: &std::sync::Mutex<PendingPool>,
    persona_uid: &str,
) -> RamariaResult<()> {
    // 1. 未吸收事件（本会话新提取）
    let new_events = storage.list_unabsorbed_events(persona_uid).await?;
    if new_events.is_empty() {
        return Ok(());
    }

    // 2. 现有规则 + 待定池（克隆进出锁，避免 MutexGuard 跨 await）
    let mut rules = storage.list_behavior_rules_by_persona(persona_uid).await?;
    let mut pool = lock_recover(pending, "commands_behavior.pending").clone();

    // 3. 计算增量更新指令
    let outcome = compute_incremental_update(
        &new_events,
        &mut rules,
        &mut pool,
        config,
        embedding,
        now_ms(),
    )
    .await?;

    // 计算完成后写回待定池（跨 await 期间锁已释放）
    *lock_recover(pending, "commands_behavior.pending") = pool;

    // 4a. 归入规则 → 追加证据
    if !outcome.assigned.is_empty() {
        // 归簇后"滚动更新簇统计 → 规则参数微调"：证据追加 + updated_at 刷新
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
                    rule.evidence.push(BehaviorEvidence {
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
            let mut samples: Vec<BehaviorSample> = events.iter().map(sample_from_event).collect();
            let clusterer = BehaviorClusterer::new(config, embedding);
            let clusters = clusterer.cluster_samples(&events, &mut samples).await?;
            for cluster in clusters {
                let generator = BehaviorRuleGenerator::new(RuleGenConfig::from(config), llm);
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

    // 4d. 漂移检测 → 告警（仅日志；规则重构由全量重学承担）
    if outcome.drift_triggered {
        tracing::warn!(
            persona_uid,
            "检测到反应模式系统性漂移，建议执行行为规则全量重学（behavior learn）"
        );
    }

    Ok(())
}

// =========================================================
// 在线情境路由
// =========================================================

/// 情境路由（在线对话）：读规则 + 查询构造 → 路由决策（与传输无关，供 app / service 共用）。
///
/// 流程:
/// 1. 行为层关闭 / 消息为空 → 静默降级（matched=false，不注入行为块）。
/// 2. 读取 persona 全部规则（仅启用中的规则参与评分，由 `route_rules` 过滤）。
/// 3. 查询侧关键词规范化：词典从 keyword_pool 词条装载（与关键词服务镜像同源）；
///    装载失败 / 词表为空 → 纯 bigram 词频（零 embedding，行为等价）。
/// 4. `build_query_context_with_normalizer` 构造查询 → `route_rules` 产出路由决策。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `config`: 行为层配置（总开关 / 阈值 / Top-N）。
/// - `embedding`: 嵌入模型 provider（None → 纯关键词降级）。
/// - `persona_uid`: 人格 UID。
/// - `messages`: 当前会话消息（查询构造取最近窗口）。
///
/// 返回:
/// - `Ok(RoutingResult)`: 命中决策或静默降级结果；调用方据此决定是否注入行为块。
/// - `Err`: 规则读取 / 查询向量化失败（调用方记录 warn 并降级，不阻塞对话）。
///
/// 安全约束:
/// - 查询文本仅为内存中转，不落日志、不落库。
pub async fn route(
    storage: &dyn StorageBackend,
    config: &BehaviorConfig,
    embedding: Option<&dyn EmbeddingProvider>,
    persona_uid: &str,
    messages: &[Message],
) -> RamariaResult<RoutingResult> {
    if !config.enabled || messages.is_empty() {
        return Ok(RoutingResult {
            matched: false,
            primary: None,
            secondary: Vec::new(),
        });
    }
    let rules = storage.list_behavior_rules_by_persona(persona_uid).await?;

    // 查询侧关键词规范化（关键词池别名归一 → 口语说法更易命中事件关键词）：
    // 词表从 keyword_pool 装载后使用（无 std 锁跨 await）；装载失败 / 词表为空
    // 时退化为纯 bigram 词频（零 embedding，行为等价，不阻塞路由）。
    let normalizer = match storage.list_keyword_pool_entries().await {
        Ok(rows) => {
            let mut service = crate::keyword::KeywordService::new();
            service.load_pool_entries(&rows);
            QueryKeywordNormalizer::from_pool(service.pool())
        }
        Err(e) => {
            tracing::warn!(error = %e, "加载 keyword_pool 词条失败，路由查询退化为纯 bigram 词频");
            QueryKeywordNormalizer::empty()
        }
    };
    let query = build_query_context_with_normalizer(messages, embedding, &normalizer).await?;
    let params = RoutingParams::from(config);
    Ok(route_rules(&rules, &query, &params))
}
