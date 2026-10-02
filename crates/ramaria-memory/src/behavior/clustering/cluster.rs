//! crates/ramaria-memory/src/behavior/clustering/cluster.rs - 密度聚类
//!
//! 设计特点:
//! - 相似度矩阵三路融合，邻居判定 sim ≥ θ_nb（不含自己）。
//! - 核心样本邻居数 ≥ min_cluster_size，核心骨架按密度可达 BFS 连接成连通分量。
//! - 非核心样本软分配到相似度最高的邻接核心所在簇，无核心邻居则为孤立点。
//! - 输出每条样本的 tier（core / edge / noise）与孤立点比例。

use super::sample::BehaviorSample;
use super::similarity::fused_similarity;

// =========================================================
// 密度聚类
// =========================================================

/// 单样本的簇分配结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterAssignment {
    /// 样本在输入列表中的索引
    pub sample_index: usize,
    /// 归属簇 id（None = 孤立点，不入簇）
    pub cluster_id: Option<usize>,
    /// 归属层级: core（核心）/ edge（边界）/ noise（孤立）
    pub tier: &'static str,
}

/// 原始簇（成员索引集合）。
#[derive(Debug, Clone, PartialEq)]
pub struct RawCluster {
    /// 簇成员样本索引（核心 + 边界）
    pub member_indices: Vec<usize>,
    /// 核心成员样本索引
    pub core_indices: Vec<usize>,
}

/// 密度聚类结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DensityClusterResult {
    /// 每条样本的分配
    pub assignments: Vec<ClusterAssignment>,
    /// 簇列表（与 assignment.cluster_id 对应）
    pub clusters: Vec<RawCluster>,
    /// 孤立点比例（0.0..1.0，失败模式检查输入）
    pub outlier_ratio: f64,
    /// 簇数量
    pub cluster_count: usize,
}

/// 密度聚类（v3.1 §4.2 Step 2.3）。
///
/// 算法:
/// 1. 构建相似度矩阵（三路融合，β 权重）。
/// 2. 邻居: 与其他样本 sim ≥ θ_nb。
/// 3. 核心样本: 邻居数 ≥ min_cluster_size。
/// 4. 簇生长: 核心样本按"密度可达"传递连接成连通分量（核心骨架）。
/// 5. 边界软分配: 非核心样本邻接 ≥1 个核心样本 → 分配到 sim 最高的核心所在簇。
/// 6. 孤立点: 非核心且无核心邻居 → 不入簇。
///
/// 参数:
/// - `samples`: 聚类输入样本。
/// - `theta_nb`: 邻域相似度阈值。
/// - `min_cluster_size`: 核心样本最小邻居数。
/// - `beta1` / `beta2`: 三路融合权重。
pub fn density_cluster(
    samples: &[BehaviorSample],
    theta_nb: f64,
    min_cluster_size: usize,
    beta1: f64,
    beta2: f64,
) -> DensityClusterResult {
    let n = samples.len();
    if n == 0 {
        return DensityClusterResult {
            assignments: Vec::new(),
            clusters: Vec::new(),
            outlier_ratio: 0.0,
            cluster_count: 0,
        };
    }

    // 相似度矩阵（上三角 + 对角线 1.0）
    let mut sim = vec![vec![0.0f64; n]; n];
    for i in 0..n {
        sim[i][i] = 1.0;
        for j in (i + 1)..n {
            let s = fused_similarity(&samples[i], &samples[j], beta1, beta2);
            sim[i][j] = s;
            sim[j][i] = s;
        }
    }

    // 邻居判定（不含自己）
    let is_neighbor = |a: usize, b: usize| a != b && sim[a][b] >= theta_nb;

    // 核心样本判定
    let core: Vec<bool> = (0..n)
        .map(|i| (0..n).filter(|&j| is_neighbor(i, j)).count() >= min_cluster_size)
        .collect();

    // 核心样本的密度可达连通分量（BFS 骨架）
    let mut core_cluster: Vec<Option<usize>> = vec![None; n];
    let mut cluster_count = 0usize;
    let mut cluster_cores: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        if !core[i] || core_cluster[i].is_some() {
            continue;
        }
        // BFS：经核心邻居传递（密度可达）
        let mut stack = vec![i];
        core_cluster[i] = Some(cluster_count);
        let mut members: Vec<usize> = Vec::new();
        while let Some(cur) = stack.pop() {
            members.push(cur);
            for j in 0..n {
                if core[j] && core_cluster[j].is_none() && is_neighbor(cur, j) {
                    core_cluster[j] = Some(cluster_count);
                    stack.push(j);
                }
            }
        }
        cluster_cores.push(members);
        cluster_count += 1;
    }

    // 边界软分配 + 孤立点判定
    let mut assignments: Vec<ClusterAssignment> = Vec::with_capacity(n);
    let mut member_indices: Vec<Vec<usize>> = vec![Vec::new(); cluster_count];
    for i in 0..n {
        if let Some(cid) = core_cluster[i] {
            member_indices[cid].push(i);
            assignments.push(ClusterAssignment {
                sample_index: i,
                cluster_id: Some(cid),
                tier: "core",
            });
            continue;
        }
        // 非核心：找邻接的核心样本，分配到 sim 最高者所在簇
        let mut best: Option<(usize, f64)> = None;
        for j in 0..n {
            if core[j] && is_neighbor(i, j) && best.map(|(_, s)| sim[i][j] > s).unwrap_or(true) {
                best = Some((core_cluster[j].unwrap_or(0), sim[i][j]));
            }
        }
        match best {
            Some((cid, _)) => {
                member_indices[cid].push(i);
                assignments.push(ClusterAssignment {
                    sample_index: i,
                    cluster_id: Some(cid),
                    tier: "edge",
                });
            }
            None => {
                // 孤立点：不入簇
                assignments.push(ClusterAssignment {
                    sample_index: i,
                    cluster_id: None,
                    tier: "noise",
                });
            }
        }
    }

    let clusters: Vec<RawCluster> = (0..cluster_count)
        .map(|cid| RawCluster {
            member_indices: std::mem::take(&mut member_indices[cid]),
            core_indices: cluster_cores[cid].clone(),
        })
        .collect();

    let outlier_count = assignments
        .iter()
        .filter(|a| a.cluster_id.is_none())
        .count();
    DensityClusterResult {
        assignments,
        clusters,
        outlier_ratio: outlier_count as f64 / n as f64,
        cluster_count,
    }
}
