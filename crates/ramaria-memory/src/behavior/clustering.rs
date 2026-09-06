//! crates/ramaria-memory/src/behavior/clustering.rs - 行为样本聚类（D2，v3.1 §4.2 Step 2）
//!
//! 设计特点:
//! - 双通道向量化：反应通道 r = embedding(paraphrase⊕attitude)、情境通道 s = embedding(关键词拼接)
//! - 三路融合相似度：sim = β1·cos(r_i,r_j) + β2·cos(s_i,s_j) + (1−β1−β2)·Jaccard(K_i,K_j)
//!   —— 通道缺向量时对应权重归零并归一化（embedding 不可用 → 纯关键词 Jaccard，β=0 降级）
//! - 密度聚类：邻域 sim ≥ θ_nb、核心样本邻居数 ≥ min_cluster_size、密度可达连接、
//!   边界软分配、孤立点不入簇；孤立点比例 > 60% 触发失败模式检查（下调 θ_nb 重试）
//! - 簇提炼：关键词并集（频次 Top-N）/ 簇中心向量 / valence 加权均值与标准差 /
//!   presentation 分布 / situation_strength 均值 / 时间跨度 / 簇质量（内聚度 × 一致性）
//! - 纯计算函数零 I/O；向量化通过 `EmbeddingProvider` trait 注入，便于 mock 确定性测试

use ramaria_core::behavior::{BehaviorSituation, PresentationFreq};
use ramaria_core::config::BehaviorConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::{MemoryEvent, Presentation};
use std::collections::HashMap;

// =========================================================
// 聚类输入样本
// =========================================================

/// 单条事件的聚类样本（情境-反应对，v3.1 §4.2 Step 1）。
///
/// 字段约定:
/// - `situation_keywords`: 情境侧关键词集（不含 valence，避免情绪信号污染情境判定）。
/// - `situation_vector`: 情境通道向量（embedding 不可用时为 None → 降级关键词通道）。
/// - `reaction_vector`: 反应通道向量（embedding 不可用时为 None）。
/// - `valence` / `presentation` / `salience`: 反应侧特征（簇提炼与参数化输入）。
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorSample {
    /// 事件 id（证据引用）
    pub event_id: i64,
    /// 情境侧关键词集（去重小写）
    pub situation_keywords: Vec<String>,
    /// 情境通道向量 s_i
    pub situation_vector: Option<Vec<f32>>,
    /// 反应通道向量 r_i
    pub reaction_vector: Option<Vec<f32>>,
    /// 情绪效价 -1.0..1.0
    pub valence: f64,
    /// 陈述方式
    pub presentation: Presentation,
    /// 显著性权重（salience 加权证据量）
    pub salience: f64,
    /// 情境强度 1-5（None 等效 3）
    pub situation_strength: Option<i32>,
    /// 事件开始时间（Unix 毫秒）
    pub start_ms: i64,
}

/// 从 `MemoryEvent` 构造样本（向量留空，由 `vectorize` 填充）。
///
/// 参数:
/// - `event`: L2 事件。
///
/// 说明:
/// - 关键词取 `keywords` 逗号分隔拆分（保留原词，去重去空）。
/// - 反应通道文本 = paraphrase ⊕ attitude（优先 paraphrase，缺失回退 attitude；
///   两者皆缺则该事件不参与反应通道向量化，但仍可参与情境通道与关键词聚类）。
pub fn sample_from_event(event: &MemoryEvent) -> BehaviorSample {
    let keywords: Vec<String> = event
        .keywords
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect::<Vec<_>>();

    BehaviorSample {
        event_id: event.id,
        situation_keywords: dedup_keywords(&keywords),
        situation_vector: None,
        reaction_vector: None,
        valence: event.valence.clamp(-1.0, 1.0),
        presentation: event.presentation,
        salience: event.salience.clamp(0.0, 1.0),
        situation_strength: event.situation_strength,
        start_ms: event.start,
    }
}

