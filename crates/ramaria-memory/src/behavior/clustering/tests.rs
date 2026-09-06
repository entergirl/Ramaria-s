//! crates/ramaria-memory/src/behavior/clustering/tests.rs - 行为聚类器单元测试
//!
//! 设计特点:
//! - 覆盖样本构造/去重、向量化、融合相似度、密度聚类、簇精炼与增量归簇。
//! - 使用合成事件与 mock embedding，不依赖真实 LLM/embedding。

use super::*;
use ramaria_core::config::BehaviorConfig;
use ramaria_core::types::Presentation;

fn sample(
    event_id: i64,
    keywords: &[&str],
    r: Option<Vec<f32>>,
    s: Option<Vec<f32>>,
) -> BehaviorSample {
    BehaviorSample {
        event_id,
        situation_keywords: dedup_keywords(
            &keywords.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
        ),
        situation_vector: s,
        reaction_vector: r,
        valence: 0.0,
        presentation: Presentation::Mixed,
        salience: 0.5,
        situation_strength: Some(3),
        start_ms: 1_000,
    }
}

// ---- 三路融合相似度 ----

#[test]
fn fused_similarity_matches_formula() {
    // 构造已知向量：r 完全一致、s 相反、关键词无交集
    let a = sample(1, &["a"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0]));
    let b = sample(2, &["b"], Some(vec![1.0, 0.0]), Some(vec![-1.0, 0.0]));
    // sim = 0.4*1 + 0.3*(-1) + 0.3*0 = 0.1
    let sim = fused_similarity(&a, &b, 0.4, 0.3);
    assert!((sim - 0.1).abs() < 1e-9, "实际 {sim}");
}

#[test]
fn fused_similarity_beta_weights_dominate() {
    // a: r 与 b 相近、s 与 b 正交；b: r 与 a 相近、s 与 a 正交
    // β1 大 → 反应通道主导 → sim 高；β2 大 → 情境通道（正交=0）主导 → sim 低
    let a = sample(1, &["a"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0]));
    let b = sample(2, &["b"], Some(vec![0.9, 0.1]), Some(vec![0.0, 1.0]));
    let sim_high_beta1 = fused_similarity(&a, &b, 0.9, 0.05);
    let sim_high_beta2 = fused_similarity(&a, &b, 0.05, 0.9);
    assert!(
        sim_high_beta1 > sim_high_beta2,
        "{sim_high_beta1} vs {sim_high_beta2}"
    );
}

#[test]
fn fused_similarity_clips_cos_negative() {
    let a = sample(1, &["a"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0]));
    let b = sample(2, &["b"], Some(vec![-1.0, 0.0]), Some(vec![1.0, 0.0]));
    // cos(r) = -1 → β1 项负值被保留（clip 到 [-1,1]），但总和非负（Jaccard=0 时可能为负→clamp 0）
    let sim = fused_similarity(&a, &b, 0.4, 0.3);
    assert!(
        (0.0..=1.0).contains(&sim),
        "相似度应 clamp 到 [0,1]，实际 {sim}"
    );
}

#[test]
fn fused_similarity_keyword_channel_only() {
    // 双通道向量缺失 → 退化为纯 Jaccard（β 通道权重归零归一化）
    let a = sample(1, &["加班", "累"], None, None);
    let b = sample(2, &["加班", "累", "工作"], None, None);
    let sim = fused_similarity(&a, &b, 0.4, 0.3);
    // Jaccard({加班,累},{加班,累,工作}) = 2/3
    assert!((sim - 2.0 / 3.0).abs() < 1e-9, "实际 {sim}");
}

#[test]
fn fused_similarity_empty_keywords_zero() {
    let a = sample(1, &[], None, None);
    let b = sample(2, &[], None, None);
    assert_eq!(fused_similarity(&a, &b, 0.4, 0.3), 0.0);
}

