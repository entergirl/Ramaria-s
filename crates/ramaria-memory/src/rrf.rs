//! crates/ramaria-memory/src/rrf.rs - Ramaria RRF 多通道融合模块
//!
//! 设计特点:
//! - 实现标准 Reciprocal Rank Fusion (RRF) 融合算法
//! - 支持 2-4 通道: 向量检索 + BM25 关键词 + 知识图谱 + 关键词镜像（扩展）
//! - 各通道权重和 RRF 平滑系数 K 可独立配置
//! - 未出现在某通道中的文档使用惩罚排名 (penalty rank)
//! - 纯数学模块，零 I/O，不依赖数据库或异步运行时
//!
//! 关键词镜像通道说明:
//! - `rrf_fuse_with_keyword` 处理含关键词镜像（KeywordService CompositeIndex）
//!   的融合场景；关键词通道缺席时继续使用既有单/双/三通道函数（行为不变）。
//! - 新增通道不会改变既有向量/BM25/图谱三通道的贡献公式（向量权重恒 1，
//!   bm25/graph 沿用各自权重），仅在关键词通道有结果时为相关文档追加一项。

use std::collections::HashMap;

// =========================================================
// 数据类型
// =========================================================

/// 单个检索通道的排序结果。
///
/// 职责:
/// - 封装某通道返回的有序文档列表。
/// - 每个文档以 (id, raw_score) 形式表示，按相关性降序排列。
///
/// 字段约定:
/// - `results`: 按该通道相关性降序排列的文档，排序位置即为其 rank。
/// - `id` 应为跨通道可比的唯一标识 (如记忆的 UUID 或 database id)。
#[derive(Debug, Clone)]
pub struct ChannelResult<I: Clone + std::hash::Hash + Eq> {
    /// 该通道的排序文档列表，索引 0 为 rank=1
    pub results: Vec<(I, f64)>,
}

/// RRF 融合配置。
///
/// 职责:
/// - 集中管理 RRF 融合的全部参数。
///
/// 字段约定:
/// - `k`: RRF 平滑系数，防止高 rank 的分数被过度惩罚。标准取值 60。
/// - `bm25_weight`: BM25 通道相对于向量通道的权重，默认 1.0。
/// - `graph_weight`: 图谱通道相对于向量通道的权重，默认 0.8。
/// - `keyword_weight`: 关键词镜像通道相对于向量通道的权重，默认 1.0。
/// - `top_k`: 融合后返回的最大文档数。
#[derive(Debug, Clone)]
pub struct RrfConfig {
    /// RRF 平滑系数 k (默认 60)
    pub k: f64,
    /// BM25 通道权重
    pub bm25_weight: f64,
    /// 图谱通道权重
    pub graph_weight: f64,
    /// 关键词镜像通道权重
    pub keyword_weight: f64,
    /// 融合后返回的最大结果条数
    pub top_k: usize,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            k: 60.0,
            bm25_weight: 1.0,
            graph_weight: 0.8,
            keyword_weight: 1.0,
            top_k: 5,
        }
    }
}

/// RRF 融合后的单条结果。
///
/// 职责:
/// - 封装融合后的一条文档及其在各通道中的原始分数。
///
/// 字段约定:
/// - `doc_id`: 文档唯一标识。
/// - `rrf_score`: 融合后的 RRF 总分数。
/// - `vector_raw_score`: 在向量通道中的原始相关度分数。None 表示不出现在该通道。
/// - `bm25_raw_score`: 在 BM25 通道中的原始相关度分数。None 表示不出现在该通道。
/// - `graph_raw_score`: 在图谱通道中的原始相关度分数。None 表示不出现在该通道。
#[derive(Debug, Clone)]
pub struct FusedResult<I> {
    /// 文档标识
    pub doc_id: I,
    /// RRF 融合分数
    pub rrf_score: f64,
    /// 向量通道原始相关度分数
    pub vector_raw_score: Option<f64>,
    /// BM25 通道原始相关度分数
    pub bm25_raw_score: Option<f64>,
    /// 图谱通道原始相关度分数
    pub graph_raw_score: Option<f64>,
}

