//! rust/crates/ramaria-memory/src/inference/clustering.rs - 态度语义聚类
//!
//! 设计特点:
//! - 对去情境化后的 attitude（paraphrase）embedding 进行密度聚类
//! - 使用余弦相似度作为距离度量（文本 embedding 的相似度体现在方向而非绝对距离）
//! - min_cluster_size=3 锁定，不暴露为可配置项
//! - 软聚类: 每条态度按归属强度分为核心样本(≥0.7)、边界样本(<0.7)、噪声样本
//! - 当前为简化实现（基于余弦相似度的密度聚类）， 接入真实 embedding 后可替换为 UMAP+HDBSCAN
//! - 纯数值计算，零 I/O，不依赖数据库或异步运行时

// =========================================================
// 配置与输出类型
// =========================================================

/// 态度聚类配置。
///
/// 职责:
/// - 集中管理聚类参数。
///
/// 字段约定:
/// - `min_cluster_size`: 最小簇大小，锁定为 3（对应算法文档中 HDBSCAN 的锁定参数）。
/// - `core_threshold`: 核心样本归属概率阈值，默认 0.7。
/// - `similarity_threshold`: 余弦相似度阈值——两样本相似度 > 此值视为同一簇候选，默认 0.5。
#[derive(Debug, Clone)]
pub struct ClusteringConfig {
    /// 最小簇大小（锁定为 3）
    pub min_cluster_size: usize,
    /// 核心样本归属概率阈值
    pub core_threshold: f64,
    /// 余弦相似度阈值
    pub similarity_threshold: f64,
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: 3,
            core_threshold: 0.7,
            similarity_threshold: 0.5,
        }
    }
}

/// 单条态度样本（用于聚类输入）。
///
/// 职责:
/// - 封装去情境化态度文本及其 embedding 向量。
/// - `source_index` 指向原始事件在输入列表中的位置，保证可追溯。
#[derive(Debug, Clone)]
pub struct AttitudeSample {
    /// 去情境化态度文本
    pub paraphrase: String,
    /// 文本 embedding
    pub embedding: Vec<f32>,
    /// 原始事件在输入列表中的索引
    pub source_index: usize,
}

/// 单条态度的聚类结果。
///
/// 职责:
/// - 记录该态度被分配到哪个簇，及其对各簇的归属概率。
#[derive(Debug, Clone)]
pub struct ClusterAssignment {
    /// 原始样本在输入列表中的索引
    pub source_index: usize,
    /// 主簇标签（0-based），噪声样本为 None
    pub primary_cluster: Option<usize>,
    /// 对各簇的归属概率（概率和 ≈ 1）
    pub probabilities: Vec<f64>,
    /// 归属层级: "core"/"edge"/"noise"
    pub tier: String,
}

/// 单个簇的描述。
///
/// 职责:
/// - 记录簇的结构信息，供 LLM 推断时参考。
#[derive(Debug, Clone)]
pub struct ClusterDescription {
    /// 簇索引（0-based）
    pub index: usize,
    /// 簇成员数
    pub size: usize,
    /// 核心样本的去情境化态度文本（供语义标签生成）
    pub core_paraphrases: Vec<String>,
    /// 边界样本的去情境化态度文本
    pub edge_paraphrases: Vec<String>,
    /// 簇中心向量（核心样本 embedding 均值）
    pub centroid: Vec<f32>,
}

/// 聚类完整输出。
#[derive(Debug, Clone)]
pub struct ClusteringResult {
    /// 每条态度的分配结果
    pub assignments: Vec<ClusterAssignment>,
    /// 簇描述列表
    pub clusters: Vec<ClusterDescription>,
    /// 簇数量
    pub cluster_count: usize,
}

// =========================================================
// 余弦相似度计算
// =========================================================

/// 计算两个向量的余弦相似度。
///
/// 公式: cos(θ) = (a·b) / (||a|| · ||b||)
///
/// 参数:
/// - `a`, `b`: 两个等长向量。
///
/// 返回:
/// - 余弦相似度 -1.0..1.0。若任一向量为零向量则返回 0.0。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len(), "向量长度必须相等");
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for i in 0..a.len() {
        let ai = a[i] as f64;
        let bi = b[i] as f64;
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }
    if norm_a < 1e-12 || norm_b < 1e-12 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

/// 计算所有样本间的余弦相似度矩阵。
///
/// 返回:
/// - (n × n) 的上三角填充相似度矩阵。对角线为 1.0。
pub fn build_similarity_matrix(samples: &[AttitudeSample]) -> Vec<Vec<f64>> {
    let n = samples.len();
    let mut matrix = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        matrix[i][i] = 1.0;
        for j in (i + 1)..n {
            let sim = cosine_similarity(&samples[i].embedding, &samples[j].embedding);
            matrix[i][j] = sim;
            matrix[j][i] = sim;
        }
    }
    matrix
}

// =========================================================
// 简化密度聚类（HDBSCAN 近似）
// =========================================================

