//! crates/ramaria-memory/src/inference/clustering/tests.rs - //! crates/ramaria-memory/src/inference/clustering.rs - 态度语义聚类单元测试
//!
//! 设计特点:
//! - 位于 inference::clustering 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 clustering.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;

/// 构造测试用嵌入向量（简单 4 维）。
fn make_embedding(vals: &[f32]) -> Vec<f32> {
    vals.to_vec()
}

fn make_sample(index: usize, paraphrase: &str, embedding: Vec<f32>) -> AttitudeSample {
    AttitudeSample {
        paraphrase: paraphrase.to_string(),
        embedding,
        source_index: index,
    }
}

// ---- 相似度矩阵 ----

#[test]
fn similarity_matrix_symmetric() {
    let samples = vec![
        make_sample(0, "态度A", make_embedding(&[1.0, 0.0, 0.0])),
        make_sample(1, "态度B", make_embedding(&[0.0, 1.0, 0.0])),
    ];
    let matrix = build_similarity_matrix(&samples);
    assert_eq!(matrix.len(), 2);
    assert_eq!(matrix[0].len(), 2);
    assert!((matrix[0][0] - 1.0).abs() < 1e-10); // 自相似
    assert!((matrix[0][1]).abs() < 1e-10); // 正交
    assert_eq!(matrix[0][1], matrix[1][0]); // 对称
}

// ---- 聚类 ----

#[test]
fn clustering_empty_input() {
    let config = ClusteringConfig::default();
    let result = simple_density_cluster(&[], &config);
    assert_eq!(result.cluster_count, 0);
    assert!(result.assignments.is_empty());
    assert!(result.clusters.is_empty());
}

#[test]
fn clustering_single_sample() {
    let config = ClusteringConfig::default();
    let samples = vec![make_sample(0, "态度A", make_embedding(&[1.0, 0.0, 0.0]))];
    let result = simple_density_cluster(&samples, &config);
    // 单样本无法满足 min_cluster_size=3，应为噪声
    assert_eq!(result.cluster_count, 0);
    assert_eq!(result.assignments[0].tier, "noise");
}

#[test]
fn clustering_three_similar_samples() {
    let config = ClusteringConfig::default();
    // 三个高度相似的样本（方向接近）
    let samples = vec![
        make_sample(0, "态度A", make_embedding(&[1.0, 0.1, 0.0])),
        make_sample(1, "态度B", make_embedding(&[0.95, 0.05, 0.0])),
        make_sample(2, "态度C", make_embedding(&[0.9, 0.15, 0.0])),
    ];
    let result = simple_density_cluster(&samples, &config);
    // 三者应该聚为 1 个簇
    assert_eq!(result.cluster_count, 1);
    assert_eq!(result.clusters[0].size, 3);
    // 所有样本应被分配
    for a in &result.assignments {
        assert!(a.primary_cluster.is_some());
        assert!(a.probabilities.len() == 1);
    }
}

#[test]
fn clustering_two_distinct_groups() {
    let config = ClusteringConfig::default();
    // 两组明显不同的样本
    let samples = vec![
        // Group 1: 方向接近 [1,0,0]
        make_sample(0, "G1-A", make_embedding(&[1.0, 0.0, 0.0])),
        make_sample(1, "G1-B", make_embedding(&[0.9, 0.1, 0.0])),
        make_sample(2, "G1-C", make_embedding(&[0.95, -0.05, 0.0])),
        // Group 2: 方向接近 [0,1,0]
        make_sample(3, "G2-A", make_embedding(&[0.0, 1.0, 0.0])),
        make_sample(4, "G2-B", make_embedding(&[0.1, 0.9, 0.0])),
        make_sample(5, "G2-C", make_embedding(&[-0.05, 0.95, 0.0])),
    ];
    let result = simple_density_cluster(&samples, &config);
    // 应形成 2 个簇
    assert!(
        result.cluster_count >= 2,
        "应至少有2个簇，实际有{}个",
        result.cluster_count
    );
}

#[test]
fn clustering_below_min_size_is_noise() {
    let config = ClusteringConfig::default();
    // 只有 2 个相似的样本，不满足 min_cluster_size=3
    let samples = vec![
        make_sample(0, "A", make_embedding(&[1.0, 0.0, 0.0])),
        make_sample(1, "B", make_embedding(&[0.95, 0.05, 0.0])),
    ];
    let result = simple_density_cluster(&samples, &config);
    assert_eq!(result.cluster_count, 0);
    for a in &result.assignments {
        assert_eq!(a.tier, "noise");
    }
}

