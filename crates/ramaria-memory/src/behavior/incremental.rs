//! crates/ramaria-memory/src/behavior/incremental.rs - 行为规则增量更新（D6，v3.1 §4.4）
//!
//! 设计特点:
//! - 归簇：会话封存的新事件与各规则情境中心相似度 ≥ θ_join → 归入最近簇
//! - 待定池：未归入事件入池积累；≥ min_cluster_size 且内聚（两两 sim ≥ θ_join）→ 成簇
//!   （可生成新规则）；超过过期天数（默认 30 天）未成簇 → 低置信标记（不参与规则生成）
//! - 证据衰减：旧规则证据权重按 Ebbinghaus 衰减，总权重低于阈值 → 降级/失效
//! - 系统性变化：复用 §3.4 漂移检测（Wasserstein + 置换检验）判断反应模式是否漂移 → 规则重构
//! - 全部为纯计算（零 I/O），落库编排由 app 层执行（本模块输出"更新指令"）
//!
//! 安全约束:
//! - 只处理事件 id / 结构化字段，不记录任何原文

use ramaria_core::behavior::{BehaviorRule, RuleSource};
use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::now_ms;

use super::clustering::{
    BehaviorSample, cosine_clipped, fused_similarity, jaccard, sample_from_event, vectorize,
};
use crate::decay::calc_decay_weight;
use crate::inference::drift::{DriftConfig, detect_dimension_drift};

/// 待定池事件过期天数默认值（30 天）。
pub const PENDING_EXPIRE_DAYS_DEFAULT: u32 = 30;

// =========================================================
// 归簇（新事件 → 最近簇）
// =========================================================

/// 样本与规则的情境侧相似度（归簇用，v3.1 §4.2 Step 2.5）。
///
/// 公式:
/// - cos(样本情境向量, 规则情境中心) × β2 + Jaccard(样本关键词, 规则关键词) × (1−β2)
/// - 任一侧缺向量 → 纯关键词 Jaccard（embedding 不可用降级）。
///
/// 说明:
/// - 归簇比较的是**情境**（不带反应通道，避免情绪信号污染情境判定）。
pub fn sample_rule_similarity(sample: &BehaviorSample, rule: &BehaviorRule, beta2: f64) -> f64 {
    let beta2 = beta2.clamp(0.0, 1.0);
    let jac = jaccard(&sample.situation_keywords, &rule.situation.keywords);
    match (&sample.situation_vector, &rule.situation.centroid) {
        (Some(sv), Some(c)) => {
            let cos_s = cosine_clipped(sv, c).max(0.0);
            beta2 * cos_s + (1.0 - beta2) * jac
        }
        _ => jac,
    }
}

/// 将新事件归入最近簇（v3.1 §4.4）。
///
/// 参数:
/// - `sample`: 新事件样本。
/// - `rules`: 现有规则（含禁用项——禁用规则不吸收新事件）。
/// - `theta_join`: 归簇阈值（默认 0.7）。
///
/// 返回:
/// - `Some(rule_idx)`: 归入规则索引（相似度最高且 ≥ θ_join）。
/// - `None`: 未归入任何簇（进入待定池）。
pub fn assign_event_to_cluster(
    sample: &BehaviorSample,
    rules: &[BehaviorRule],
    theta_join: f64,
    beta2: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (idx, rule) in rules.iter().enumerate() {
        if !rule.enabled || rule.source != RuleSource::Auto {
            // 禁用规则与手工规则不作为归簇目标（手工规则是用户锚点，不自动吸收）
            continue;
        }
        let sim = sample_rule_similarity(sample, rule, beta2);
        if sim >= theta_join && best.map(|(_, s)| sim > s).unwrap_or(true) {
            best = Some((idx, sim));
        }
    }
    best.map(|(idx, _)| idx)
}

// =========================================================
// 待定池
// =========================================================

/// 待定池中的单条事件。
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEvent {
    /// 事件 id
    pub event_id: i64,
    /// 情境样本（关键词/向量/valence/salience 等）
    pub sample: BehaviorSample,
    /// 入池时间（Unix 毫秒）
    pub added_at_ms: i64,
    /// 低置信标记（超过过期天数未成簇 → true，不参与规则生成）
    pub low_confidence: bool,
}
impl PendingEvent {
    /// 从事件创建待定项。
    pub fn from_event(event: &ramaria_core::types::MemoryEvent, now: i64) -> Self {
        Self {
            event_id: event.id,
            sample: sample_from_event(event),
            added_at_ms: now,
            low_confidence: false,
        }
    }
}

/// 待定池（未归入簇的事件积累区，v3.1 §4.2 Step 2.5）。
///
/// 职责:
/// - 积累未归入事件；每次封存时推进：
///   - 内聚且 ≥ min_cluster_size → 成簇（供生成新规则）。
///   - 超过过期天数未成簇 → 低置信标记。
#[derive(Debug, Clone)]
pub struct PendingPool {
    /// 待定事件
    pub events: Vec<PendingEvent>,
    /// 成簇所需最小事件数（默认 3）
    pub min_cluster_size: usize,
    /// 内聚阈值（两两 sim ≥ θ_join，默认 0.7）
    pub theta_join: f64,
    /// 过期天数（默认 30）
    pub expire_days: u32,
    /// 双通道融合权重（成簇内聚度计算用）
    pub beta1: f64,
    pub beta2: f64,
}

