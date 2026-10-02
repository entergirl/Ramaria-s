//! crates/ramaria-memory/src/inference/shrink/tests.rs - //! crates/ramaria-memory/src/inference/shrink.rs - 经验贝叶斯小样本收缩单元测试
//!
//! 设计特点:
//! - 位于 inference::shrink 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 shrink.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

// ---- γ 动态计算 ----

/// compute_dynamic_gamma 各样本量参数化验证。
#[test]
fn gamma_cases() {
    let config = ShrinkConfig::default();
    let cases = [
        (10.0, 4.0),       // γ = 3 + 30/30（max(10,30)=30）
        (300.0, 3.1),      // γ = 3 + 30/300
        (10_000.0, 3.003), // γ = 3 + 30/10000
    ];
    for (n, expected) in cases {
        let gamma = compute_dynamic_gamma(n, &config);
        assert!((gamma - expected).abs() < 0.001, "n={n}");
    }
}

// ---- Valence 收缩 ----

/// shrink_valence 各 n_eff 参数化验证。
#[test]
fn shrink_valence_cases() {
    let cases = [
        (100.0, 0.777), // n_eff 很大 → 接近原始值
        (1.0, 0.32),    // n_eff 很小 → 接近全局均值
        (0.0, 0.2),     // 完全依赖先验
    ];
    for (n_eff, expected) in cases {
        let result = shrink_valence(0.8, n_eff, 0.2, 4.0);
        assert!((result - expected).abs() < 0.01, "n_eff={n_eff}");
    }
}

// ---- Logit / Sigmoid ----

#[test]
fn logit_sigmoid_roundtrip() {
    for &p in &[0.1, 0.3, 0.5, 0.7, 0.9] {
        let l = logit(p);
        let s = sigmoid(l);
        assert!((s - p).abs() < 1e-6, "roundtrip failed for p={}", p);
    }
}

#[test]
fn logit_boundaries() {
    // 边界值不应 panic
    let l0 = logit(0.0);
    let l1 = logit(1.0);
    // 应返回有效值
    assert!(l0.is_finite());
    assert!(l1.is_finite());
    // sigmoid 应在 [0,1]
    let s0 = sigmoid(l0);
    assert!((0.0..=1.0).contains(&s0));
}

#[test]
fn sigmoid_center() {
    assert!((sigmoid(0.0) - 0.5).abs() < 1e-10);
}

// ---- Share 收缩 ----

#[test]
fn shrink_share_basic() {
    // 分类 share=0.9, n_eff=2, 全局 share=0.5, γ=4
    // cat_logit = ln(0.9/0.1) ≈ 2.197
    // global_logit = ln(0.5/0.5) = 0.0
    // shrunk_logit = (2/6)*2.197 + (4/6)*0.0 ≈ 0.732
    // sigmoid(0.732) ≈ 0.675
    let result = shrink_share(0.9, 2.0, 0.5, 4.0);
    assert!((result - 0.675).abs() < 0.01);
}

// ---- Presentation 收缩 ----

#[test]
fn shrink_presentation_sum_to_one() {
    let (o, s, m) = shrink_presentation(0.6, 0.3, 0.1, 2.0, 0.33, 0.33, 0.34, 4.0);
    let total = o + s + m;
    assert!(
        (total - 1.0).abs() < 1e-10,
        "收缩后 presentation 比例和应为1，实际={}",
        total
    );
}

#[test]
fn shrink_presentation_small_n_eff() {
    // 小样本时收缩向全局先验靠近
    let (o, s, m) = shrink_presentation(1.0, 0.0, 0.0, 1.0, 0.33, 0.33, 0.34, 4.0);
    // 收缩后不应再是 (1,0,0)，而应更均匀
    assert!(o < 1.0, "小样本时应向先验收缩");
    assert!(s > 0.0);
    assert!(m > 0.0);
}

// ---- 全局统计 ----