#[test]
fn clustering_soft_assignment_probabilities_sum_to_one() {
    let config = ClusteringConfig::default();
    let samples = vec![
        make_sample(0, "G1-A", make_embedding(&[1.0, 0.0, 0.0])),
        make_sample(1, "G1-B", make_embedding(&[0.9, 0.1, 0.0])),
        make_sample(2, "G1-C", make_embedding(&[0.95, -0.05, 0.0])),
        make_sample(3, "G2-A", make_embedding(&[0.0, 1.0, 0.0])),
        make_sample(4, "G2-B", make_embedding(&[0.1, 0.9, 0.0])),
        make_sample(5, "G2-C", make_embedding(&[-0.05, 0.95, 0.0])),
    ];
    let result = simple_density_cluster(&samples, &config);
    for a in &result.assignments {
        if !a.probabilities.is_empty() {
            let sum: f64 = a.probabilities.iter().sum();
            assert!((sum - 1.0).abs() < 0.01, "概率和应接近1，实际={}", sum);
        }
    }
}

#[test]
fn clustering_tier_assignment() {
    let config = ClusteringConfig::default();
    // 三样本 + 一个明显不同的噪声样本
    let samples = vec![
        make_sample(0, "核心A", make_embedding(&[1.0, 0.0, 0.0])),
        make_sample(1, "核心B", make_embedding(&[0.9, 0.1, 0.0])),
        make_sample(2, "核心C", make_embedding(&[0.95, -0.05, 0.0])),
        make_sample(3, "噪声", make_embedding(&[0.0, 0.0, 1.0])),
    ];
    let result = simple_density_cluster(&samples, &config);
    // 前三者应形成簇，第四个为噪声
    let noise = result.assignments.iter().find(|a| a.source_index == 3);
    assert!(noise.is_some());
    // 核心样本 tier 应为 "core" 或 "edge"
    for i in 0..3 {
        let a = &result.assignments[i];
        assert!(
            a.tier == "core" || a.tier == "edge",
            "样本 {} 的 tier 应为 core 或 edge，实际为 {}",
            i,
            a.tier
        );
    }
}

#[test]
fn run_clustering_convenience() {
    let config = ClusteringConfig::default();
    let samples = vec![
        make_sample(0, "A", make_embedding(&[1.0, 0.0, 0.0])),
        make_sample(1, "B", make_embedding(&[0.9, 0.1, 0.0])),
        make_sample(2, "C", make_embedding(&[0.95, -0.05, 0.0])),
    ];
    let result = run_clustering(&samples, &config);
    assert_eq!(result.cluster_count, 1);
}

// ---- 语义标签生成 ----

#[test]
fn semantic_label_empty_phrases() {
    let desc = ClusterDescription {
        index: 0,
        size: 0,
        core_paraphrases: vec![],
        edge_paraphrases: vec![],
        centroid: vec![0.0; 4],
    };
    let label = generate_semantic_label(&desc);
    assert_eq!(label, "未命名簇");
}

#[test]
fn semantic_label_common_phrase() {
    let desc = ClusterDescription {
        index: 0,
        size: 3,
        core_paraphrases: vec![
            "对加班感到疲惫和无奈".to_string(),
            "加班导致身心透支".to_string(),
            "频繁加班影响了生活质量".to_string(),
        ],
        edge_paraphrases: vec![],
        centroid: vec![1.0; 4],
    };
    let label = generate_semantic_label(&desc);
    // "加班" 出现在所有 3 条中（100% ≥ 50%），应被提取
    assert!(
        label.contains("加班"),
        "标签应包含'加班'，实际为: {}",
        label
    );
}

#[test]
fn semantic_label_no_high_freq_phrase() {
    let desc = ClusterDescription {
        index: 0,
        size: 5,
        core_paraphrases: vec![
            "喜欢户外跑步".to_string(),
            "对编程有热情".to_string(),
            "周末喜欢看电影".to_string(),
            "享受独自旅行".to_string(),
            "热爱阅读历史书籍".to_string(),
        ],
        edge_paraphrases: vec![],
        centroid: vec![1.0; 4],
    };
    let label = generate_semantic_label(&desc);
    // 无短语达到 50%（3/5），应取频次最高的短语
    assert!(!label.is_empty());
    assert_ne!(label, "未命名簇");
}