// =========================================================
// 核心融合函数
// =========================================================

/// 计算惩罚排名。
///
/// 当某文档不在某通道的返回结果中时，使用一个较大的排名作为惩罚。
///
/// 公式: penalty_rank = top_k * 2 + 1
///
/// 说明:
/// - `top_k` 是融合后期望返回的最大条数。
/// - 惩罚排名应大于所有实际出现的排名 (实际 rank ≤ result_count)。
fn penalty_rank(top_k: usize) -> f64 {
    (top_k * 2 + 1) as f64
}

/// 对向量检索通道执行 RRF 融合。
///
/// 用法:
/// - 当只有向量通道 (无 BM25、无图谱) 时使用。
///
/// 参数:
/// - `vector_results`: 向量通道的排序结果。
/// - `config`: RRF 融合配置。
///
/// 返回:
/// - 按 RRF 分数降序排列的融合结果，最多 `config.top_k` 条。
pub fn rrf_single_channel<I: Clone + std::hash::Hash + Eq + std::fmt::Debug>(
    vector_results: &ChannelResult<I>,
    config: &RrfConfig,
) -> Vec<FusedResult<I>> {
    let mut fused: Vec<FusedResult<I>> = vector_results
        .results
        .iter()
        .enumerate()
        .map(|(idx, (id, score))| {
            let rank = (idx + 1) as f64;
            let rrf_score = 1.0 / (config.k + rank);
            FusedResult {
                doc_id: id.clone(),
                rrf_score,
                vector_raw_score: Some(*score),
                bm25_raw_score: None,
                graph_raw_score: None,
            }
        })
        .collect();

    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(config.top_k);
    fused
}

/// 对向量 + BM25 双通道执行 RRF 融合。
///
/// 用法:
/// - 标准双通道融合场景。
///
/// 公式:
/// - RRF_score = 1.0 / (k + v_rank) + bm25_weight / (k + b_rank)
///
/// 参数:
/// - `vector_results`: 向量通道的排序结果。
/// - `bm25_results`: BM25 通道的排序结果。
/// - `config`: RRF 融合配置。
///
/// 返回:
/// - 按 RRF 分数降序排列的融合结果，最多 `config.top_k` 条。
pub fn rrf_two_channels<I: Clone + std::hash::Hash + Eq + std::fmt::Debug>(
    vector_results: &ChannelResult<I>,
    bm25_results: &ChannelResult<I>,
    config: &RrfConfig,
) -> Vec<FusedResult<I>> {
    let penalty = penalty_rank(config.top_k);

    // 构建 BM25 排名映射: doc_id → rank
    let bm25_ranks: HashMap<&I, (f64, f64)> = bm25_results
        .results
        .iter()
        .enumerate()
        .map(|(idx, (id, score))| (id, ((idx + 1) as f64, *score)))
        .collect();

    // 收集所有出现的文档 ID（去重，保持首次出现顺序）
    let mut seen = std::collections::HashSet::new();
    let mut all_ids: Vec<&I> = Vec::new();
    for (id, _) in vector_results
        .results
        .iter()
        .chain(bm25_results.results.iter())
    {
        if seen.insert(id) {
            all_ids.push(id);
        }
    }

    // 构建向量排名映射
    let vector_ranks: HashMap<&I, (f64, f64)> = vector_results
        .results
        .iter()
        .enumerate()
        .map(|(idx, (id, score))| (id, ((idx + 1) as f64, *score)))
        .collect();

    let mut fused: Vec<FusedResult<I>> = all_ids
        .iter()
        .map(|id| {
            let (v_rank, v_score) = vector_ranks
                .get(id)
                .map(|(r, s)| (*r, Some(*s)))
                .unwrap_or((penalty, None));

            let (b_rank, b_score) = bm25_ranks
                .get(id)
                .map(|(r, s)| (*r, Some(*s)))
                .unwrap_or((penalty, None));

            let rrf_score = 1.0 / (config.k + v_rank) + config.bm25_weight / (config.k + b_rank);

            FusedResult {
                doc_id: (*id).clone(),
                rrf_score,
                vector_raw_score: v_score,
                bm25_raw_score: b_score,
                graph_raw_score: None,
            }
        })
        .collect();

    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(config.top_k);
    fused
}