#[test]
fn compute_global_stats_basic() {
    use crate::inference::stats::{CalibratedWeightConfig, compute_category_stats};
    use ramaria_core::types::{MemoryEvent, Presentation, now_ms};

    fn mk(
        title: &str,
        kw: &str,
        salience: f64,
        valence: f64,
        share: f64,
        pres: Presentation,
    ) -> MemoryEvent {
        let now = now_ms();
        let mut ev = MemoryEvent::new(
            "user-0001".into(),
            title.into(),
            "摘要".into(),
            now - 1000,
            now,
        );
        ev.keywords = Some(kw.into());
        ev.confidence = 0.9;
        ev.salience = salience;
        ev.valence = valence;
        ev.share = share;
        ev.presentation = pres;
        ev
    }

    let events1 = vec![
        mk("E1", "工作", 0.8, 0.5, 0.7, Presentation::Objective),
        mk("E2", "工作", 0.6, 0.3, 0.5, Presentation::Subjective),
    ];
    let events2 = vec![mk("E3", "社交", 0.7, -0.2, 0.9, Presentation::Mixed)];

    let wcfg = CalibratedWeightConfig::default();
    let cat1 = compute_category_stats("工作", &events1, None, &wcfg);
    let cat2 = compute_category_stats("社交", &events2, None, &wcfg);
    let cats = vec![cat1, cat2];

    let (gv, gs, go, gsu, gm, n_total) = compute_global_stats(&cats);
    assert!(n_total > 0.0);
    // 全局值应在各分类值之间
    assert!((-0.2..=0.5).contains(&gv));
    assert!((0.5..=0.9).contains(&gs));
    let pres_sum = go + gsu + gm;
    assert!((pres_sum - 1.0).abs() < 1e-10, "全局 presentation 和应为1");
}

// ---- 批量收缩 ----

#[test]
fn shrink_category_updates_all_fields() {
    use crate::inference::stats::{CalibratedWeightConfig, compute_category_stats};
    use ramaria_core::types::{MemoryEvent, Presentation, now_ms};

    fn mk(salience: f64, valence: f64, share: f64, pres: Presentation) -> MemoryEvent {
        let now = now_ms();
        let mut ev = MemoryEvent::new(
            "user-0001".into(),
            "E".into(),
            "摘要".into(),
            now - 1000,
            now,
        );
        ev.keywords = Some("工作".into());
        ev.confidence = 0.9;
        ev.salience = salience;
        ev.valence = valence;
        ev.share = share;
        ev.presentation = pres;
        ev
    }

    let events = vec![mk(0.5, 0.9, 0.9, Presentation::Objective)];
    let wcfg = CalibratedWeightConfig::default();
    let mut cat = compute_category_stats("工作", &events, None, &wcfg);
    let original_valence = cat.valence_mean;
    let original_share = cat.share_mean;
    let original_obj = cat.presentation_objective_ratio;

    // 单事件 n_eff=0.5，应该明显收缩
    shrink_category(&mut cat, 0.1, 0.4, 0.33, 0.33, 0.34, 4.0);

    // 收缩后值应向先验靠近
    assert!(
        cat.valence_mean < original_valence,
        "valence 应向先验 0.1 方向收缩"
    );
    assert!(
        cat.share_mean < original_share,
        "share 应向先验 0.4 方向收缩"
    );
    assert!(
        cat.presentation_objective_ratio < original_obj,
        "objective ratio 应收缩"
    );

    // presentation 比例和仍为 1
    let sum = cat.presentation_objective_ratio
        + cat.presentation_subjective_ratio
        + cat.presentation_mixed_ratio;
    assert!((sum - 1.0).abs() < 1e-10, "presentation 和应为1");
}

// =========================================================
// 分层先验收缩
// =========================================================

/// 构造测试用 CategoryStats。
fn make_cat(
    category: &str,
    n_eff: f64,
    valence_mean: f64,
    share_mean: f64,
    obj: f64,
    sub: f64,
    mix: f64,
) -> CategoryStats {
    CategoryStats {
        category: category.into(),
        event_count: n_eff as usize,
        n_eff,
        valence_mean,
        valence_std: 0.2,
        valence_positive_ratio: if valence_mean > 0.0 { 0.7 } else { 0.3 },
        share_mean,
        share_std: 0.1,
        presentation_objective_ratio: obj,
        presentation_subjective_ratio: sub,
        presentation_mixed_ratio: mix,
        group_weight: 1.0,
    }
}