#[test]
fn jaccard_basic() {
    // 用例已随实现迁移至 similarity.rs 表驱动测试；此处保留行为级冒烟断言
    let a = vec!["a".to_string(), "b".to_string()];
    let b = vec!["b".to_string(), "c".to_string()];
    assert!((jaccard(&a, &b) - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn jaccard_identical_and_empty() {
    let a = vec!["a".to_string()];
    assert_eq!(jaccard(&a, &a), 1.0);
    assert_eq!(jaccard(&[], &[]), 0.0);
    assert_eq!(jaccard(&a, &[]), 0.0);
}

/// 行为级 clamp 语义验证（经薄包装调用统一实现 `similarity::cosine_similarity`）:
/// 结果保留负值到 [-1,1]（不在此处 clip 为 0），零向量/维度不一致 → 0.0。
#[test]
fn cosine_clipped_handles_zero_and_mismatch() {
    assert_eq!(cosine_clipped(&[], &[]), 0.0);
    assert_eq!(cosine_clipped(&[1.0], &[1.0, 2.0]), 0.0, "维度不一致");
    assert_eq!(cosine_clipped(&[0.0, 0.0], &[1.0, 0.0]), 0.0, "零向量");
    let c = cosine_clipped(&[1.0, 0.0], &[-1.0, 0.0]);
    assert!((c + 1.0).abs() < 1e-9, "cos=-1 保留负值给上层 clip");
}

#[test]
fn beta_weight_constraint() {
    let cfg = BehaviorConfig::default();
    assert!(cfg.beta1 + cfg.beta2 <= 1.0);
    // 越界 β 防御：关键词权重取 max(0)；无向量通道且关键词权重为 0 → sim=0（不 panic）
    let a = sample(1, &["x"], None, None);
    let b = sample(2, &["x"], None, None);
    let sim = fused_similarity(&a, &b, 0.8, 0.8);
    assert!(
        (0.0..=1.0).contains(&sim),
        "越界 β 应返回合法范围值，实际 {sim}"
    );
}

// ---- 密度聚类 ----

/// 构造 N 个向量：同簇内相似、跨簇相异。
fn clusterable_samples() -> Vec<BehaviorSample> {
    let mut v = Vec::new();
    // 簇 A：反应/情境都朝向 (1,0)
    for i in 0..4 {
        v.push(sample(
            i,
            &["加班", "累"],
            Some(vec![0.9, 0.1]),
            Some(vec![1.0, 0.0]),
        ));
    }
    // 簇 B：朝向 (0,1)
    for i in 4..8 {
        v.push(sample(
            i,
            &["猫", "可爱"],
            Some(vec![0.1, 0.9]),
            Some(vec![0.0, 1.0]),
        ));
    }
    v
}

#[test]
fn density_cluster_separates_two_clusters() {
    let samples = clusterable_samples();
    let r = density_cluster(&samples, 0.5, 3, 0.4, 0.3);
    assert_eq!(r.cluster_count, 2);
    assert!(r.outlier_ratio < 1e-9, "无孤立点");
    for a in &r.assignments {
        assert!(a.cluster_id.is_some());
    }
}

#[test]
fn density_cluster_core_edge_noise_tiers() {
    // 纯关键词通道（β=0 → sim=Jaccard），精确控制邻居关系:
    // - 核心 1-4: 关键词嵌套递增，两两 Jaccard ≥0.5（邻居），各邻居 3 ≥ min_cluster_size
    // - 边界: {a,b} 与核心 1/2 相似（邻居 2 <3）→ 边界软分配
    // - 孤立: {z} 与所有人无交集 → 噪声
    let samples = vec![
        sample(0, &["a", "b", "c"], None, None),
        sample(1, &["a", "b", "c", "d"], None, None),
        sample(2, &["a", "b", "c", "d", "e"], None, None),
        sample(3, &["a", "b", "c", "d", "e", "f"], None, None),
        sample(4, &["a", "b"], None, None),
        sample(5, &["z"], None, None),
    ];
    let r = density_cluster(&samples, 0.5, 3, 0.0, 0.0);
    assert_eq!(r.cluster_count, 1);
    let tiers: Vec<&str> = r.assignments.iter().map(|a| a.tier).collect();
    assert_eq!(tiers[0], "core");
    assert_eq!(tiers[1], "core");
    assert_eq!(tiers[2], "core");
    assert_eq!(tiers[3], "core");
    assert_eq!(
        tiers[4], "edge",
        "边界软分配（邻居 2 <3 非核心，但邻接核心）"
    );
    assert_eq!(tiers[5], "noise", "孤立点不入簇");
    assert!((r.outlier_ratio - 1.0 / 6.0).abs() < 1e-9);
}

#[test]
fn density_reachability_chains_through_cores() {
    // 链式：A-B 相似 0.8、B-C 相似 0.8，但 A-C 仅 0.3（<θ_nb）
    // 核心 B 连接 A 与 C → 三者同簇（密度可达）
    let samples = vec![
        sample(0, &["a"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0])),
        sample(1, &["a"], Some(vec![0.8, 0.6]), Some(vec![0.8, 0.6])),
        sample(2, &["a"], Some(vec![0.28, 0.96]), Some(vec![0.28, 0.96])),
        // 补充核心邻居数：每个都需 ≥3 邻居
        sample(3, &["a"], Some(vec![0.9, 0.4]), Some(vec![0.9, 0.4])),
        sample(4, &["a"], Some(vec![0.5, 0.87]), Some(vec![0.5, 0.87])),
    ];
    let r = density_cluster(&samples, 0.5, 3, 0.4, 0.3);
    assert_eq!(r.cluster_count, 1, "密度可达应连接成单簇");
    assert_eq!(
        r.assignments
            .iter()
            .filter(|a| a.cluster_id.is_some())
            .count(),
        5
    );
}

#[test]
fn edge_sample_soft_assigned_to_cluster() {
    // 纯关键词通道：4 核心（互相邻居）+ 边界样本 {a,b}（邻居 2 <3 → 非核心）
    // 边界软分配：邻接核心 → 归入核心所在簇
    let samples = vec![
        sample(0, &["a", "b", "c"], None, None),
        sample(1, &["a", "b", "c", "d"], None, None),
        sample(2, &["a", "b", "c", "d", "e"], None, None),
        sample(3, &["a", "b", "c", "d", "e", "f"], None, None),
        sample(4, &["a", "b"], None, None),
    ];
    let r = density_cluster(&samples, 0.5, 3, 0.0, 0.0);
    assert_eq!(r.cluster_count, 1);
    let edge = &r.assignments[4];
    assert_eq!(edge.tier, "edge", "边界软分配");
    let core_cid = r.assignments[0].cluster_id;
    assert_eq!(edge.cluster_id, core_cid, "边界归入邻接核心所在簇");
}

#[test]
fn min_cluster_size_isolates_small_groups() {
    // 只有 2 个相似样本，min_cluster_size=3 → 两者都是孤立点
    let samples = vec![
        sample(0, &["a"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0])),
        sample(1, &["a"], Some(vec![0.95, 0.1]), Some(vec![1.0, 0.0])),
    ];
    let r = density_cluster(&samples, 0.5, 3, 0.4, 0.3);
    assert_eq!(r.cluster_count, 0);
    assert_eq!(
        r.assignments
            .iter()
            .filter(|a| a.cluster_id.is_none())
            .count(),
        2
    );
    assert!((r.outlier_ratio - 1.0).abs() < 1e-9);
}

#[test]
fn density_cluster_empty_and_single() {
    let empty: Vec<BehaviorSample> = Vec::new();
    let r = density_cluster(&empty, 0.5, 3, 0.4, 0.3);
    assert_eq!(r.cluster_count, 0);
    assert_eq!(r.outlier_ratio, 0.0);

    let single = vec![sample(
        0,
        &["a"],
        Some(vec![1.0, 0.0]),
        Some(vec![1.0, 0.0]),
    )];
    let r2 = density_cluster(&single, 0.5, 3, 0.4, 0.3);
    assert_eq!(r2.cluster_count, 0);
    assert_eq!(r2.assignments[0].tier, "noise");
}

/// 构造互异且与核心正交的孤立样本（8 维单位向量，两两 cos=0；关键词互异）。
fn isolated_samples(count: usize, start_idx: usize) -> Vec<BehaviorSample> {
    let mut v = Vec::new();
    for k in 0..count {
        let mut vec = vec![0.0f32; 8];
        // 维度 2..=7 的单位基向量（两两正交；与核心簇的 0/1 维正交）
        vec[2 + (k % 6)] = 1.0;
        v.push(sample(
            (start_idx + k) as i64,
            &[format!("孤{k}").as_str()],
            Some(vec.clone()),
            Some(vec),
        ));
    }
    v
}

#[test]
fn density_cluster_reports_high_outlier_ratio() {
    // 8 个核心（两簇 4+4，min_cluster_size=3 时成簇）+ 15 个互异孤立样本
    // → 孤立比例 = 15/23 ≈ 0.65 > 0.6（失败模式检查触发阈值）
    let mut samples = clusterable_samples();
    samples.extend(isolated_samples(15, 100));
    let r = density_cluster(&samples, 0.5, 3, 0.4, 0.3);
    let threshold = BehaviorConfig::default().max_outlier_ratio;
    assert!(
        r.outlier_ratio > threshold,
        "孤立点比例应超限: {}",
        r.outlier_ratio
    );
    // 核心簇仍被识别（孤立点不入簇，不污染核心簇）
    assert_eq!(r.cluster_count, 2);
}

// ---- 簇提炼 ----

#[test]
fn refine_keywords_top_n() {
    let samples = vec![
        sample(
            1,
            &["加班", "累", "工作"],
            Some(vec![1.0, 0.0]),
            Some(vec![1.0, 0.0]),
        ),
        sample(
            2,
            &["加班", "累", "深夜"],
            Some(vec![0.9, 0.1]),
            Some(vec![1.0, 0.0]),
        ),
        sample(
            3,
            &["加班", "周末"],
            Some(vec![0.95, 0.05]),
            Some(vec![1.0, 0.0]),
        ),
    ];
    let rc = refine_cluster(&samples, &[0, 1, 2], 0.4, 0.3);
    assert!(rc.situation.keywords.contains(&"加班".to_string()));
    assert!(rc.situation.keywords.contains(&"累".to_string()));
    // 频次最高的 "加班" 应排第一
    assert_eq!(rc.situation.keywords[0], "加班");
}

#[test]
fn refine_valence_weighted_mean_and_std() {
    let mut a = sample(1, &["x"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0]));
    a.valence = -0.5;
    a.salience = 0.8;
    let mut b = sample(2, &["x"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0]));
    b.valence = -0.3;
    b.salience = 0.2;
    let rc = refine_cluster(&[a, b], &[0, 1], 0.4, 0.3);
    // 加权均值 = (-0.5*0.8 + -0.3*0.2) / 1.0 = -0.46
    assert!((rc.situation.valence_mean - (-0.46)).abs() < 1e-9);
    assert!(rc.situation.valence_std > 0.0);
    assert_eq!(rc.situation.sample_count, 2);
}

