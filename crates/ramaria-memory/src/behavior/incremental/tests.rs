//! crates/ramaria-memory/src/behavior/incremental/tests.rs - 行为增量更新单元测试
//!
//! 设计特点:
//! - 位于 behavior::incremental 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由父文件以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::behavior::{BehaviorParams, BehaviorSituation};
use ramaria_core::types::MemoryEvent;

/// 固定测试基准时间（Unix 毫秒），保证用例不依赖真实时钟、连续运行结果一致。
const TEST_NOW_MS: i64 = 1_760_000_000_000;

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
    let ev = event(10, "加班", -0.5);
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
        pool.add_with_time(ev, TEST_NOW_MS);
    }
    let (formed, low) = pool.advance(TEST_NOW_MS);
    assert_eq!(formed.len(), 1, "3 条同质事件成簇");
    assert_eq!(formed[0].len(), 3);
    assert!(low.is_empty());
}

#[test]
fn pending_pool_does_not_form_below_min_size() {
    let mut pool = PendingPool::new(&cfg());
    for i in 0..2 {
        pool.add_with_time(&event(100 + i, "加班,累", -0.5), TEST_NOW_MS);
    }
    let (formed, _) = pool.advance(TEST_NOW_MS);
    assert!(formed.is_empty(), "2 条 < min_cluster_size 不成簇");
}

#[test]
fn pending_pool_low_confidence_after_expiry() {
    let mut pool = PendingPool::new(&cfg());
    pool.add_with_time(&event(1, "加班,累", -0.5), TEST_NOW_MS);
    // 入池后模拟超过 30 天
    let far_future = TEST_NOW_MS + 40 * 86_400_000;
    let (formed, low) = pool.advance(far_future);
    assert!(formed.is_empty());
    assert_eq!(low, vec![1], "超期未成簇 → 低置信");
    assert!(pool.events[0].low_confidence);
}

#[test]
fn pending_pool_expired_low_confidence_not_re_flagged() {
    let mut pool = PendingPool::new(&cfg());
    pool.add_with_time(&event(1, "加班,累", -0.5), TEST_NOW_MS);
    let far_future = TEST_NOW_MS + 40 * 86_400_000;
    pool.advance(far_future);
    // 再次推进不重复标记
    let (_, low) = pool.advance(far_future + 86_400_000);
    assert!(low.is_empty());
}

/// 内聚成簇的必要条件是**两两**相似度 ≥ θ_join：
/// 同主题两条 + 异质一条（与任一成员关键词零重合）→ 不成簇
/// （验证 θ_join 内聚门槛不是"简单按数量凑簇"）。
#[test]
fn pending_pool_requires_pairwise_cohesion() {
    let mut pool = PendingPool::new(&cfg());
    // 两条同质（加班）+ 一条异质（养猫）——异质样本阻断内聚
    pool.add_with_time(&event(100, "加班,累", -0.5), TEST_NOW_MS);
    pool.add_with_time(&event(101, "加班,累", -0.5), TEST_NOW_MS);
    pool.add_with_time(&event(102, "养猫,开心", 0.6), TEST_NOW_MS);
    let (formed, _low) = pool.advance(TEST_NOW_MS);
    assert!(formed.is_empty(), "含异质样本的组不满足两两内聚，不应成簇");
    // 三条全部仍在待定池（未成簇、未标记低置信）
    assert_eq!(pool.events.len(), 3);
    assert!(pool.events.iter().all(|e| !e.low_confidence));
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
        ..Default::default()
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
        ..Default::default()
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
    let outcome = compute_incremental_update(&[], &mut rules, &mut pending, &cfg(), None, now_ms())
        .await
        .expect("计算成功");
    assert_eq!(outcome.decayed_rule_ids, vec![1], "旧规则证据衰减失效");
}