/// compute_domain_prior 各索引/n_eff 参数化验证。
#[test]
fn compute_domain_prior_cases() {
    // 空索引 → None
    let cats = vec![make_cat("工作", 10.0, 0.5, 0.6, 0.4, 0.3, 0.3)];
    assert!(
        compute_domain_prior(&cats, &[]).is_none(),
        "空索引应返回 None"
    );
    // n_eff < 1.0 → None
    let cats = vec![make_cat("社交", 0.5, 0.8, 0.9, 0.2, 0.5, 0.3)];
    assert!(
        compute_domain_prior(&cats, &[0]).is_none(),
        "n_eff < 1.0 应返回 None"
    );
    // 有效领域 → Some，prior 接近原始值
    let cats = vec![
        make_cat("工作", 8.0, 0.6, 0.7, 0.5, 0.3, 0.2),
        make_cat("社交", 5.0, 0.1, 0.8, 0.2, 0.5, 0.3),
    ];
    let prior = compute_domain_prior(&cats, &[1]).expect("有效领域应返回 Some");
    assert!((prior.valence_mean - 0.1).abs() < 0.01);
    assert!((prior.share_mean - 0.8).abs() < 0.01);
}

#[test]
fn run_shrinkage_layered_base_primary_use_global() {
    let config = ShrinkConfig::default();
    let mut cats = vec![make_cat("工作", 10.0, 0.8, 0.7, 0.5, 0.3, 0.2)];
    let original_valence = cats[0].valence_mean;

    let mut hints = HashMap::new();
    hints.insert("工作".to_string(), TraitLayer::Base);

    let gamma = run_shrinkage_layered(&mut cats, &config, &hints, None);
    assert!(gamma > 0.0);

    // Base 使用全局先验，n_eff=10 较大，收缩幅度小
    assert!(
        cats[0].valence_mean <= original_valence,
        "应向全局均值方向收缩（全局 valence 可能较低）"
    );
}

#[test]
fn run_shrinkage_layered_accent_uses_domain_prior() {
    let config = ShrinkConfig::default();
    // 两个分类: 工作（Base, n_eff=20, valence=0.8）和 社交（Accent, n_eff=2, valence=-0.5）
    let mut cats = vec![
        make_cat("工作", 20.0, 0.8, 0.7, 0.5, 0.3, 0.2),
        make_cat("社交", 3.0, -0.5, 0.4, 0.2, 0.5, 0.3),
    ];

    // 记录原始值
    let social_original_valence = cats[1].valence_mean;

    // 先用全局先验收缩一次（等价于已删除的 run_shrinkage：全局统计 → γ → 逐分类收缩）
    let mut cats_global = cats.clone();
    {
        let (gv, gs, go, gsub, gmix, n_total) = compute_global_stats(&cats_global);
        let gamma_g = compute_dynamic_gamma(n_total, &config);
        for cat in cats_global.iter_mut() {
            shrink_category(cat, gv, gs, go, gsub, gmix, gamma_g);
        }
    }
    let social_global_shrunk = cats_global[1].valence_mean;

    // 再用分层先验收缩
    let mut hints = HashMap::new();
    hints.insert("工作".to_string(), TraitLayer::Base);
    hints.insert("社交".to_string(), TraitLayer::Accent);
    // 只有 1 个 Accent → 领域先验不可用 → fallback 全局，结果应与 run_shrinkage 一致
    let gamma = run_shrinkage_layered(&mut cats, &config, &hints, None);
    assert!(gamma > 0.0);

    // 单 Accent 时 fallback 全局，结果应接近全局收缩
    assert!(
        (cats[1].valence_mean - social_global_shrunk).abs() < 0.01,
        "单 Accent fallback 全局时结果应一致"
    );
    // 社交的 n_eff=3 较小，应被明显收缩
    assert!(
        cats[1].valence_mean > social_original_valence,
        "小样本负值应被向全局均值收缩（提升）"
    );
}