/// 对向量 + BM25 + 图谱三通道执行 RRF 融合。
///
/// 用法:
/// - 完整的三通道融合场景。
///
/// 公式:
/// - RRF_score = 1.0/(k+v_rank) + bm25_weight/(k+b_rank) + graph_weight/(k+g_rank)
///
/// 参数:
/// - `vector_results`: 向量通道结果。
/// - `bm25_results`: BM25 通道结果。
/// - `graph_results`: 图谱通道结果。
/// - `config`: RRF 融合配置。
///
/// 返回:
/// - 按 RRF 分数降序排列的融合结果，最多 `config.top_k` 条。
pub fn rrf_fuse<I: Clone + std::hash::Hash + Eq + std::fmt::Debug>(
    vector_results: &ChannelResult<I>,
    bm25_results: &ChannelResult<I>,
    graph_results: &ChannelResult<I>,
    config: &RrfConfig,
) -> Vec<FusedResult<I>> {
    let penalty = penalty_rank(config.top_k);

    // 构建三通道排名映射
    let vector_ranks: HashMap<&I, (f64, f64)> = vector_results
        .results
        .iter()
        .enumerate()
        .map(|(idx, (id, score))| (id, ((idx + 1) as f64, *score)))
        .collect();

    let bm25_ranks: HashMap<&I, (f64, f64)> = bm25_results
        .results
        .iter()
        .enumerate()
        .map(|(idx, (id, score))| (id, ((idx + 1) as f64, *score)))
        .collect();

    let graph_ranks: HashMap<&I, (f64, f64)> = graph_results
        .results
        .iter()
        .enumerate()
        .map(|(idx, (id, score))| (id, ((idx + 1) as f64, *score)))
        .collect();

    // 收集所有出现的文档 ID (保持首次出现顺序)
    let mut seen = std::collections::HashSet::new();
    let mut all_ids: Vec<&I> = Vec::new();
    for results in [
        &vector_results.results,
        &bm25_results.results,
        &graph_results.results,
    ] {
        for (id, _) in results {
            if seen.insert(id) {
                all_ids.push(id);
            }
        }
    }

    let mut fused: Vec<FusedResult<I>> = all_ids
        .iter()
        .map(|id| {
            let (v_rank, v_score) = vector_ranks
                .get(id)
                .map(|(r, s)| (*r, Some(*s)))
                .unwrap_or((penalty, None));

            let (b_rank, b_score) = bm25_ranks
                .get(id)
                .map(|(r, s)| (*r, Some(*s)))
                .unwrap_or((penalty, None));

            let (g_rank, g_score) = graph_ranks
                .get(id)
                .map(|(r, s)| (*r, Some(*s)))
                .unwrap_or((penalty, None));

            let rrf_score = 1.0 / (config.k + v_rank)
                + config.bm25_weight / (config.k + b_rank)
                + config.graph_weight / (config.k + g_rank);

            FusedResult {
                doc_id: (*id).clone(),
                rrf_score,
                vector_raw_score: v_score,
                bm25_raw_score: b_score,
                graph_raw_score: g_score,
            }
        })
        .collect();

    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(config.top_k);
    fused
}

// =========================================================
// 含关键词镜像通道的融合
// =========================================================