/// 基于余弦相似度的简化密度聚类。
///
/// 算法:
/// 1. 构建相似度矩阵（n×n）。
/// 2. 对每个样本，统计与其余弦相似度 > `similarity_threshold` 的邻居数。
/// 3. 按邻居数降序扫描，将高密度样本及其邻居合并为簇。
/// 4. 簇大小 < `min_cluster_size` 的样本标记为噪声。
///
/// 参数:
/// - `samples`: 态度样本列表。
/// - `config`: 聚类配置。
///
/// 返回:
/// - ClusteringResult，包含软分配和簇描述。
pub fn simple_density_cluster(
    samples: &[AttitudeSample],
    config: &ClusteringConfig,
) -> ClusteringResult {
    let n = samples.len();
    if n == 0 {
        return ClusteringResult {
            assignments: Vec::new(),
            clusters: Vec::new(),
            cluster_count: 0,
        };
    }

    let sim_matrix = build_similarity_matrix(samples);

    // 统计每个样本的邻居数（相似度 > threshold）
    let neighbor_counts: Vec<usize> = (0..n)
        .map(|i| {
            sim_matrix[i]
                .iter()
                .enumerate()
                .filter(|(j, sim)| *j != i && **sim > config.similarity_threshold)
                .count()
        })
        .collect();

    // 按邻居数降序索引
    let mut indices: Vec<usize> = (0..n).collect();
    indices.sort_by(|a, b| neighbor_counts[*b].cmp(&neighbor_counts[*a]));

    // 聚类
    let mut labels: Vec<Option<usize>> = vec![None; n];
    let mut cluster_id = 0usize;

    for &i in &indices {
        if labels[i].is_some() {
            continue;
        }
        // 收集未标记邻居
        let mut neighbors: Vec<usize> = (0..n)
            .filter(|&j| {
                j != i && sim_matrix[i][j] > config.similarity_threshold && labels[j].is_none()
            })
            .collect();

        // 加上自身
        neighbors.push(i);

        // 检查是否满足最小簇大小
        if neighbors.len() >= config.min_cluster_size {
            for &j in &neighbors {
                labels[j] = Some(cluster_id);
            }
            cluster_id += 1;
        }
    }

    // 构建硬分配结果
    let cluster_count = cluster_id;

    // 对每个样本计算软分配概率（基于到各簇中心的最大相似度比例）
    let mut cluster_centroids: Vec<Vec<f32>> =
        vec![vec![0.0; samples[0].embedding.len()]; cluster_count];
    let mut cluster_sizes: Vec<usize> = vec![0; cluster_count];

    for (i, label) in labels.iter().enumerate() {
        if let Some(cid) = label
            && *cid < cluster_count
        {
            for (d, &val) in cluster_centroids[*cid]
                .iter_mut()
                .zip(samples[i].embedding.iter())
            {
                *d += val;
            }
            cluster_sizes[*cid] += 1;
        }
    }

    // 归一化簇中心
    for cid in 0..cluster_count {
        if cluster_sizes[cid] > 0 {
            let size = cluster_sizes[cid] as f32;
            for d in cluster_centroids[cid].iter_mut() {
                *d /= size;
            }
        }
    }

    // 收集簇的核心/边界文本
    let mut cluster_core: Vec<Vec<String>> = vec![Vec::new(); cluster_count];
    let mut cluster_edge: Vec<Vec<String>> = vec![Vec::new(); cluster_count];

    // 软分配
    let mut assignments = Vec::with_capacity(n);
    for i in 0..n {
        let primary = labels[i];

        // 计算到各簇中心的余弦相似度作为归属概率基础
        let raw_probs: Vec<f64> = if cluster_count > 0 {
            cluster_centroids
                .iter()
                .map(|centroid| cosine_similarity(&samples[i].embedding, centroid).max(0.0))
                .collect()
        } else {
            Vec::new()
        };

        // 归一化概率
        let prob_sum: f64 = raw_probs.iter().sum();
        let probabilities: Vec<f64> = if prob_sum > 0.0 {
            raw_probs.iter().map(|p| p / prob_sum).collect()
        } else if cluster_count > 0 {
            vec![1.0 / cluster_count as f64; cluster_count]
        } else {
            Vec::new()
        };

        // 确定归属层级
        let max_prob = probabilities
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, &p)| p)
            .unwrap_or(0.0);

        let tier = if primary.is_none() {
            "noise".to_string()
        } else if max_prob >= config.core_threshold {
            "core".to_string()
        } else {
            "edge".to_string()
        };

        // 收集核心/边界文本
        if let Some(cid) = primary
            && cid < cluster_count
        {
            if tier == "core" {
                cluster_core[cid].push(samples[i].paraphrase.clone());
            } else {
                cluster_edge[cid].push(samples[i].paraphrase.clone());
            }
        }

        assignments.push(ClusterAssignment {
            source_index: samples[i].source_index,
            primary_cluster: primary,
            probabilities,
            tier,
        });
    }

    // 构建簇描述
    let clusters: Vec<ClusterDescription> = (0..cluster_count)
        .map(|cid| ClusterDescription {
            index: cid,
            size: cluster_sizes[cid],
            core_paraphrases: cluster_core[cid].clone(),
            edge_paraphrases: cluster_edge[cid].clone(),
            centroid: cluster_centroids[cid].clone(),
        })
        .collect();

    ClusteringResult {
        assignments,
        clusters,
        cluster_count,
    }
}

/// 执行态度聚类的便捷入口。
///
/// 参数:
/// - `samples`: 态度样本列表（含 paraphrase 和 embedding）。
/// - `config`: 聚类配置。
///
/// 返回:
/// - ClusteringResult。
pub fn run_clustering(samples: &[AttitudeSample], config: &ClusteringConfig) -> ClusteringResult {
    simple_density_cluster(samples, config)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
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

    // ---- 余弦相似度 ----

    #[test]
    fn cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-10);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-10);
    }

    #[test]
    fn cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-10);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-10);
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
}