impl PendingPool {
    /// 创建待定池（参数从 `BehaviorConfig` 派生）。
    pub fn new(config: &BehaviorConfig) -> Self {
        Self {
            events: Vec::new(),
            min_cluster_size: config.min_cluster_size,
            theta_join: config.theta_join,
            expire_days: config.pending_expire_days,
            beta1: config.beta1,
            beta2: config.beta2,
        }
    }

    /// 加入一条待定事件（入池时间取当前时钟）。
    ///
    /// 说明:
    /// - 委托 `add_with_time` 实现；需要确定性时间的调用方（测试/回放）
    ///   可直接使用 `add_with_time` 显式注入时间。
    pub fn add(&mut self, event: &ramaria_core::types::MemoryEvent) {
        self.add_with_time(event, now_ms());
    }

    /// 加入一条待定事件，并显式指定入池时间（Unix 毫秒）。
    ///
    /// 参数:
    /// - `event`: 待定事件。
    /// - `now_ms`: 入池时间；`advance` 的过期判定以该时间为起点。
    pub fn add_with_time(&mut self, event: &ramaria_core::types::MemoryEvent, now_ms: i64) {
        self.events.push(PendingEvent::from_event(event, now_ms));
    }

    /// 推进待定池（每次会话封存后调用）。
    ///
    /// 流程:
    /// 1. 对未标记事件做内聚分组（两两 fused sim ≥ θ_join 的连通组）。
    /// 2. 组大小 ≥ min_cluster_size → 成簇（返回事件 id 组，供上层生成新规则）。
    /// 3. 未成簇且入池超过 expire_days → 低置信标记（不参与规则生成）。
    ///
    /// 返回:
    /// - `(成簇事件 id 组, 新标记低置信的事件 id 列表)`。
    pub fn advance(&mut self, now: i64) -> (Vec<Vec<i64>>, Vec<i64>) {
        let mut formed: Vec<Vec<i64>> = Vec::new();
        let mut newly_low: Vec<i64> = Vec::new();

        // 内聚分组：贪心扫描，把互相 sim ≥ θ_join 的未标记事件聚组
        let mut assigned: Vec<bool> = vec![false; self.events.len()];
        for i in 0..self.events.len() {
            if assigned[i] || self.events[i].low_confidence {
                continue;
            }
            let mut group: Vec<usize> = vec![i];
            assigned[i] = true;
            for (j, is_assigned) in assigned.iter_mut().enumerate().skip(i + 1) {
                if *is_assigned || self.events[j].low_confidence {
                    continue;
                }
                // 与组内全部成员互相 sim ≥ θ_join 才入组
                let cohesive = group.iter().all(|&k| {
                    fused_similarity(
                        &self.events[k].sample,
                        &self.events[j].sample,
                        self.beta1,
                        self.beta2,
                    ) >= self.theta_join
                });
                if cohesive {
                    group.push(j);
                    *is_assigned = true;
                }
            }
            if group.len() >= self.min_cluster_size {
                formed.push(group.iter().map(|&k| self.events[k].event_id).collect());
            }
        }

        // 过期低置信标记（未成簇且超期）
        for ev in self.events.iter_mut() {
            if ev.low_confidence {
                continue;
            }
            let days = (now - ev.added_at_ms).max(0) as f64 / 86_400_000.0;
            if days > self.expire_days as f64 {
                ev.low_confidence = true;
                newly_low.push(ev.event_id);
            }
        }

        (formed, newly_low)
    }
}

// =========================================================
// 证据衰减
// =========================================================

/// 证据衰减（v3.1 §4.4 旧规则证据衰减）。
///
/// 规则:
/// - 每条证据权重按 Ebbinghaus 衰减（`calc_decay_weight`，以规则创建时间为基准）。
/// - 衰减后**总权重**低于 `threshold` → 返回 true（规则降级/失效，由上层处理）。
///
/// 参数:
/// - `rule`: 待衰减规则（原地修改证据权重）。
/// - `now`: 当前时间（Unix 毫秒）。
/// - `decay_stability_s`: 衰减稳定性系数（天）。
/// - `threshold`: 总权重下限（低于 → 失效）。
///
/// 返回:
/// - `true` = 规则证据衰减到应降级/失效。
pub fn decay_evidence_weights(
    rule: &mut BehaviorRule,
    now: i64,
    decay_stability_s: f64,
    threshold: f64,
) -> bool {
    let factor = calc_decay_weight(rule.created_at, now, decay_stability_s);
    for ev in &mut rule.evidence {
        ev.weight *= factor;
    }
    let total: f64 = rule.evidence.iter().map(|e| e.weight).sum();
    total < threshold
}