/// 关键词去重（保序、小写化）。
pub fn dedup_keywords(raw: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.iter()
        .map(|k| k.trim().to_lowercase())
        .filter(|k| !k.is_empty())
        .filter(|k| seen.insert(k.clone()))
        .collect()
}

// =========================================================
// 双通道向量化
// =========================================================

/// 双通道向量化：为样本填充情境通道与反应通道向量。
///
/// 参数:
/// - `samples`: 待向量化样本（原地修改）。
/// - `embedder`: 嵌入模型 provider；`None` 表示 embedding 不可用（纯关键词降级）。
///
/// 说明:
/// - 反应通道文本 = paraphrase ⊕ attitude；情境通道文本 = 关键词空格拼接。
/// - 单条 embedding 失败只影响该样本对应通道（记 warn 后置 None），不整体失败——
///   保证 embedding 局部故障时聚类仍可降级运行（静默降级链）。
pub async fn vectorize(
    samples: &mut [BehaviorSample],
    events: &[MemoryEvent],
    embedder: Option<&dyn EmbeddingProvider>,
) -> RamariaResult<()> {
    let Some(embedder) = embedder else {
        // embedding 不可用：全部通道留空，聚类退化为纯关键词 Jaccard（β=0）
        tracing::warn!("行为聚类 embedding 不可用，降级纯关键词 Jaccard 通道");
        return Ok(());
    };

    // 事件 id → 反应通道文本（paraphrase ⊕ attitude）
    let reaction_texts: HashMap<i64, Option<String>> = events
        .iter()
        .map(|e| {
            let text = match (&e.paraphrase, &e.attitude) {
                (Some(p), Some(a)) => Some(format!("{}\n{}", p, a)),
                (Some(p), None) => Some(p.clone()),
                (None, Some(a)) => Some(a.clone()),
                (None, None) => None,
            };
            (e.id, text)
        })
        .collect();

    // 收集需要向量化的文本（按样本顺序，保留索引）
    let mut reaction_payloads: Vec<Option<String>> = Vec::with_capacity(samples.len());
    let mut situation_payloads: Vec<Option<String>> = Vec::with_capacity(samples.len());
    for s in samples.iter() {
        let r = reaction_texts.get(&s.event_id).cloned().flatten();
        let s_text = if s.situation_keywords.is_empty() {
            None
        } else {
            Some(s.situation_keywords.join(" "))
        };
        reaction_payloads.push(r);
        situation_payloads.push(s_text);
    }

    // 分两批向量化（反应通道 + 情境通道），各自独立降级
    vectorize_channel(samples, events, embedder, &reaction_payloads, |s, v| {
        s.reaction_vector = Some(v);
    })
    .await?;
    vectorize_channel(samples, events, embedder, &situation_payloads, |s, v| {
        s.situation_vector = Some(v);
    })
    .await?;

    Ok(())
}

/// 对单通道批量向量化；缺失文本或单条失败 → 对应向量为 None（不阻塞整体）。
async fn vectorize_channel(
    samples: &mut [BehaviorSample],
    events: &[MemoryEvent],
    embedder: &dyn EmbeddingProvider,
    payloads: &[Option<String>],
    assign: impl Fn(&mut BehaviorSample, Vec<f32>),
) -> RamariaResult<()> {
    let _ = events;
    // 先批量请求可向量化文本
    let mut batch_texts: Vec<&str> = Vec::new();
    let mut batch_index: Vec<usize> = Vec::new(); // 样本索引
    for (i, p) in payloads.iter().enumerate() {
        if let Some(text) = p {
            batch_texts.push(text.as_str());
            batch_index.push(i);
        }
    }
    if batch_texts.is_empty() {
        return Ok(());
    }
    let vectors = match embedder.embed_batch(&batch_texts).await {
        Ok(v) => v,
        Err(e) => {
            // 批量失败：回退逐条尝试，单条失败记 warn 置 None（静默降级）
            tracing::warn!(error = %e, "行为聚类批量向量化失败，回退逐条");
            let mut out = Vec::with_capacity(batch_texts.len());
            for t in batch_texts {
                match embedder.embed(t).await {
                    Ok(v) => out.push(v),
                    Err(e2) => {
                        tracing::warn!(error = %e2, "行为聚类单条向量化失败，置 None");
                        out.push(Vec::new()); // 哨兵空向量
                    }
                }
            }
            out
        }
    };
    for (k, &sample_idx) in batch_index.iter().enumerate() {
        let v = &vectors[k];
        if !v.is_empty() {
            assign(&mut samples[sample_idx], v.clone());
        }
    }
    Ok(())
}