#[test]
fn refine_presentation_distribution() {
    let mut a = sample(1, &["x"], None, None);
    a.presentation = Presentation::Subjective;
    let mut b = sample(2, &["x"], None, None);
    b.presentation = Presentation::Subjective;
    let mut c = sample(3, &["x"], None, None);
    c.presentation = Presentation::Objective;
    let rc = refine_cluster(&[a, b, c], &[0, 1, 2], 0.4, 0.3);
    let subj = rc
        .situation
        .presentation_dist
        .iter()
        .find(|p| p.presentation == Presentation::Subjective)
        .unwrap();
    assert!((subj.freq - 2.0 / 3.0).abs() < 1e-9);
    // 分布按频率降序
    assert!(rc.situation.presentation_dist[0].freq >= rc.situation.presentation_dist[1].freq);
}

#[test]
fn refine_time_span_and_strength() {
    let mut a = sample(1, &["x"], None, None);
    a.start_ms = 1_000_000;
    a.situation_strength = Some(4);
    let mut b = sample(2, &["x"], None, None);
    b.start_ms = 1_000_000 + 3 * 86_400_000; // +3 天
    b.situation_strength = None; // 等效 3
    let rc = refine_cluster(&[a, b], &[0, 1], 0.4, 0.3);
    assert!((rc.situation.time_span_days - 3.0).abs() < 1e-9);
    assert!((rc.situation.situation_strength_mean - 3.5).abs() < 1e-9);
}