#[test]
fn run_shrinkage_layered_multiple_accents() {
    let config = ShrinkConfig::default();
    // 三个分类: 工作(Base), 社交(Accent), 家庭(Accent)
    let mut cats = vec![
        make_cat("工作", 20.0, 0.6, 0.7, 0.4, 0.3, 0.3),
        make_cat("社交", 3.0, -0.4, 0.8, 0.1, 0.6, 0.3),
        make_cat("家庭", 4.0, -0.2, 0.3, 0.3, 0.3, 0.4),
    ];

    // 记录 accent 分类的原始值
    let social_original_valence = cats[1].valence_mean;
    let family_original_valence = cats[2].valence_mean;

    let mut hints = HashMap::new();
    hints.insert("工作".to_string(), TraitLayer::Base);
    hints.insert("社交".to_string(), TraitLayer::Accent);
    hints.insert("家庭".to_string(), TraitLayer::Accent);

    let gamma = run_shrinkage_layered(&mut cats, &config, &hints, None);
    assert!(gamma > 0.0);

    // 工作（Base, n_eff=20）几乎不变
    assert!((cats[0].valence_mean - 0.6).abs() < 0.1);

    // 社交和家庭（Accent, 小样本）应被收缩
    // 领域先验来自 Accent 子集: valence ≈ (-0.4*3 + -0.2*4)/(3+4) ≈ -0.286
    // 社交收缩: (3*−0.4 + γ*−0.286)/(3+γ) — 应向 -0.286 靠近
    assert!(
        cats[1].valence_mean >= social_original_valence,
        "社交应被向领域均值收缩（领域均值高于原始值）"
    );
    assert!(
        cats[2].valence_mean <= family_original_valence,
        "家庭应被向领域均值收缩（领域均值低于原始值）"
    );
}

#[test]
fn run_shrinkage_layered_empty_hints() {
    let config = ShrinkConfig::default();
    let mut cats = vec![make_cat("工作", 10.0, 0.8, 0.7, 0.5, 0.3, 0.2)];

    let mut cats_expected = cats.clone();
    // 全局先验收缩基线（等价于已删除的 run_shrinkage）
    {
        let (gv, gs, go, gsub, gmix, n_total) = compute_global_stats(&cats_expected);
        let gamma_g = compute_dynamic_gamma(n_total, &config);
        for cat in cats_expected.iter_mut() {
            shrink_category(cat, gv, gs, go, gsub, gmix, gamma_g);
        }
    }

    let hints = HashMap::new(); // 空 hints
    let gamma = run_shrinkage_layered(&mut cats, &config, &hints, None);
    assert!(gamma > 0.0);

    // 空 hints 应退化为全局先验（与全局先验收缩结果一致）
    assert!(
        (cats[0].valence_mean - cats_expected[0].valence_mean).abs() < 0.01,
        "空 hints 应与全局先验收缩结果一致"
    );
}

#[test]
fn run_shrinkage_layered_empty_categories() {
    let config = ShrinkConfig::default();
    let mut cats: Vec<CategoryStats> = Vec::new();
    let hints = HashMap::new();
    let gamma = run_shrinkage_layered(&mut cats, &config, &hints, None);
    assert!((gamma - 4.0).abs() < 1e-10);
}

#[test]
fn shrink_prior_from_categories_empty() {
    let cats: Vec<CategoryStats> = Vec::new();
    let prior = ShrinkPrior::from_categories(&cats);
    assert!((prior.valence_mean - 0.0).abs() < 1e-10);
    assert!((prior.share_mean - 0.5).abs() < 1e-10);
    assert!((prior.n_total_eff - 0.0).abs() < 1e-10);
}

#[test]
fn shrink_prior_from_categories_single() {
    let cats = vec![make_cat("工作", 10.0, 0.6, 0.7, 0.4, 0.3, 0.3)];
    let prior = ShrinkPrior::from_categories(&cats);
    assert!((prior.valence_mean - 0.6).abs() < 0.01);
    assert!((prior.share_mean - 0.7).abs() < 0.01);
    assert!((prior.n_total_eff - 10.0).abs() < 0.01);
}