#[test]
fn semantic_label_single_paraphrase() {
    let desc = ClusterDescription {
        index: 0,
        size: 1,
        core_paraphrases: vec!["对于权威的否定感到强烈抵触，认为自己的专业判断被忽视".to_string()],
        edge_paraphrases: vec![],
        centroid: vec![0.0; 4],
    };
    let label = generate_semantic_label(&desc);
    // 单条核心样本：短语出现率 100%，应提取其关键短语
    assert!(!label.is_empty());
    assert_ne!(label, "未命名簇");
}

#[test]
fn semantic_label_deduplication_within_paraphrase() {
    let desc = ClusterDescription {
        index: 0,
        size: 2,
        core_paraphrases: vec![
            "加班加班真的很累".to_string(),
            "加班影响了我的生活".to_string(),
        ],
        edge_paraphrases: vec![],
        centroid: vec![0.0; 4],
    };
    let label = generate_semantic_label(&desc);
    // "加班" 在两条中各出现，但同一 paraphrase 内应去重计数
    assert!(
        label.contains("加班"),
        "标签应包含'加班'，实际为: {}",
        label
    );
}

// ---- 跨版本匹配 ----

fn make_hist_snapshot(
    id: i64,
    label: &str,
    embedding: Vec<f32>,
    category: &str,
) -> HistoricalSnapshot {
    HistoricalSnapshot {
        id,
        semantic_label: Some(label.to_string()),
        semantic_label_embedding: Some(ramaria_core::types::ClusterSnapshot::serialize_embedding(
            &embedding,
        )),
        category: category.to_string(),
        created_at: 1000 * id,
    }
}

#[test]
fn cross_version_match_exact() {
    let current_emb = vec![1.0_f32, 0.0, 0.0, 0.0];
    let historical = vec![make_hist_snapshot(
        1,
        "工作压力",
        current_emb.clone(),
        "工作",
    )];
    let result = match_clusters_cross_version(&current_emb, &historical, 0.85);
    assert_eq!(result.total_historical, 1);
    assert_eq!(result.matched_count, 1);
    assert!(result.best_match.is_some());
    let best = result.best_match.unwrap();
    assert!(best.is_match);
    assert!(
        (best.similarity - 1.0).abs() < 1e-6,
        "相同向量相似度应为1.0，实际={}",
        best.similarity
    );
}

#[test]
fn cross_version_match_no_match() {
    let current_emb = vec![1.0_f32, 0.0, 0.0, 0.0];
    let historical = vec![make_hist_snapshot(
        1,
        "社交活跃",
        vec![0.0, 1.0, 0.0, 0.0],
        "社交",
    )];
    let result = match_clusters_cross_version(&current_emb, &historical, 0.85);
    assert_eq!(result.total_historical, 1);
    assert_eq!(result.matched_count, 0);
}

#[test]
fn cross_version_match_empty_history() {
    let current_emb = vec![1.0_f32, 0.0, 0.0];
    let result = match_clusters_cross_version(&current_emb, &[], 0.85);
    assert_eq!(result.total_historical, 0);
    assert!(result.matches.is_empty());
}

#[test]
fn cross_version_match_empty_current_embedding() {
    let historical = vec![make_hist_snapshot(1, "测试", vec![1.0, 0.0], "工作")];
    let result = match_clusters_cross_version(&[], &historical, 0.85);
    assert_eq!(result.total_historical, 1);
    assert!(result.matches.is_empty());
}

#[test]
fn cross_version_match_dimension_mismatch() {
    let current_emb = vec![1.0_f32, 0.0, 0.0]; // 3 维
    let historical = vec![make_hist_snapshot(1, "测试", vec![1.0, 0.0], "工作")]; // 2 维
    let result = match_clusters_cross_version(&current_emb, &historical, 0.85);
    // 维度不匹配应被跳过
    assert_eq!(result.matches.len(), 0);
}