#[test]
fn refine_centroid_vectors() {
    // sample(事件, 关键词, 反应向量 r, 情境向量 s)
    let samples = vec![
        sample(1, &["x"], Some(vec![1.0, 0.0]), Some(vec![2.0, 0.0])),
        sample(2, &["x"], Some(vec![0.0, 1.0]), Some(vec![0.0, 2.0])),
    ];
    let rc = refine_cluster(&samples, &[0, 1], 0.4, 0.3);
    // centroid = 情境通道均值 = ((2,0)+(0,2))/2 = (1,1)
    let c = rc.situation.centroid.expect("情境中心应存在");
    assert!(
        (c[0] - 1.0).abs() < 1e-6 && (c[1] - 1.0).abs() < 1e-6,
        "{c:?}"
    );
    // response_centroid = 反应通道均值 = ((1,0)+(0,1))/2 = (0.5,0.5)
    let r = rc.situation.response_centroid.expect("反应中心应存在");
    assert!(
        (r[0] - 0.5).abs() < 1e-6 && (r[1] - 0.5).abs() < 1e-6,
        "{r:?}"
    );
}

#[test]
fn refine_centroid_none_when_no_vectors() {
    let samples = vec![sample(1, &["x"], None, None), sample(2, &["x"], None, None)];
    let rc = refine_cluster(&samples, &[0, 1], 0.4, 0.3);
    assert!(rc.situation.centroid.is_none());
    assert!(rc.situation.response_centroid.is_none());
}