// =========================================================
// 系统性变化检测（漂移）
// =========================================================

/// 反应模式系统性变化检测（v3.1 §4.4 复用 §3.4 漂移检测）。
///
/// 规则:
/// - 对 valence 维度做 Wasserstein 置换检验（`detect_dimension_drift`），
///   新旧组均值显著漂移 → 返回 true（触发规则重构）。
///
/// 参数:
/// - `historical_valences`: 历史（旧）事件 valence。
/// - `recent_valences`: 近期（新）事件 valence。
/// - `config`: 漂移检测配置（置换次数多时较慢，测试用小值）。
///
/// 返回:
/// - 是否显著漂移。
pub fn detect_reaction_drift(
    historical_valences: &[f64],
    recent_valences: &[f64],
    config: &DriftConfig,
) -> bool {
    let result = detect_dimension_drift(
        "valence",
        historical_valences,
        recent_valences,
        &vec![1.0; historical_valences.len()],
        &vec![1.0; recent_valences.len()],
        config,
    );
    result.is_significant
}

// =========================================================
// 增量更新编排（纯计算，落库由 app 层执行）
// =========================================================

/// 一次封存触发的增量更新结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IncrementalUpdateOutcome {
    /// 归入现有规则的事件 id → 规则 id
    pub assigned: Vec<(i64, i64)>,
    /// 待定池成簇（事件 id 组，可生成新规则）
    pub new_cluster_event_ids: Vec<Vec<i64>>,
    /// 新标记低置信的事件 id
    pub low_confidence_event_ids: Vec<i64>,
    /// 证据衰减到应降级/失效的规则 id
    pub decayed_rule_ids: Vec<i64>,
    /// 是否检测到反应模式漂移（触发规则重构）
    pub drift_triggered: bool,
}

/// 计算一次封存的增量更新指令（纯计算 + 向量化，不落库）。
///
/// 流程:
/// 1. 新事件向量化（embedding 不可用 → 纯关键词归簇）。
/// 2. 逐一归簇（assign_event_to_cluster）→ 归入记录；未归入 → 待定池。
/// 3. 待定池推进（成簇/低置信）。
/// 4. 现有规则证据衰减 → 标记失效。
/// 5. valence 漂移检测 → 标记重构。
///
/// 参数:
/// - `new_events`: 本会话新提取的事件。
/// - `rules`: 现有规则（可变——证据衰减会修改权重；调用方应传克隆或接受原地修改）。
/// - `pending`: 待定池（跨会话状态，可空）。
/// - `config`: 行为层配置。
/// - `embedder`: 嵌入模型 provider（None → 纯关键词降级）。
/// - `now`: 当前时间（Unix 毫秒）。
///
/// 返回:
/// - `IncrementalUpdateOutcome`（供 app 层落库：更新归入规则、生成新规则、失效旧规则）。
pub async fn compute_incremental_update(
    new_events: &[ramaria_core::types::MemoryEvent],
    rules: &mut [BehaviorRule],
    pending: &mut PendingPool,
    config: &BehaviorConfig,
    embedder: Option<&dyn EmbeddingProvider>,
    now: i64,
) -> RamariaResult<IncrementalUpdateOutcome> {
    let mut outcome = IncrementalUpdateOutcome::default();

    // 1. 向量化（embedding 不可用 → 向量全 None，归簇退化为纯关键词）
    let mut samples: Vec<BehaviorSample> = new_events.iter().map(sample_from_event).collect();
    vectorize(&mut samples, new_events, embedder).await?;

    // 2. 归簇
    for (event, sample) in new_events.iter().zip(samples.iter()) {
        match assign_event_to_cluster(sample, rules, config.theta_join, config.beta2) {
            Some(idx) => outcome.assigned.push((event.id, rules[idx].id)),
            None => pending.add(event),
        }
    }

    // 3. 待定池推进
    let (formed, low) = pending.advance(now);
    outcome.new_cluster_event_ids = formed;
    outcome.low_confidence_event_ids = low;

    // 4. 证据衰减
    for rule in rules.iter_mut() {
        if decay_evidence_weights(rule, now, 60.0, config.evidence_decay_threshold) {
            outcome.decayed_rule_ids.push(rule.id);
        }
    }

    // 5. 漂移检测（新旧事件 valence 对比）
    if new_events.len() >= 3 {
        let recent: Vec<f64> = new_events.iter().map(|e| e.valence).collect();
        let historical: Vec<f64> = rules
            .iter()
            .flat_map(|r| std::iter::repeat_n(r.situation.valence_mean, 2))
            .collect();
        if !historical.is_empty() {
            let drift_cfg = DriftConfig {
                alpha: 0.05,
                n_permutations: 50, // 轻量置换（真实场景可加大）
                restore_real_distribution: true,
            };
            outcome.drift_triggered = detect_reaction_drift(&historical, &recent, &drift_cfg);
        }
    }

    Ok(outcome)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