// =========================================================
// 跨用户经验先验
// =========================================================

fn make_prior(valence: f64, share: f64, n_eff: f64) -> ShrinkPrior {
    ShrinkPrior {
        valence_mean: valence,
        share_mean: share,
        obj_ratio: 0.4,
        sub_ratio: 0.3,
        mix_ratio: 0.3,
        n_total_eff: n_eff,
    }
}

/// 空输入（系统内无已有人格画像）→ 回退统一默认先验（首个 persona）。
#[test]
fn merge_cross_user_prior_empty_returns_default() {
    let prior = merge_cross_user_prior(&[]);
    assert!((prior.valence_mean - 0.0).abs() < 1e-10);
    assert!((prior.share_mean - 0.5).abs() < 1e-10);
    assert!((prior.obj_ratio - 1.0 / 3.0).abs() < 1e-10);
    assert!((prior.n_total_eff - 0.0).abs() < 1e-10);
}

/// 多 persona 按 n_eff 加权平均。
#[test]
fn merge_cross_user_prior_weighted_average() {
    // persona A: valence=0.6, n=10；persona B: valence=-0.2, n=30
    // 加权 valence = (0.6*10 + (-0.2)*30)/40 = (6 - 6)/40 = 0.0
    let priors = vec![make_prior(0.6, 0.7, 10.0), make_prior(-0.2, 0.3, 30.0)];
    let merged = merge_cross_user_prior(&priors);
    assert!(
        (merged.valence_mean - 0.0).abs() < 1e-9,
        "v={}",
        merged.valence_mean
    );
    // share = (0.7*10 + 0.3*30)/40 = (7+9)/40 = 0.4
    assert!(
        (merged.share_mean - 0.4).abs() < 1e-9,
        "s={}",
        merged.share_mean
    );
    assert!((merged.n_total_eff - 40.0).abs() < 1e-9);
}

/// 单 persona 的跨用户先验 = 该 persona 自身先验。
#[test]
fn merge_cross_user_prior_single() {
    let prior = make_prior(0.5, 0.6, 12.0);
    let merged = merge_cross_user_prior(std::slice::from_ref(&prior));
    assert!((merged.valence_mean - 0.5).abs() < 1e-9);
    assert!((merged.share_mean - 0.6).abs() < 1e-9);
    assert!((merged.n_total_eff - 12.0).abs() < 1e-9);
}

// =========================================================
// 聚合行 → 跨用户先验换算
// =========================================================

fn make_agg(persona_uid: &str, n_events: u64, valence: f64, share: f64) -> PersonaEventAggregate {
    PersonaEventAggregate::new(persona_uid, n_events, valence, share, 0.4, 0.3, 0.3)
}

/// 聚合行字段到 ShrinkPrior 的换算（n_events 作为有效样本量）。
#[test]
fn aggregate_to_shrink_prior_maps_fields() {
    let agg = make_agg("char-a", 40, 0.25, 0.62);
    let prior = ShrinkPrior::from(&agg);
    assert!((prior.valence_mean - 0.25).abs() < 1e-12);
    assert!((prior.share_mean - 0.62).abs() < 1e-12);
    assert!((prior.obj_ratio - 0.4).abs() < 1e-12);
    assert!((prior.sub_ratio - 0.3).abs() < 1e-12);
    assert!((prior.mix_ratio - 0.3).abs() < 1e-12);
    assert!((prior.n_total_eff - 40.0).abs() < 1e-12);
}

/// 聚合行：空输入（系统内无其他 persona）→ None，不冒充中性先验。
#[test]
fn build_cross_user_prior_empty_returns_none() {
    assert!(build_cross_user_prior(&[]).is_none());
}