#[test]
fn refine_quality_and_neff() {
    // 同质簇：valence 一致、成员相似 → 高质量
    let mut a = sample(1, &["x"], Some(vec![1.0, 0.0]), Some(vec![1.0, 0.0]));
    a.valence = 0.4;
    a.salience = 0.9;
    let mut b = sample(2, &["x"], Some(vec![0.95, 0.1]), Some(vec![1.0, 0.0]));
    b.valence = 0.4;
    b.salience = 0.7;
    let mut c = sample(3, &["x"], Some(vec![0.9, 0.2]), Some(vec![1.0, 0.0]));
    c.valence = 0.4;
    c.salience = 0.6;
    let rc = refine_cluster(&[a, b, c], &[0, 1, 2], 0.4, 0.3);
    assert!((rc.n_eff - 2.2).abs() < 1e-9, "n_eff = salience 加权和");
    assert!(rc.cohesion > 0.9, "内聚度应接近 1");
    assert!(rc.quality > 0.8, "质量 = 内聚度 × 一致性");
}

#[test]
fn refine_member_event_ids_chain() {
    let samples = vec![
        sample(10, &["x"], None, None),
        sample(11, &["x"], None, None),
    ];
    let rc = refine_cluster(&samples, &[0, 1], 0.4, 0.3);
    assert_eq!(rc.member_event_ids, vec![10, 11]);
}

#[test]
fn sample_from_event_keywords_split() {
    let mut ev = MemoryEvent::new("char-0001".into(), "标题".into(), "摘要".into(), 1, 2);
    ev.keywords = Some("加班, 累 ,,工作".into());
    ev.situation_strength = Some(4);
    ev.valence = -0.6;
    ev.presentation = Presentation::Subjective;
    let s = sample_from_event(&ev);
    assert_eq!(s.situation_keywords, vec!["加班", "累", "工作"]);
    assert_eq!(s.situation_strength, Some(4));
    assert!((s.valence + 0.6).abs() < 1e-9);
}

#[test]
fn dedup_keywords_lowercase_keep_order() {
    let raw = vec!["加班".to_string(), "加班".to_string(), "累".to_string()];
    assert_eq!(dedup_keywords(&raw), vec!["加班", "累"]);
}

// ---- 向量化与编排 ----

/// 确定性 mock embedding：基于文本哈希的固定维度向量（同文本同向量）。
struct HashEmbedder {
    model_info: ramaria_core::traits::EmbeddingModelInfo,
}