/// 向量 + BM25 + 图谱 + 关键词镜像四通道 RRF 融合。
///
/// 用法:
/// - 当关键词镜像（KeywordService CompositeIndex）作为第四检索通道产生结果时使用；
///   关键词通道缺席时继续走既有单/双/三通道函数（行为不变）。
///
/// 公式:
/// - `RRF_score = [向量项] + bm25_weight·[BM25项] + graph_weight·[图谱项]
///                + keyword_weight·[关键词项]`
/// - 文档未出现在某通道时，该项以惩罚排名计；该通道整体缺席则不产生该项。
/// - 向量通道权重恒为 1（与既有融合公式一致，保证三通道既有贡献延续）。
///
/// 参数:
/// - `vector_results`: 向量通道结果（缺席传 None）。
/// - `bm25_results`: BM25 通道结果（缺席传 None）。
/// - `graph_results`: 图谱通道结果（缺席传 None）。
/// - `keyword_results`: 关键词镜像通道结果（调用方保证有数据）。
/// - `config`: RRF 融合配置（含 `keyword_weight`）。
///
/// 返回:
/// - 按 RRF 分数降序排列的融合结果，最多 `config.top_k` 条。
pub fn rrf_fuse_with_keyword<I: Clone + std::hash::Hash + Eq + std::fmt::Debug>(
    vector_results: Option<&ChannelResult<I>>,
    bm25_results: Option<&ChannelResult<I>>,
    graph_results: Option<&ChannelResult<I>>,
    keyword_results: &ChannelResult<I>,
    config: &RrfConfig,
) -> Vec<FusedResult<I>> {
    let penalty = penalty_rank(config.top_k);

    // 各通道排名映射：doc_id → (rank, raw_score)
    let vector_ranks = vector_results.map(channel_rank_map);
    let bm25_ranks = bm25_results.map(channel_rank_map);
    let graph_ranks = graph_results.map(channel_rank_map);
    let keyword_ranks: Option<std::collections::HashMap<&I, (f64, f64)>> =
        Some(channel_rank_map(keyword_results));

    // 收集所有出现的文档 ID（去重，保持首次出现顺序：vector → bm25 → graph → keyword）
    let mut seen = std::collections::HashSet::new();
    let mut all_ids: Vec<&I> = Vec::new();
    for ch in [
        vector_results,
        bm25_results,
        graph_results,
        Some(keyword_results),
    ]
    .into_iter()
    .flatten()
    {
        for (id, _) in &ch.results {
            if seen.insert(id) {
                all_ids.push(id);
            }
        }
    }

    let mut fused: Vec<FusedResult<I>> = all_ids
        .iter()
        .map(|id| {
            let (v_rank, v_score) = rank_and_score(&vector_ranks, id, penalty);
            let (b_rank, b_score) = rank_and_score(&bm25_ranks, id, penalty);
            let (g_rank, g_score) = rank_and_score(&graph_ranks, id, penalty);
            let (kw_rank, _) = rank_and_score(&keyword_ranks, id, penalty);

            let mut rrf_score = 0.0;
            if vector_results.is_some() {
                rrf_score += 1.0 / (config.k + v_rank);
            }
            if bm25_results.is_some() {
                rrf_score += config.bm25_weight / (config.k + b_rank);
            }
            if graph_results.is_some() {
                rrf_score += config.graph_weight / (config.k + g_rank);
            }
            rrf_score += config.keyword_weight / (config.k + kw_rank);

            FusedResult {
                doc_id: (*id).clone(),
                rrf_score,
                vector_raw_score: v_score,
                bm25_raw_score: b_score,
                graph_raw_score: g_score,
            }
        })
        .collect();

    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(config.top_k);
    fused
}

/// 构建通道排名映射：doc_id → (rank, raw_score)（索引 0 对应 rank=1）。
fn channel_rank_map<I: Clone + std::hash::Hash + Eq>(
    ch: &ChannelResult<I>,
) -> std::collections::HashMap<&I, (f64, f64)> {
    ch.results
        .iter()
        .enumerate()
        .map(|(idx, (id, score))| (id, ((idx + 1) as f64, *score)))
        .collect()
}

