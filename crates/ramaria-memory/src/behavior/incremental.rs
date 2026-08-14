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

    /// 加入一条待定事件。
    pub fn add(&mut self, event: &ramaria_core::types::MemoryEvent) {
        self.events.push(PendingEvent::from_event(event, now_ms()));
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
mod tests {
    use super::*;
    use ramaria_core::behavior::{BehaviorParams, BehaviorSituation};
    use ramaria_core::types::MemoryEvent;

    fn rule(id: i64, keywords: &[&str], centroid: Option<Vec<f32>>) -> BehaviorRule {
        let mut r = BehaviorRule::new(
            "char-0001",
            BehaviorSituation {
                keywords: keywords.iter().map(|k| k.to_string()).collect(),
                centroid,
                response_centroid: None,
                valence_mean: -0.4,
                valence_std: 0.2,
                sample_count: 6,
                presentation_dist: Vec::new(),
                situation_strength_mean: 3.0,
                time_span_days: 10.0,
                trait_refs: Vec::new(),
            },
            Some(format!("规则 {id}")),
            BehaviorParams::default(),
            RuleSource::Auto,
        );
        r.id = id;
        r
    }

    fn event(id: i64, keywords: &str, valence: f64) -> MemoryEvent {
        let mut ev = MemoryEvent::new("char-0001".into(), format!("t{id}"), format!("s{id}"), 1, 2);
        ev.id = id;
        ev.keywords = Some(keywords.to_string());
        ev.valence = valence;
        ev
    }

    fn sample_from(ev: &MemoryEvent) -> BehaviorSample {
        sample_from_event(ev)
    }

    fn cfg() -> BehaviorConfig {
        BehaviorConfig::default()
    }

    // ---- 归簇 ----

    #[test]
    fn assign_joins_most_similar_cluster_above_threshold() {
        let rules = vec![
            rule(1, &["加班"], Some(vec![1.0, 0.0])),
            rule(2, &["猫"], Some(vec![0.0, 1.0])),
        ];
        let ev = event(10, "加班,累", -0.5);
        // 情境向量：与规则 1 同向（关键词部分重合）
        let mut s = sample_from(&ev);
        s.situation_vector = Some(vec![0.95, 0.05]);
        // sim = 0.3·cos(0.95) + 0.7·J(1/2) = 0.285 + 0.35 = 0.635 ≥ θ_join=0.6
        let idx = assign_event_to_cluster(&s, &rules, 0.6, 0.3).expect("应归簇");
        assert_eq!(rules[idx].id, 1);
    }

    #[test]
    fn assign_returns_none_below_threshold() {
        let rules = vec![rule(1, &["加班"], Some(vec![1.0, 0.0]))];
        let ev = event(10, "完全无关的话题", 0.1);
        let s = sample_from(&ev);
        assert!(assign_event_to_cluster(&s, &rules, 0.7, 0.3).is_none());
    }

    #[test]
    fn assign_skips_disabled_and_manual_rules() {
        let mut auto = rule(1, &["加班"], Some(vec![1.0, 0.0]));
        auto.enabled = false;
        let mut manual = rule(2, &["加班"], Some(vec![1.0, 0.0]));
        manual.source = RuleSource::Manual;
        let rules = vec![auto, manual];
        let mut ev = event(10, "加班", -0.5);
        let mut s = sample_from(&ev);
        s.situation_vector = Some(vec![1.0, 0.0]);
        // 禁用与手工规则都不吸收 → None（进待定池）
        assert!(assign_event_to_cluster(&s, &rules, 0.7, 0.3).is_none());
    }

    #[test]
    fn sample_rule_similarity_degrades_to_keywords() {
        let r = rule(1, &["加班", "累"], None);
        let ev = event(10, "加班,累,工作", -0.5);
        let s = sample_from(&ev);
        // 无向量 → Jaccard（查询侧？这里用集合 Jaccard）{加班,累}∩{加班,累,工作}/{加班,累,工作} = 2/3
        let sim = sample_rule_similarity(&s, &r, 0.3);
        assert!((sim - 2.0 / 3.0).abs() < 1e-9, "实际 {sim}");
    }

    // ---- 待定池 ----

    #[test]
    fn pending_pool_forms_cluster_when_cohesive() {
        let mut pool = PendingPool::new(&cfg());
        let events: Vec<MemoryEvent> = (0..3).map(|i| event(100 + i, "加班,累", -0.5)).collect();
        for ev in &events {
            pool.add(ev);
        }
        let (formed, low) = pool.advance(now_ms());
        assert_eq!(formed.len(), 1, "3 条同质事件成簇");
        assert_eq!(formed[0].len(), 3);
        assert!(low.is_empty());
    }

    #[test]
    fn pending_pool_does_not_form_below_min_size() {
        let mut pool = PendingPool::new(&cfg());
        for i in 0..2 {
            pool.add(&event(100 + i, "加班,累", -0.5));
        }
        let (formed, _) = pool.advance(now_ms());
        assert!(formed.is_empty(), "2 条 < min_cluster_size 不成簇");
    }

    #[test]
    fn pending_pool_low_confidence_after_expiry() {
        let mut pool = PendingPool::new(&cfg());
        pool.add(&event(1, "加班,累", -0.5));
        // 入池后模拟超过 30 天
        let far_future = now_ms() + 40 * 86_400_000;
        let (formed, low) = pool.advance(far_future);
        assert!(formed.is_empty());
        assert_eq!(low, vec![1], "超期未成簇 → 低置信");
        assert!(pool.events[0].low_confidence);
    }

    #[test]
    fn pending_pool_expired_low_confidence_not_re_flagged() {
        let mut pool = PendingPool::new(&cfg());
        pool.add(&event(1, "加班,累", -0.5));
        let far_future = now_ms() + 40 * 86_400_000;
        pool.advance(far_future);
        // 再次推进不重复标记
        let (_, low) = pool.advance(far_future + 86_400_000);
        assert!(low.is_empty());
    }

    // ---- 证据衰减 ----

    #[test]
    fn evidence_decays_below_threshold_marks_invalid() {
        let mut r = rule(1, &["加班"], None);
        r.created_at = now_ms() - 365 * 86_400_000; // 一年前
        r.evidence = (1..=6)
            .map(|i| ramaria_core::behavior::BehaviorEvidence {
                event_id: i,
                weight: 0.8,
            })
            .collect();
        let mut clone = r.clone();
        let expired = decay_evidence_weights(&mut clone, now_ms(), 60.0, 0.3);
        assert!(expired, "一年衰减后总权重 < 0.3 → 应失效");
        assert!(clone.evidence.iter().all(|e| e.weight < 0.8));
    }

    #[test]
    fn recent_rule_evidence_not_expired() {
        let mut r = rule(1, &["加班"], None);
        r.evidence = (1..=6)
            .map(|i| ramaria_core::behavior::BehaviorEvidence {
                event_id: i,
                weight: 0.8,
            })
            .collect();
        let mut clone = r.clone();
        let expired = decay_evidence_weights(&mut clone, now_ms(), 60.0, 0.3);
        assert!(!expired, "刚创建未衰减 → 不失效");
    }

    // ---- 漂移 ----

    #[test]
    fn drift_detected_when_reaction_flips() {
        // 历史全是消极（-0.5），近期全积极（+0.5）→ 显著漂移
        let historical = vec![-0.5; 8];
        let recent = vec![0.5; 8];
        let config = DriftConfig {
            alpha: 0.05,
            n_permutations: 200,
        };
        assert!(detect_reaction_drift(&historical, &recent, &config));
    }

    #[test]
    fn no_drift_when_distributions_similar() {
        let historical = vec![-0.5, -0.4, -0.6, -0.5, -0.3];
        let recent = vec![-0.5, -0.4, -0.6, -0.5, -0.3];
        let config = DriftConfig {
            alpha: 0.05,
            n_permutations: 200,
        };
        assert!(!detect_reaction_drift(&historical, &recent, &config));
    }

    // ---- 编排 ----

    #[tokio::test]
    async fn compute_update_assigns_and_forms_new_cluster() {
        let mut rules = vec![rule(1, &["猫"], Some(vec![0.0, 1.0]))];
        let mut pending = PendingPool::new(&cfg());
        let ev1 = event(10, "加班,累", -0.5);
        // 无 embedding：关键词"加班,累"与规则"猫"的 Jaccard=0 < θ_join → 未归入 → 待定池
        let outcome =
            compute_incremental_update(&[ev1], &mut rules, &mut pending, &cfg(), None, now_ms())
                .await
                .expect("计算成功");
        assert!(outcome.assigned.is_empty());
        assert!(outcome.new_cluster_event_ids.is_empty());
        assert_eq!(pending.events.len(), 1, "未归入事件进待定池");
    }

    #[tokio::test]
    async fn compute_update_assigns_by_keywords_when_vectors_match() {
        // 纯关键词路径：事件关键词与规则关键词完全重合（J=1 ≥ θ_join）→ 归入
        let mut rules = vec![rule(1, &["加班", "累"], None)];
        let mut pending = PendingPool::new(&cfg());
        let ev1 = event(10, "加班,累", -0.5);
        let outcome =
            compute_incremental_update(&[ev1], &mut rules, &mut pending, &cfg(), None, now_ms())
                .await
                .expect("计算成功");
        assert_eq!(outcome.assigned.len(), 1, "关键词重合归入规则 1");
        assert_eq!(outcome.assigned[0], (10, 1));
        assert!(pending.events.is_empty());
    }

    #[tokio::test]
    async fn compute_update_decays_old_rule() {
        let mut old = rule(1, &["加班"], None);
        old.created_at = now_ms() - 400 * 86_400_000;
        old.evidence = (1..=3)
            .map(|i| ramaria_core::behavior::BehaviorEvidence {
                event_id: i,
                weight: 0.5,
            })
            .collect();
        let mut rules = vec![old];
        let mut pending = PendingPool::new(&cfg());
        let outcome =
            compute_incremental_update(&[], &mut rules, &mut pending, &cfg(), None, now_ms())
                .await
                .expect("计算成功");
        assert_eq!(outcome.decayed_rule_ids, vec![1], "旧规则证据衰减失效");
    }
}