// =========================================================
// 三路融合相似度
// =========================================================

/// 计算两样本的三路融合相似度。
///
/// 公式: sim = β1·cos(r_i,r_j) + β2·cos(s_i,s_j) + (1−β1−β2)·Jaccard(K_i,K_j)
///
/// 降级说明:
/// - 双方都有反应向量才计入 β1 项，否则该项权重归零并重新归一化其余项；
///   情境通道同理。embedding 全部不可用 → sim = Jaccard（β 通道权重为 0）。
/// - cos 计算前 clip 到 [-1,1]（浮点误差防御），零向量余弦按 0 处理。
///
/// 参数:
/// - `beta1` + `beta2`: 双通道权重，约束 β1 + β2 ≤ 1（关键词权重 = 1 − β1 − β2）。
///
/// 返回:
/// - 相似度 0.0..1.0（Jaccard 与 clip 后余弦均非负，加权和保持非负）。
pub fn fused_similarity(a: &BehaviorSample, b: &BehaviorSample, beta1: f64, beta2: f64) -> f64 {
    let beta3 = (1.0 - beta1 - beta2).max(0.0);
    let r_ok = a.reaction_vector.is_some() && b.reaction_vector.is_some();
    let s_ok = a.situation_vector.is_some() && b.situation_vector.is_some();

    let mut weight_sum = beta3;
    let mut acc = beta3 * jaccard(&a.situation_keywords, &b.situation_keywords);

    if r_ok {
        let cos_r = cosine_clipped(
            a.reaction_vector.as_deref().unwrap_or_default(),
            b.reaction_vector.as_deref().unwrap_or_default(),
        );
        weight_sum += beta1;
        acc += beta1 * cos_r;
    }
    if s_ok {
        let cos_s = cosine_clipped(
            a.situation_vector.as_deref().unwrap_or_default(),
            b.situation_vector.as_deref().unwrap_or_default(),
        );
        weight_sum += beta2;
        acc += beta2 * cos_s;
    }

    if weight_sum <= 0.0 {
        return 0.0;
    }
    (acc / weight_sum).clamp(0.0, 1.0)
}