#[test]
fn cross_version_match_multiple_historical() {
    let current_emb = vec![1.0_f32, 0.0, 0.0, 0.0];
    let historical = vec![
        make_hist_snapshot(1, "不相关", vec![0.0, 1.0, 0.0, 0.0], "社交"),
        make_hist_snapshot(2, "高度相似", vec![0.95, 0.05, 0.0, 0.0], "工作"),
        make_hist_snapshot(3, "中度相似", vec![0.7, 0.3, 0.0, 0.0], "工作"),
    ];
    let result = match_clusters_cross_version(&current_emb, &historical, 0.85);
    assert_eq!(result.total_historical, 3);
    // 第二个快照相似度最高，应先出现
    let best = result.best_match.unwrap();
    assert_eq!(best.snapshot_id, 2);
    assert!(best.similarity > 0.9);
}

#[test]
fn cross_version_match_with_null_embedding() {
    let current_emb = vec![1.0_f32, 0.0];
    let historical = vec![
        HistoricalSnapshot {
            id: 1,
            semantic_label: Some("无embedding".into()),
            semantic_label_embedding: None,
            category: "工作".into(),
            created_at: 1000,
        },
        make_hist_snapshot(2, "正常", vec![0.9, 0.1], "工作"),
    ];
    let result = match_clusters_cross_version(&current_emb, &historical, 0.85);
    // 只有 id=2 的快照有效
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].snapshot_id, 2);
}

/// 0.85 边界断言：0.85 匹配、0.84 不匹配。
///
/// 用单位向量构造精确余弦相似度：对目标余弦 `c`，向量 `[c, sqrt(1-c²)]` 是单位向量，
/// 与 `[1, 0]` 的余弦 = `c`。`sqrt` 在 f64 下计算，避免手写近似值精度不足。
/// 为避免 f32 舍入导致余弦恰好落在阈值两侧的脆弱断言，匹配侧取 0.8501（稳 ≥ 0.85）、
/// 不匹配侧取 0.8499（稳 < 0.85），验证阈值 0.85 的硬边界语义。
#[test]
fn cross_version_match_threshold_boundary_085() {
    let query = vec![1.0_f32, 0.0];
    let mk = |c: f64| vec![c as f32, (1.0 - c * c).sqrt() as f32];

    // 余弦 ≈ 0.8501（稳 ≥ 0.85）→ 判定匹配
    let match_emb = mk(0.8501);
    let match_result =
        match_clusters_cross_version(&query, &[make_hist_snapshot(1, "t", match_emb, "c")], 0.85);
    assert_eq!(match_result.matched_count, 1, "≥0.85 应判定为匹配");
    let m = match_result.best_match.unwrap();
    assert!(m.is_match);
    assert!(m.similarity >= 0.85, "sim={:.4}", m.similarity);

    // 余弦 ≈ 0.8499（稳 < 0.85）→ 判定不匹配
    let no_match_emb = mk(0.8499);
    let no_result = match_clusters_cross_version(
        &query,
        &[make_hist_snapshot(2, "t2", no_match_emb, "c")],
        0.85,
    );
    assert_eq!(no_result.matched_count, 0, "<0.85 不应判定为匹配");
    let nm = no_result.best_match.unwrap();
    assert!(!nm.is_match);
    assert!(nm.similarity < 0.85, "sim={:.4}", nm.similarity);
}

/// 阈值常量锁定 0.85。
#[test]
fn cross_version_threshold_constant_is_085() {
    assert!((CROSS_VERSION_MATCH_THRESHOLD - 0.85).abs() < f64::EPSILON);
}

/// 配置开关回退：关闭升级（threshold=0.75）时，相似度 0.80 的簇被判定为匹配；
/// 开启 0.85 后同一簇不匹配。
///
/// 断言阈值收紧带来的行为差异，供回归对照。
#[test]
fn cross_version_threshold_loose_fallback_behavior() {
    // 与 [1,0] 余弦相似度 = 0.80 的向量
    let emb = vec![0.80_f32, 0.6_f32];
    let snap = make_hist_snapshot(1, "t", emb, "c");

    // 宽松阈值 0.75：0.80 ≥ 0.75 → 匹配
    let loose = match_clusters_cross_version(&[1.0_f32, 0.0], std::slice::from_ref(&snap), 0.75);
    assert_eq!(loose.matched_count, 1, "阈值 0.75 时 0.80 应匹配");

    // 收紧阈值 0.85：0.80 < 0.85 → 不匹配
    let tight =
        match_clusters_cross_version(&[1.0_f32, 0.0], &[snap], CROSS_VERSION_MATCH_THRESHOLD);
    assert_eq!(tight.matched_count, 0, "阈值 0.85 时 0.80 不应匹配");
}