/// 聚合行：单 persona 且事件充足 → Some（该 persona 自身即跨用户来源）。
#[test]
fn build_cross_user_prior_single_persona_enough() {
    let rows = vec![make_agg("char-a", 40, 0.5, 0.6)];
    let prior = build_cross_user_prior(&rows).expect("单 persona 事件充足应返回 Some");
    assert!((prior.n_total_eff - 40.0).abs() < 1e-9);
    assert!((prior.valence_mean - 0.5).abs() < 1e-9);
}

/// 聚合行：多 persona 按 n_events 加权合并。
#[test]
fn build_cross_user_prior_weighted_merge() {
    // char-a: valence=0.6, n=30；char-b: valence=-0.2, n=10
    // 加权 valence = (0.6*30 + (-0.2)*10)/40 = (18-2)/40 = 0.4
    let rows = vec![
        make_agg("char-a", 30, 0.6, 0.7),
        make_agg("char-b", 10, -0.2, 0.3),
    ];
    let prior = build_cross_user_prior(&rows).expect("总事件充足应返回 Some");
    assert!(
        (prior.valence_mean - 0.4).abs() < 1e-9,
        "v={}",
        prior.valence_mean
    );
    assert!((prior.n_total_eff - 40.0).abs() < 1e-9);
}

/// 聚合行：总事件数低于阈值（< 30）→ None，避免借用不可靠杂讯。
#[test]
fn build_cross_user_prior_too_few_events_returns_none() {
    let rows = vec![make_agg("char-a", 29, 0.5, 0.6)];
    assert!(
        build_cross_user_prior(&rows).is_none(),
        "总事件不足 30 时应返回 None"
    );
}

/// 聚合行：n_events = 0 的行被忽略（无事件即无经验来源）。
#[test]
fn build_cross_user_prior_skips_zero_event_rows() {
    let rows = vec![
        make_agg("char-a", 0, 0.5, 0.6),
        make_agg("char-b", 40, 0.2, 0.4),
    ];
    let prior = build_cross_user_prior(&rows).expect("有效来源应返回 Some");
    assert!(
        (prior.n_total_eff - 40.0).abs() < 1e-9,
        "0 事件行不应参与加权"
    );
    assert!((prior.valence_mean - 0.2).abs() < 1e-9);
}

/// `run_shrinkage_layered` 传入跨用户先验时，小样本分类向跨用户先验收缩。
#[test]
fn run_shrinkage_layered_uses_cross_user_prior() {
    let config = ShrinkConfig::default();
    // 当前 persona 分类 valence=0.8（极端、小样本），n_eff=1
    let mut cats = vec![make_cat("工作", 1.0, 0.8, 0.7, 0.5, 0.3, 0.2)];
    let cross_user = make_prior(0.0, 0.5, 100.0); // 跨用户经验先验（中性）

    let mut hints = HashMap::new();
    hints.insert("工作".to_string(), TraitLayer::Base);

    let gamma = run_shrinkage_layered(&mut cats, &config, &hints, Some(&cross_user));
    assert!(gamma > 0.0);

    // n_eff=1 极小时，收缩应显著向跨用户先验 0.0 靠近（远低于原 0.8）
    assert!(
        cats[0].valence_mean < 0.6,
        "小样本应显著向跨用户先验收缩，实际={}",
        cats[0].valence_mean
    );
}

/// 关闭冷启动跨用户先验（传入 None）→ 回退当前 persona 内先验。
#[test]
fn run_shrinkage_layered_fallback_own_prior() {
    let config = ShrinkConfig::default();
    let mut cats = vec![make_cat("工作", 1.0, 0.8, 0.7, 0.5, 0.3, 0.2)];
    let mut hints = HashMap::new();
    hints.insert("工作".to_string(), TraitLayer::Base);

    let gamma = run_shrinkage_layered(&mut cats, &config, &hints, None);
    assert!(gamma > 0.0);
    // 当前 persona 内全局先验 = 0.8（唯一分类），n_eff 再小也向自身 0.8 收缩，基本不变
    assert!(
        (cats[0].valence_mean - 0.8).abs() < 0.2,
        "无跨用户先验时应向自身先验收缩，实际={}",
        cats[0].valence_mean
    );
}