impl HashEmbedder {
    fn new(dim: usize) -> Self {
        Self {
            model_info: ramaria_core::traits::EmbeddingModelInfo {
                model_id: "hash-embedder".into(),
                dimension: dim,
            },
        }
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for HashEmbedder {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        let mut v = vec![0.0f32; self.model_info.dimension];
        for (i, b) in text.as_bytes().iter().enumerate() {
            v[i % self.model_info.dimension] += *b as f32 * 0.01;
        }
        Ok(v)
    }
    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }
    fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
        self.model_info.clone()
    }
    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }
    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }
    fn download_progress(&self) -> f64 {
        1.0
    }
    fn is_available(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn vectorize_fills_both_channels() {
    let mut evs: Vec<MemoryEvent> = Vec::new();
    let mut ev = MemoryEvent::new("char-0001".into(), "标题".into(), "摘要".into(), 1, 2);
    ev.id = 1;
    ev.paraphrase = Some("感到疲惫".into());
    ev.attitude = Some("加班好累".into());
    ev.keywords = Some("加班,累".into());
    evs.push(ev);

    let mut samples = evs.iter().map(sample_from_event).collect::<Vec<_>>();
    let embedder = HashEmbedder::new(4);
    vectorize(&mut samples, &evs, Some(&embedder))
        .await
        .expect("向量化成功");
    assert!(samples[0].reaction_vector.is_some(), "反应通道已填充");
    assert!(samples[0].situation_vector.is_some(), "情境通道已填充");
    // 反应通道文本 = paraphrase ⊕ attitude
    assert_ne!(samples[0].reaction_vector, samples[0].situation_vector);
}

#[tokio::test]
async fn vectorize_without_embedder_degrades_to_keywords() {
    let mut evs: Vec<MemoryEvent> = Vec::new();
    let mut ev = MemoryEvent::new("char-0001".into(), "标题".into(), "摘要".into(), 1, 2);
    ev.id = 1;
    ev.paraphrase = Some("感到疲惫".into());
    ev.keywords = Some("加班,累".into());
    evs.push(ev);

    let mut samples = evs.iter().map(sample_from_event).collect::<Vec<_>>();
    vectorize(&mut samples, &evs, None)
        .await
        .expect("无 embedding 不报错");
    assert!(samples[0].reaction_vector.is_none());
    assert!(samples[0].situation_vector.is_none());
}

#[tokio::test]
async fn cluster_events_end_to_end() {
    let mut evs: Vec<MemoryEvent> = Vec::new();
    for i in 0..4 {
        let mut ev = MemoryEvent::new(
            "char-0001".into(),
            format!("标题{i}"),
            format!("摘要{i}"),
            1,
            2,
        );
        ev.id = i;
        ev.paraphrase = Some("加班后感到疲惫".into());
        ev.attitude = Some("最近加班太多".into());
        ev.keywords = Some("加班,累,工作".into());
        ev.valence = -0.5;
        ev.presentation = Presentation::Subjective;
        ev.salience = 0.8;
        evs.push(ev);
    }
    let cfg = BehaviorConfig::default();
    let embedder = HashEmbedder::new(8);
    let clusterer = BehaviorClusterer::new(&cfg, Some(&embedder));
    let clusters = clusterer.cluster_events(&evs).await.expect("聚类成功");
    assert_eq!(clusters.len(), 1, "4 条同质事件应聚成 1 簇");
    assert_eq!(clusters[0].member_event_ids.len(), 4);
    assert!(clusters[0].situation.keywords.contains(&"加班".to_string()));
    assert!(clusters[0].situation.valence_mean < 0.0);
}

#[tokio::test]
async fn cluster_events_empty_returns_empty() {
    let cfg = BehaviorConfig::default();
    let clusterer = BehaviorClusterer::new(&cfg, None);
    let clusters = clusterer.cluster_events(&[]).await.expect("空输入成功");
    assert!(clusters.is_empty());
}

#[tokio::test]
async fn cluster_events_without_embedding_still_clusters_by_keywords() {
    // embedding 不可用：纯关键词 Jaccard 也能聚类（降级链）
    let mut evs: Vec<MemoryEvent> = Vec::new();
    for i in 0..4 {
        let mut ev = MemoryEvent::new(
            "char-0001".into(),
            format!("标题{i}"),
            format!("摘要{i}"),
            1,
            2,
        );
        ev.id = i;
        ev.keywords = Some("加班,累".into());
        evs.push(ev);
    }
    for i in 4..8 {
        let mut ev = MemoryEvent::new(
            "char-0001".into(),
            format!("标题{i}"),
            format!("摘要{i}"),
            1,
            2,
        );
        ev.id = i;
        ev.keywords = Some("猫,可爱".into());
        evs.push(ev);
    }
    let cfg = BehaviorConfig::default();
    let clusterer = BehaviorClusterer::new(&cfg, None);
    let clusters = clusterer.cluster_events(&evs).await.expect("降级聚类成功");
    assert_eq!(clusters.len(), 2, "纯关键词应分出两个簇");
}

#[tokio::test]
async fn cluster_events_high_outlier_retries_and_reports() {
    // 高孤立点比例（>60%）：编排层下调 θ_nb 重试后仍返回核心簇（不 panic、不阻塞）
    let mut evs: Vec<MemoryEvent> = Vec::new();
    for i in 0..4 {
        let mut ev = MemoryEvent::new(
            "char-0001".into(),
            format!("标题{i}"),
            format!("摘要{i}"),
            1,
            2,
        );
        ev.id = i;
        ev.keywords = Some("加班,累".into());
        evs.push(ev);
    }
    for k in 0..10 {
        let mut ev = MemoryEvent::new("char-0001".into(), format!("孤{k}"), format!("孤{k}"), 1, 2);
        ev.id = 100 + k;
        ev.keywords = Some(format!("互异话题{k}"));
        evs.push(ev);
    }
    let cfg = BehaviorConfig::default();
    let clusterer = BehaviorClusterer::new(&cfg, None);
    let clusters = clusterer.cluster_events(&evs).await.expect("重试后成功");
    // 4 个同质核心事件成 1 簇；10 个互异孤立点不产生规则
    assert_eq!(clusters.len(), 1, "只有核心簇产生规则");
    assert_eq!(clusters[0].member_event_ids.len(), 4);
}