/// 两集合的 Jaccard 相似度（空集 → 0.0）。
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::jaccard_similarity`，本函数为薄包装。
pub fn jaccard(a: &[String], b: &[String]) -> f64 {
    crate::similarity::jaccard_similarity(
        a.iter().map(String::as_str),
        b.iter().map(String::as_str),
    )
}

/// 余弦相似度（零向量 → 0.0；结果 clip 到 [-1,1] 防御浮点误差）。
///
/// 说明（v1.5 收敛）:
/// - 实现统一收敛到 `crate::similarity::cosine_similarity`，本函数为薄包装。
/// - 统一实现同样 clamp 到 [-1,1]；调用点如需 [0,1] 语义请自行 `.max(0.0)`
///   （`routing::score_rule` 与 `incremental` 模块已如此处理）。
pub fn cosine_clipped(a: &[f32], b: &[f32]) -> f64 {
    crate::similarity::cosine_similarity(a, b)
}

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

// =========================================================
// 簇提炼
// =========================================================

/// 簇成员逐事件信息（供近期事件加权证据链）。
///
/// 职责:
/// - 保留每个成员的 `start_ms` 与 `salience`，使 `build_evidence` 能用真实
///   事件时间计算 recency_factor（修复"恒 1.0"缺陷，D-V16-007）。
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterMember {
    /// 事件 id（锚点可能为负，调用方过滤后写入证据链）
    pub event_id: i64,
    /// 事件开始时间（Unix 毫秒）
    pub start_ms: i64,
    /// 显著性权重（salience 加权）
    pub salience: f64,
}

/// 提炼后的簇（可直接构造 `BehaviorSituation` 持久化）。
#[derive(Debug, Clone, PartialEq)]
pub struct RefinedCluster {
    /// 情境侧特征（含关键词并集/簇中心/valence 分布/presentation 分布等）
    pub situation: BehaviorSituation,
    /// 有效样本量 n_eff（salience 加权）
    pub n_eff: f64,
    /// 内聚度（簇内成员间平均相似度）
    pub cohesion: f64,
    /// 簇质量 = 内聚度 × 一致性（1 − 归一化 valence 标准差）
    pub quality: f64,
    /// 簇内事件 id（证据链引用）
    pub member_event_ids: Vec<i64>,
    /// 簇成员逐事件信息（保留 start/salience，供近期事件加权）。
    ///
    /// 与 `member_event_ids` 一一对应（同索引），顺序一致。
    pub member_events: Vec<ClusterMember>,
}

/// 关键词并集保留的 Top-N 条数。
pub const KEYWORD_TOP_N: usize = 10;

/// 簇提炼（v3.1 §4.2 Step 2.4）。
///
/// 输出:
/// - 关键词并集（频次 Top-N）
/// - 簇中心（情境通道 + 反应通道向量均值）
/// - valence 加权均值与标准差（salience 加权）
/// - presentation 分布
/// - situation_strength 均值（None 按 3）
/// - 时间跨度（天）
/// - n_eff / 内聚度 / 簇质量
pub fn refine_cluster(
    samples: &[BehaviorSample],
    members: &[usize],
    beta1: f64,
    beta2: f64,
) -> RefinedCluster {
    let member_samples: Vec<&BehaviorSample> = members.iter().map(|&i| &samples[i]).collect();

    // ---- 关键词并集（频次 Top-N） ----
    let mut kw_freq: HashMap<&str, usize> = HashMap::new();
    for s in &member_samples {
        for k in &s.situation_keywords {
            *kw_freq.entry(k.as_str()).or_insert(0) += 1;
        }
    }
    let mut kw_sorted: Vec<(&str, usize)> = kw_freq.into_iter().collect();
    kw_sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let keywords: Vec<String> = kw_sorted
        .into_iter()
        .take(KEYWORD_TOP_N)
        .map(|(k, _)| k.to_string())
        .collect();

    // ---- 簇中心（通道向量均值） ----
    let centroid = mean_vector(
        member_samples
            .iter()
            .filter_map(|s| s.situation_vector.as_deref()),
    );
    let response_centroid = mean_vector(
        member_samples
            .iter()
            .filter_map(|s| s.reaction_vector.as_deref()),
    );

    // ---- valence 加权均值/标准差（salience 加权） ----
    let weight_sum: f64 = member_samples.iter().map(|s| s.salience).sum();
    let valence_mean = if weight_sum > 0.0 {
        member_samples
            .iter()
            .map(|s| s.valence * s.salience)
            .sum::<f64>()
            / weight_sum
    } else {
        member_samples.iter().map(|s| s.valence).sum::<f64>() / member_samples.len().max(1) as f64
    };
    let variance = if weight_sum > 0.0 {
        member_samples
            .iter()
            .map(|s| s.salience * (s.valence - valence_mean).powi(2))
            .sum::<f64>()
            / weight_sum
    } else {
        member_samples
            .iter()
            .map(|s| (s.valence - valence_mean).powi(2))
            .sum::<f64>()
            / member_samples.len().max(1) as f64
    };
    let valence_std = variance.sqrt();

    // ---- presentation 分布 ----
    let mut pres_count: HashMap<Presentation, usize> = HashMap::new();
    for s in &member_samples {
        *pres_count.entry(s.presentation).or_insert(0) += 1;
    }
    let total = member_samples.len().max(1);
    let mut presentation_dist: Vec<PresentationFreq> = pres_count
        .into_iter()
        .map(|(p, c)| PresentationFreq {
            presentation: p,
            freq: c as f64 / total as f64,
        })
        .collect();
    presentation_dist.sort_by(|a, b| {
        b.freq
            .partial_cmp(&a.freq)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ---- situation_strength 均值（None 等效 3） ----
    let strength_mean = member_samples
        .iter()
        .map(|s| s.situation_strength.unwrap_or(3) as f64)
        .sum::<f64>()
        / total as f64;

    // ---- 时间跨度（天） ----
    let min_start = member_samples.iter().map(|s| s.start_ms).min().unwrap_or(0);
    let max_start = member_samples.iter().map(|s| s.start_ms).max().unwrap_or(0);
    let time_span_days = (max_start - min_start) as f64 / 86_400_000.0;

    // ---- 内聚度 / 簇质量 ----
    let n = member_samples.len();
    let cohesion = if n <= 1 {
        1.0
    } else {
        let mut sum = 0.0;
        let mut cnt = 0usize;
        for a in 0..n {
            for b in (a + 1)..n {
                sum += fused_similarity(member_samples[a], member_samples[b], beta1, beta2);
                cnt += 1;
            }
        }
        sum / cnt as f64
    };
    // 一致性 = 1 − 归一化 valence 标准差（valence 范围 [-1,1]，std 上限 2）
    let consistency = (1.0 - (valence_std / 2.0)).clamp(0.0, 1.0);
    let quality = cohesion * consistency;

    // ---- n_eff ----
    let n_eff = if weight_sum > 0.0 {
        weight_sum
    } else {
        member_samples.len() as f64
    };

    RefinedCluster {
        situation: BehaviorSituation {
            keywords,
            centroid,
            response_centroid,
            valence_mean: valence_mean.clamp(-1.0, 1.0),
            valence_std,
            sample_count: member_samples.len(),
            presentation_dist,
            situation_strength_mean: strength_mean,
            time_span_days,
            trait_refs: Vec::new(),
        },
        n_eff,
        cohesion,
        quality,
        member_event_ids: member_samples.iter().map(|s| s.event_id).collect(),
        member_events: member_samples
            .iter()
            .map(|s| ClusterMember {
                event_id: s.event_id,
                start_ms: s.start_ms,
                salience: s.salience,
            })
            .collect(),
    }
}

/// 计算多向量的均值（无向量输入 → None）。
fn mean_vector<'a>(vecs: impl Iterator<Item = &'a [f32]>) -> Option<Vec<f32>> {
    let mut it = vecs;
    let first = it.next()?;
    let dim = first.len();
    if dim == 0 {
        return None;
    }
    let mut sum = vec![0.0f64; dim];
    let mut count = 0usize;
    for v in std::iter::once(first).chain(it) {
        if v.len() == dim {
            for (s, &x) in sum.iter_mut().zip(v.iter()) {
                *s += x as f64;
            }
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some(sum.iter().map(|&s| (s / count as f64) as f32).collect())
}

// =========================================================
// 学习管线编排（D2 入口）
// =========================================================

/// 行为聚类编排器。
///
/// 职责:
/// - 事件 → 样本 → 向量化 → 密度聚类（含失败模式检查重试）→ 簇提炼。
pub struct BehaviorClusterer<'a> {
    config: &'a BehaviorConfig,
    embedder: Option<&'a dyn EmbeddingProvider>,
}

impl<'a> BehaviorClusterer<'a> {
    /// 创建聚类编排器。
    ///
    /// 参数:
    /// - `config`: 行为层配置（θ_nb/min_cluster_size/β 权重/孤立点比例上限）。
    /// - `embedder`: 嵌入模型 provider；`None` 表示 embedding 不可用（纯关键词降级）。
    pub fn new(config: &'a BehaviorConfig, embedder: Option<&'a dyn EmbeddingProvider>) -> Self {
        Self { config, embedder }
    }

    /// 执行完整聚类管线。
    ///
    /// 流程:
    /// 1. `sample_from_event` 构造样本。
    /// 2. `vectorize` 双通道向量化（embedding 不可用 → 纯关键词）。
    /// 3. `density_cluster` 密度聚类；孤立点比例 > `max_outlier_ratio` 时
    ///    按失败模式检查下调 θ_nb（每次 −0.1，最多 2 次）重试。
    /// 4. `refine_cluster` 逐簇提炼。
    ///
    /// 返回:
    /// - 提炼后的簇列表（仅含有效簇，孤立点不产生簇）。
    /// - 输入为空 → 空列表。
    pub async fn cluster_events(
        &self,
        events: &[MemoryEvent],
    ) -> RamariaResult<Vec<RefinedCluster>> {
        let mut samples: Vec<BehaviorSample> = events.iter().map(sample_from_event).collect();
        self.cluster_samples(events, &mut samples).await
    }

    /// 对"已构造样本"执行聚类管线（支持 Manual 强锚点注入，v3.1 §9.3）。
    ///
    /// 与 `cluster_events` 的区别:
    /// - `samples` 由调用方构造，可混入非事件样本（如 Manual 规则锚点，
    ///   event_id 用负值标记——聚类与簇提炼照常参与，锚点可偏移簇中心）。
    /// - `vectorize` 只填充 `events` 中真实事件对应的样本（锚点样本保留
    ///   调用方预填的向量）。
    /// - 返回的簇中 `member_event_ids` 可能含负 id（锚点）；调用方在生成
    ///   证据链时应过滤（锚点不是真实事件，不写入规则 evidence）。
    ///
    /// 参数:
    /// - `events`: 真实事件（供向量化文本来源）。
    /// - `samples`: 聚类输入样本（长度 ≥ events.len()，前段为事件样本）。
    pub async fn cluster_samples(
        &self,
        events: &[MemoryEvent],
        samples: &mut [BehaviorSample],
    ) -> RamariaResult<Vec<RefinedCluster>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        vectorize(samples, events, self.embedder).await?;

        // 失败模式检查：孤立点比例超限 → 下调 θ_nb 重试（至多 2 次）
        let mut theta_nb = self.config.theta_nb;
        let mut result = density_cluster(
            samples,
            theta_nb,
            self.config.min_cluster_size,
            self.config.beta1,
            self.config.beta2,
        );
        let mut retries = 0;
        while result.outlier_ratio > self.config.max_outlier_ratio && retries < 2 {
            theta_nb = (theta_nb - 0.1).max(0.05);
            tracing::warn!(
                outlier_ratio = %format!("{:.2}", result.outlier_ratio),
                theta_nb,
                "行为聚类孤立点比例超限，下调 θ_nb 重试"
            );
            result = density_cluster(
                samples,
                theta_nb,
                self.config.min_cluster_size,
                self.config.beta1,
                self.config.beta2,
            );
            retries += 1;
        }
        if result.outlier_ratio > self.config.max_outlier_ratio {
            tracing::warn!(
                outlier_ratio = %format!("{:.2}", result.outlier_ratio),
                "行为聚类孤立点比例仍超限，接受当前结果（孤立点不产生规则）"
            );
        }

        // 簇提炼（按簇 id 升序，保证输出顺序稳定）
        let mut refined = Vec::with_capacity(result.cluster_count);
        for cid in 0..result.cluster_count {
            let members = &result.clusters[cid].member_indices;
            let rc = refine_cluster(samples, members, self.config.beta1, self.config.beta2);
            refined.push(rc);
        }
        Ok(refined)
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