/// 在排名映射中查询文档的 (rank, raw_score)；缺席或通道整体缺席时返回惩罚排名。
fn rank_and_score<I: Clone + std::hash::Hash + Eq>(
    ranks: &Option<std::collections::HashMap<&I, (f64, f64)>>,
    id: &I,
    penalty: f64,
) -> (f64, Option<f64>) {
    match ranks {
        Some(map) => map
            .get(id)
            .map(|(r, s)| (*r, Some(*s)))
            .unwrap_or((penalty, None)),
        None => (penalty, None),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建测试用通道结果: [(id, score), ...]，按 score 降序。
    fn make_channel<I: Clone + std::hash::Hash + Eq>(results: Vec<(I, f64)>) -> ChannelResult<I> {
        ChannelResult { results }
    }

    // --- penalty_rank ---

    #[test]
    fn penalty_rank_formula() {
        assert!((penalty_rank(5) - 11.0).abs() < 0.001);
        assert!((penalty_rank(10) - 21.0).abs() < 0.001);
    }

    // --- rrf_single_channel ---

    #[test]
    fn single_channel_basic() {
        let config = RrfConfig {
            top_k: 3,
            ..Default::default()
        };
        let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);

        let fused = rrf_single_channel(&vec, &config);
        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].doc_id, "a");
        assert_eq!(fused[1].doc_id, "b");
        assert_eq!(fused[2].doc_id, "c");

        // rank=1 → 1/(60+1) ≈ 0.01639
        assert!((fused[0].rrf_score - 0.01639).abs() < 0.0001);
    }

    #[test]
    fn single_channel_empty() {
        let config = RrfConfig::default();
        let vec = make_channel::<&str>(vec![]);
        let fused = rrf_single_channel(&vec, &config);
        assert!(fused.is_empty());
    }

    // --- rrf_two_channels ---

    #[test]
    fn two_channels_basic_fusion() {
        let config = RrfConfig {
            top_k: 3,
            ..Default::default()
        };
        // 向量: a > b > c
        let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7)]);
        // BM25: b > d > a
        let bm25 = make_channel(vec![("b", 0.9), ("d", 0.8), ("a", 0.7)]);

        let fused = rrf_two_channels(&vec, &bm25, &config);
        assert_eq!(fused.len(), 3);

        // b 在两个通道都排高位，应排第一
        assert_eq!(fused[0].doc_id, "b");
    }

    #[test]
    fn two_channels_doc_in_both_ranks_higher() {
        let config = RrfConfig {
            top_k: 2,
            ..Default::default()
        };
        let vec = make_channel(vec![("x", 0.9)]);
        let bm25 = make_channel(vec![("x", 0.8), ("y", 0.7)]);

        let fused = rrf_two_channels(&vec, &bm25, &config);
        assert_eq!(fused.len(), 2);
        // x 在两个通道都出现，应排第一
        assert_eq!(fused[0].doc_id, "x");
    }

    #[test]
    fn two_channels_penalty_applied() {
        let config = RrfConfig {
            top_k: 2,
            ..Default::default()
        };
        let vec = make_channel(vec![("only_vec", 0.9)]);
        let bm25 = make_channel(vec![("only_bm25", 0.8)]);

        let fused = rrf_two_channels(&vec, &bm25, &config);
        assert_eq!(fused.len(), 2);

        // both should have penalty on the other channel
        for result in &fused {
            if result.doc_id == "only_vec" {
                assert!(
                    result.bm25_raw_score.is_none(),
                    "only_vec should not have BM25 score"
                );
            }
            if result.doc_id == "only_bm25" {
                assert!(
                    result.vector_raw_score.is_none(),
                    "only_bm25 should not have vector score"
                );
            }
        }
    }

    #[test]
    fn two_channels_empty_inputs() {
        let config = RrfConfig::default();
        let vec = make_channel::<&str>(vec![]);
        let bm25 = make_channel::<&str>(vec![]);

        let fused = rrf_two_channels(&vec, &bm25, &config);
        assert!(fused.is_empty());
    }

    #[test]
    fn two_channels_one_empty() {
        let config = RrfConfig {
            top_k: 2,
            ..Default::default()
        };
        let vec = make_channel(vec![("a", 0.9), ("b", 0.8)]);
        let bm25 = make_channel::<&str>(vec![]);

        let fused = rrf_two_channels(&vec, &bm25, &config);
        assert_eq!(fused.len(), 2);
        // 仅向量通道有结果，BM25 用惩罚排名
        assert_eq!(fused[0].doc_id, "a");
    }

    // --- rrf_fuse (三通道) ---

    #[test]
    fn three_channels_basic_fusion() {
        let config = RrfConfig {
            top_k: 3,
            ..Default::default()
        };
        // 向量: a > b > c
        let vec = make_channel(vec![("a", 0.95), ("b", 0.85), ("c", 0.75)]);
        // BM25: b > d > a
        let bm25 = make_channel(vec![("b", 0.9), ("d", 0.8), ("a", 0.7)]);
        // 图谱: c > b > e
        let graph = make_channel(vec![("c", 0.9), ("b", 0.8), ("e", 0.7)]);

        let fused = rrf_fuse(&vec, &bm25, &graph, &config);
        assert_eq!(fused.len(), 3);
        // b 在三通道都排在高位，应排第一
        assert_eq!(fused[0].doc_id, "b");
    }

    #[test]
    fn three_channels_doc_in_all_three_wins() {
        let config = RrfConfig {
            top_k: 1,
            ..Default::default()
        };
        let vec = make_channel(vec![("common", 0.9)]);
        let bm25 = make_channel(vec![("common", 0.8)]);
        let graph = make_channel(vec![("common", 0.7)]);

        let fused = rrf_fuse(&vec, &bm25, &graph, &config);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].doc_id, "common");
        assert!(fused[0].vector_raw_score.is_some());
        assert!(fused[0].bm25_raw_score.is_some());
        assert!(fused[0].graph_raw_score.is_some());
    }

    #[test]
    fn three_channels_empty() {
        let config = RrfConfig::default();
        let vec = make_channel::<&str>(vec![]);
        let bm25 = make_channel::<&str>(vec![]);
        let graph = make_channel::<&str>(vec![]);

        let fused = rrf_fuse(&vec, &bm25, &graph, &config);
        assert!(fused.is_empty());
    }

    // --- rank 公式验证 ---

    #[test]
    fn rrf_score_formula_verification() {
        // rank 1 在向量通道，不在其他通道
        // RRF = 1/(60+1) + 1.0/(60+penalty) + 0.8/(60+penalty)
        // penalty = 5*2+1 = 11
        // RRF = 1/61 + 1.0/71 + 0.8/71 ≈ 0.01639 + 0.01408 + 0.01127 ≈ 0.04175
        let config = RrfConfig {
            top_k: 5,
            ..Default::default()
        };
        let vec = make_channel(vec![("x", 0.9)]);
        let bm25 = make_channel::<&str>(vec![]);
        let graph = make_channel::<&str>(vec![]);

        let fused = rrf_fuse(&vec, &bm25, &graph, &config);
        assert_eq!(fused.len(), 1);
        let expected = 1.0 / 61.0 + 1.0 / 71.0 + 0.8 / 71.0;
        assert!(
            (fused[0].rrf_score - expected).abs() < 0.0001,
            "expected {:.6}, got {:.6}",
            expected,
            fused[0].rrf_score
        );
    }

    // --- top_k 截断 ---

    #[test]
    fn top_k_truncation() {
        let config = RrfConfig {
            top_k: 2,
            ..Default::default()
        };
        let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);
        let bm25 = make_channel::<&str>(vec![]);
        let graph = make_channel::<&str>(vec![]);

        let fused = rrf_fuse(&vec, &bm25, &graph, &config);
        assert_eq!(fused.len(), 2, "should be truncated to top_k=2");
    }

    // --- 分数排序验证 ---

    #[test]
    fn results_sorted_descending() {
        let config = RrfConfig {
            top_k: 10,
            ..Default::default()
        };
        let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7)]);
        let bm25 = make_channel(vec![("d", 0.6), ("e", 0.5)]);
        let graph = make_channel(vec![("f", 0.4)]);

        let fused = rrf_fuse(&vec, &bm25, &graph, &config);
        for window in fused.windows(2) {
            assert!(
                window[0].rrf_score >= window[1].rrf_score,
                "results should be sorted descending"
            );
        }
    }

    // --- rrf_fuse_with_keyword (四通道) ---

    /// 关键词通道独有命中可把文档提升进 top（向量前 k 之外的文档进入结果）。
    #[test]
    fn keyword_channel_lifts_doc_into_top() {
        // 向量返回恰好 top_k=4 条；关键词通道独有第 5 篇文档 E（rank 1）
        let config = RrfConfig {
            top_k: 4,
            ..Default::default()
        };
        let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);
        let bm25 = make_channel::<&str>(vec![]);
        let graph = make_channel::<&str>(vec![]);
        let keyword = make_channel(vec![("e", 0.9)]);

        let fused = rrf_fuse_with_keyword(Some(&vec), Some(&bm25), Some(&graph), &keyword, &config);
        let ids: Vec<&str> = fused.iter().map(|f| f.doc_id).collect();
        assert!(
            ids.contains(&"e"),
            "关键词通道独有命中应进入 top，实际 {ids:?}"
        );
        // 关键词通道无 raw 字段暴露（e 非向量/BM25/图谱命中，其通道分数仅计入 rrf）
        let e = fused.iter().find(|f| f.doc_id == "e").unwrap();
        assert!(e.vector_raw_score.is_none());
        assert!(e.bm25_raw_score.is_none());
        assert!(e.graph_raw_score.is_none());
    }

    /// keyword_weight 参与融合：权重拉低后关键词独有命中退出 top（权重改变排序）。
    #[test]
    fn keyword_weight_changes_membership() {
        let vec = make_channel(vec![("a", 0.9), ("b", 0.8), ("c", 0.7), ("d", 0.6)]);
        let keyword = make_channel(vec![("e", 0.9)]);

        let high = RrfConfig {
            top_k: 4,
            keyword_weight: 1.0,
            ..Default::default()
        };
        let fused_high = rrf_fuse_with_keyword(Some(&vec), None, None, &keyword, &high);
        assert!(
            fused_high.iter().any(|f| f.doc_id == "e"),
            "keyword_weight=1.0 时关键词独有命中应进入 top"
        );

        let low = RrfConfig {
            top_k: 4,
            keyword_weight: 0.05,
            ..Default::default()
        };
        let fused_low = rrf_fuse_with_keyword(Some(&vec), None, None, &keyword, &low);
        assert!(
            !fused_low.iter().any(|f| f.doc_id == "e"),
            "keyword_weight 过低时关键词独有命中应退出 top（权重参与融合）"
        );
    }

    /// 仅关键词通道有数据 → 单通道语义仍可返回该文档。
    #[test]
    fn keyword_only_channel_returns_doc() {
        let config = RrfConfig {
            top_k: 5,
            ..Default::default()
        };
        let keyword = make_channel(vec![("kw_only", 0.8), ("kw2", 0.6)]);

        let fused = rrf_fuse_with_keyword(None, None, None, &keyword, &config);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].doc_id, "kw_only");
        assert!(fused[0].rrf_score > 0.0);
    }

    /// 关键词通道权重默认值 1.0（与向量同权重）。
    #[test]
    fn keyword_weight_default_is_one() {
        assert!((RrfConfig::default().keyword_weight - 1.0).abs() < f64::EPSILON);
    }
}
