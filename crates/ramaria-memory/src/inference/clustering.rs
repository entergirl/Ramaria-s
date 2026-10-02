//! crates/ramaria-memory/src/inference/clustering.rs - 态度语义聚类
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
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::cosine_similarity`，本函数为薄包装，
///   保持公开 API（`ramaria_memory::cosine_similarity`）签名不变。
///
/// 返回:
/// - 余弦相似度 -1.0..1.0。若任一向量为零向量则返回 0.0。
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    crate::similarity::cosine_similarity(a, b)
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
// 语义标签生成与跨版本簇匹配
// =========================================================

/// 从簇的核心样本中提取语义标签。
///
/// 算法:
/// 1. 对每条核心 paraphrase 按中文标点（，。！？、；：）和空格切分为短语片段。
/// 2. 统计每个短语在所有核心 paraphrase 中出现的频次。
/// 3. 筛选出现率 ≥ 50% 的短语，按频次降序排列。
/// 4. 取前 3 个短语拼接为语义标签（用 "｜" 分隔）。
/// 5. 若无满足阈值的短语，则取频次最高的前 2 个（最少取 1 个）。
///
/// 参数:
/// - `cluster`: 聚类簇描述，使用其 `core_paraphrases` 作为分析源。
///
/// 返回:
/// - 语义标签字符串。若核心样本为空则返回 `"未命名簇"`。
pub fn generate_semantic_label(cluster: &ClusterDescription) -> String {
    let phrases = &cluster.core_paraphrases;
    if phrases.is_empty() {
        return "未命名簇".to_string();
    }

    // Step 1: 对每条 paraphrase 切分为短语片段
    let all_phrase_sets: Vec<Vec<String>> = phrases
        .iter()
        .map(|text| split_chinese_phrases(text))
        .collect();

    // Step 2: 统计每个短语在多少条 paraphrase 中出现
    let total = phrases.len() as f64;
    let mut phrase_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for phrase_set in &all_phrase_sets {
        // 每条 paraphrase 内去重（同一短语在同一条中出现多次只计 1 次）
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for p in phrase_set {
            if p.len() >= 2 && seen.insert(p) {
                *phrase_counts.entry(p.clone()).or_default() += 1;
            }
        }
    }

    // Step 3: 筛选出现率 ≥ 50% 的短语，按频次降序
    let threshold = (total * 0.5).ceil() as usize;
    let mut candidates: Vec<(&String, &usize)> = phrase_counts
        .iter()
        .filter(|&(_, &count)| count >= threshold)
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.len().cmp(&b.0.len())));

    let top_phrases: Vec<String> = if candidates.len() >= 2 {
        candidates
            .iter()
            .take(3)
            .map(|(phrase, _)| (*phrase).clone())
            .collect()
    } else if !candidates.is_empty() {
        // 只有 1 个满足阈值的短语，加上频次最高的其他短语补足
        let mut result: Vec<String> = candidates
            .iter()
            .map(|(phrase, _)| (*phrase).clone())
            .collect();
        let mut remaining: Vec<(&String, &usize)> = phrase_counts
            .iter()
            .filter(|(p, _)| !result.contains(p))
            .collect();
        remaining.sort_by(|a, b| b.1.cmp(a.1));
        for (phrase, _) in remaining.iter().take(2) {
            result.push((*phrase).clone());
        }
        result
    } else {
        // 无短语达到 50% 阈值，取频次最高的 2 个
        let mut sorted: Vec<(&String, &usize)> = phrase_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        sorted
            .iter()
            .take(2) // 至少取 1 个，.take(2) 保证至少 1 个（空列表兜底在前）
            .map(|(phrase, _)| (*phrase).clone())
            .collect()
    };

    if top_phrases.is_empty() {
        "未命名簇".to_string()
    } else {
        top_phrases.join(" ｜ ")
    }
}

/// 按中文标点和空格切分文本为短语片段。
///
/// 保留长度 ≥ 2 字符的片段，过滤纯数字/标点片段。
fn split_chinese_phrases(text: &str) -> Vec<String> {
    let delimiters: &[char] = &[
        '，', '。', '！', '？', '、', '；', '：', ' ', '\t', '\n', '\r',
    ];
    text.split(delimiters)
        .map(|s| s.trim())
        .filter(|s| s.len() >= 2 && s.chars().any(|c| c.is_alphabetic() || is_cjk(c)))
        .map(|s| s.to_string())
        .collect()
}

/// 判断字符是否为 CJK（中日韩）字符。
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'   // CJK 统一表意文字
        | '\u{3400}'..='\u{4DBF}'  // CJK 扩展 A
        | '\u{F900}'..='\u{FAFF}'  // CJK 兼容表意文字
        | '\u{3040}'..='\u{309F}'  // 平假名
        | '\u{30A0}'..='\u{30FF}'  // 片假名
        | '\u{AC00}'..='\u{D7AF}'  // 韩文音节
    )
}

// =========================================================
// 跨版本簇匹配
// =========================================================

/// 跨版本簇匹配结果——单个历史快照的匹配信息。
#[derive(Debug, Clone)]
pub struct CrossVersionMatch {
    /// 历史快照的 id
    pub snapshot_id: i64,
    /// 历史快照的语义标签文本
    pub semantic_label: String,
    /// 历史快照的分类
    pub category: String,
    /// 余弦相似度 (0..1)
    pub similarity: f64,
    /// 是否匹配（similarity ≥ match_threshold）
    pub is_match: bool,
    /// 历史快照的创建时间（Unix 毫秒）
    pub snapshot_created_at: i64,
}

/// 跨版本簇匹配的聚合结果。
#[derive(Debug, Clone, Default)]
pub struct CrossVersionMatchResult {
    /// 所有匹配项（相似度降序）
    pub matches: Vec<CrossVersionMatch>,
    /// 最佳匹配项（相似度最高者）
    pub best_match: Option<CrossVersionMatch>,
    /// 被查询的历史快照总数
    pub total_historical: usize,
    /// 匹配到的快照数
    pub matched_count: usize,
}

/// 跨版本簇匹配的语义余弦相似度阈值（统一 0.85）。
///
/// 语义标签 embedding 与历史快照的余弦相似度 ≥ 该值才判定为同一性格倾向的延续，
/// 低于 0.85 视为不同倾向，避免将语义相近但不同的簇误判为跨版本延续。
pub const CROSS_VERSION_MATCH_THRESHOLD: f64 = 0.85;

/// 执行跨版本簇匹配。
///
/// 算法:
/// 1. 对当前簇的语义标签 embedding 与每个历史快照的 embedding 计算余弦相似度。
/// 2. 相似度 ≥ `match_threshold`（默认 0.85）视为匹配。
/// 3. 返回按相似度降序排列的匹配列表。
///
/// 参数:
/// - `current_embedding`: 当前簇语义标签的 embedding 向量。
/// - `historical_snapshots`: 该 persona 的历史快照列表（需含 `semantic_label_embedding`）。
/// - `match_threshold`: 匹配阈值，默认 0.85。
///
/// 返回:
/// - CrossVersionMatchResult，含匹配列表和统计信息。
pub fn match_clusters_cross_version(
    current_embedding: &[f32],
    historical_snapshots: &[HistoricalSnapshot],
    match_threshold: f64,
) -> CrossVersionMatchResult {
    let total = historical_snapshots.len();
    if current_embedding.is_empty() || total == 0 {
        return CrossVersionMatchResult {
            matches: Vec::new(),
            best_match: None,
            total_historical: total,
            matched_count: 0,
        };
    }

    let mut matches: Vec<CrossVersionMatch> = historical_snapshots
        .iter()
        .filter_map(|snap| {
            let hist_emb = snap.semantic_label_embedding.as_ref()?;
            let hist_vec = ramaria_core::types::ClusterSnapshot::deserialize_embedding(hist_emb)?;
            if hist_vec.len() != current_embedding.len() {
                return None; // 维度不匹配，跳过
            }
            let sim = cosine_similarity(current_embedding, &hist_vec);
            Some(CrossVersionMatch {
                snapshot_id: snap.id,
                semantic_label: snap.semantic_label.clone().unwrap_or_default(),
                category: snap.category.clone(),
                similarity: sim,
                is_match: sim >= match_threshold,
                snapshot_created_at: snap.created_at,
            })
        })
        .collect();

    // 按相似度降序排列
    matches.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let matched_count = matches.iter().filter(|m| m.is_match).count();
    let best_match = matches.first().cloned();

    CrossVersionMatchResult {
        matches,
        best_match,
        total_historical: total,
        matched_count,
    }
}

/// 历史快照的轻量表示（用于跨版本匹配输入）。
///
/// 职责:
/// - 从 `ClusterSnapshot` 中提取跨版本匹配所需的最小字段集。
/// - 避免在纯计算函数中依赖完整的 `ClusterSnapshot` 类型。
#[derive(Debug, Clone)]
pub struct HistoricalSnapshot {
    /// 快照 id
    pub id: i64,
    /// 语义标签文本
    pub semantic_label: Option<String>,
    /// 语义标签 embedding BLOB
    pub semantic_label_embedding: Option<Vec<u8>>,
    /// 分类
    pub category: String,
    /// 创建时间
    pub created_at: i64,
}

impl From<&ramaria_core::types::ClusterSnapshot> for HistoricalSnapshot {
    fn from(s: &ramaria_core::types::ClusterSnapshot) -> Self {
        Self {
            id: s.id,
            semantic_label: s.semantic_label.clone(),
            semantic_label_embedding: s.semantic_label_embedding.clone(),
            category: s.category.clone(),
            created_at: s.created_at,
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
