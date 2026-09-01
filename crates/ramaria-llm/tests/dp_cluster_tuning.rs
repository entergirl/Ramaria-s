//! D-P 聚类参数摸底（T-V17-1-006）——真实数据库 + 真实 embedding。
//!
//! 目标：为行为层密度聚类取样本对相似度分布分位数，为 θ_nb 定初值（P50~P75）提供数据支撑，
//! 并对候选 θ_nb / min_cluster_size 做档位对比。
//!
//! 运行方式：
//!   cargo test -p ramaria-llm --features ramaria-llm/cuda --test dp_cluster_tuning -- --ignored --nocapture
//!
//! 说明：
//! - 本测试为 `#[ignore]` 探索性摸底，需真实鸢九库 + 真实 Qwen3 embedding 模型，CI 不运行。
//! - 只读主库（`data/ramaria_assistant.db`），不写入、不修改任何数据。
//! - 依赖 `F:/9700/model/Qwen3-Embedding-0.6B`（与 CLI 运行时 embedding 模型一致）。
//! - 整个文件仅在前置 `embedding-native` feature 下编译（`--features .../cuda` 隐式启用），
//!   避免无 feature 的默认测试构建引用未编译的 native 模块。
//!
//! 输出：
//! - 参与聚类的事件数及 paraphrase/attitude 可用性。
//! - 全部样本对 fused_similarity 分布（min/P25/P50/P75/P90/max/mean）。
//! - θ_nb / min_cluster_size 档位对比（簇数 / 孤立点数 / 比例）。
//! - 定初值建议。

#![cfg(feature = "embedding-native")]

use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_memory::behavior::{
    BehaviorSample, density_cluster, fused_similarity, sample_from_event, vectorize,
};
use ramaria_storage::SqliteStorage;

/// 鸢九库绝对路径（摸底只读主库）。
const DB_PATH: &str = r"F:\Ramaria-s\main\data\ramaria_assistant.db";
/// 鸢九 persona。
const PERSONA_UID: &str = "char-535648097";
/// 真实 Qwen3 embedding 模型目录。
const MODEL_DIR: &str = r"F:/9700/model/Qwen3-Embedding-0.6B";

/// 打开主库（只读，不跑 migration、不写入）。
async fn open_main_db() -> SqliteStorage {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(DB_PATH)
        .read_only(true)
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("打开主库失败");
    SqliteStorage::new(pool)
}

/// 构造真实 embedding provider。
fn build_embedder() -> impl EmbeddingProvider {
    ramaria_llm::embedding::native::NativeEmbeddingProvider::new(MODEL_DIR)
        .expect("加载真实 embedding 模型失败")
}

/// 线性插值分位数（sorted 升序）。
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = p * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo as f64)
    }
}

/// 跑密度聚类并统计（簇数 / 孤立点数 / 孤立点比例）。
fn cluster_stats(
    samples: &[BehaviorSample],
    theta_nb: f64,
    min_size: usize,
    beta1: f64,
    beta2: f64,
) -> (usize, usize, f64) {
    let result = density_cluster(samples, theta_nb, min_size, beta1, beta2);
    let noise = result
        .assignments
        .iter()
        .filter(|a| a.cluster_id.is_none())
        .count();
    let ratio = if samples.is_empty() {
        0.0
    } else {
        noise as f64 / samples.len() as f64
    };
    (result.cluster_count, noise, ratio)
}

/// 聚类质量统计（含最大簇占比，评估簇分离性）。
/// 返回: (簇数, 孤立点数, 孤立点比例, 最大簇成员数, 最大簇占比)。
fn cluster_stats_rich(
    samples: &[BehaviorSample],
    theta_nb: f64,
    min_size: usize,
    beta1: f64,
    beta2: f64,
) -> (usize, usize, f64, usize, f64) {
    let result = density_cluster(samples, theta_nb, min_size, beta1, beta2);
    let noise = result
        .assignments
        .iter()
        .filter(|a| a.cluster_id.is_none())
        .count();
    let ratio = if samples.is_empty() {
        0.0
    } else {
        noise as f64 / samples.len() as f64
    };
    let max_size = result
        .clusters
        .iter()
        .map(|c| c.member_indices.len())
        .max()
        .unwrap_or(0);
    let max_ratio = if samples.is_empty() {
        0.0
    } else {
        max_size as f64 / samples.len() as f64
    };
    (result.cluster_count, noise, ratio, max_size, max_ratio)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn dp_cluster_tuning_reconnaissance() {
    let storage = open_main_db().await;
    let events = storage
        .list_events_by_persona(PERSONA_UID, 0, 500)
        .await
        .expect("读取事件失败");

    println!("\n==== D-P 聚类参数摸底（T-V17-1-006，真实数据）====");
    println!("persona={PERSONA_UID}, 事件数={}", events.len());
    assert!(!events.is_empty(), "应有真实事件参与摸底");

    let with_para = events.iter().filter(|e| e.paraphrase.is_some()).count();
    let with_att = events.iter().filter(|e| e.attitude.is_some()).count();
    println!(
        "paraphrase 可用: {with_para}/{}, attitude 可用: {with_att}/{}",
        events.len(),
        events.len()
    );

    // 构造样本 + 真实向量化（反应通道 paraphrase⊕attitude、情境通道 keywords）。
    let embedder_obj = build_embedder();
    let mut samples: Vec<BehaviorSample> = events.iter().map(sample_from_event).collect();
    vectorize(&mut samples, &events, Some(&embedder_obj))
        .await
        .expect("向量化失败");

    let with_reaction = samples
        .iter()
        .filter(|s| s.reaction_vector.is_some())
        .count();
    let with_situation = samples
        .iter()
        .filter(|s| s.situation_vector.is_some())
        .count();
    println!(
        "反应通道向量: {with_reaction}/{}, 情境通道向量: {with_situation}/{}",
        samples.len(),
        samples.len()
    );

    // 全部样本对 fused_similarity 分布（当前默认 β1=0.4 / β2=0.3）。
    let beta1 = 0.4_f64;
    let beta2 = 0.3_f64;
    let mut sims: Vec<f64> = Vec::new();
    for i in 0..samples.len() {
        for j in (i + 1)..samples.len() {
            sims.push(fused_similarity(&samples[i], &samples[j], beta1, beta2));
        }
    }
    sims.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sims.len();
    let mean = if n > 0 {
        sims.iter().sum::<f64>() / n as f64
    } else {
        f64::NAN
    };
    println!("\n样本对 fused_similarity 分布 (β1={beta1}, β2={beta2}, 共 {n} 对):");
    println!(
        "  min={:.4} P25={:.4} P50={:.4} P75={:.4} P90={:.4} max={:.4} mean={:.4}",
        sims.first().copied().unwrap_or(f64::NAN),
        percentile(&sims, 0.25),
        percentile(&sims, 0.50),
        percentile(&sims, 0.75),
        percentile(&sims, 0.90),
        sims.last().copied().unwrap_or(f64::NAN),
        mean,
    );

    // θ_nb 档位对比（固定 min_cluster_size=3，原始分布不自动降阈值）。
    println!("\nθ_nb 档位对比 (min_cluster_size=3, β1=0.4, β2=0.3):");
    println!(
        "  {:<8} {:<6} {:<8} {:<12} {:<10}",
        "θ_nb", "簇数", "孤立点", "孤立点比例", "样本覆盖"
    );
    let mut suggested: Vec<(f64, usize)> = Vec::new();
    for &theta in &[
        0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85,
    ] {
        let (clusters, noise, ratio) = cluster_stats(&samples, theta, 3, beta1, beta2);
        let covered = samples.len() - noise;
        println!(
            "  {theta:<8.2} {clusters:<6} {noise:<8} {ratio:<12.3} {covered}/{}",
            samples.len()
        );
        if (0.05..=0.5).contains(&ratio) && clusters >= 2 {
            suggested.push((theta, clusters));
        }
    }

    // min_cluster_size 档位（固定 θ_nb≈P50）。
    let base_theta = percentile(&sims, 0.50);
    println!("\nmin_cluster_size 档位对比 (θ_nb≈P50={base_theta:.3}, β1=0.4, β2=0.3):");
    for &ms in &[2, 3, 4, 5] {
        let (clusters, noise, ratio) = cluster_stats(&samples, base_theta, ms, beta1, beta2);
        println!(
            "  min_cluster_size={ms} → 簇数={clusters}, 孤立点={noise} ({ratio:.3}), 覆盖={}/{}",
            samples.len() - noise,
            samples.len()
        );
    }

    // 建议。
    println!("\n==== 建议 ====");
    println!(
        "  fused_similarity P50={:.3}, P75={:.3}, P90={:.3}",
        percentile(&sims, 0.50),
        percentile(&sims, 0.75),
        percentile(&sims, 0.90)
    );
    if !suggested.is_empty() {
        println!(
            "  θ_nb 定初值建议区间（簇数≥2 且孤立点 5%~50%）: {:?}",
            suggested
        );
    } else {
        println!(
            "  θ_nb 定初值建议：P50~P75 = [{:.3}, {:.3}]（簇数不足时下调）",
            percentile(&sims, 0.50),
            percentile(&sims, 0.75)
        );
    }
    println!("  （完整分布与决策登记见测试输出 / 摸底报告）");

    // ===== 二维扫描：β 权重（β1/β2，进而 β3）× θ_nb =====
    // 目标：找到能细分出 ≥2 个行为簇 且 无单一通吃大簇 的参数组合，
    // 克服"簇数恒=1"问题（β3 关键词 Jaccard 兜底拉高整体相似度）。
    println!("\n==== β×θ 二维扫描（min_cluster_size=3）====");
    println!(
        "  {:<6} {:<6} {:<6} {:<6} {:<5} {:<8} {:<8} {:<10}",
        "β1", "β2", "β3", "θ_nb", "簇数", "孤立点", "最大簇", "最大簇占比"
    );
    let mut best: Vec<(f64, f64, f64, usize, usize, f64, f64)> = Vec::new(); // β1,β2,θ,簇数,最大簇,比例,最大占比
    for &(b1, b2) in &[
        (0.40f64, 0.30f64), // 默认 β3=0.3
        (0.60, 0.30),       // β3=0.1
        (0.80, 0.10),       // β3=0.1
        (0.70, 0.20),       // β3=0.1
        (0.50, 0.40),       // β3=0.1
        (0.90, 0.05),       // β3=0.05，双通道主导
        (0.75, 0.20),       // β3=0.05
        (0.85, 0.10),       // β3=0.05
    ] {
        let b3 = (1.0 - b1 - b2).max(0.0);
        for &theta in &[0.35f64, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75] {
            let (clusters, noise, ratio, max_size, max_ratio) =
                cluster_stats_rich(&samples, theta, 3, b1, b2);
            println!(
                "  {b1:<6.2} {b2:<6.2} {b3:<6.2} {theta:<6.2} {clusters:<5} {noise:<8} {max_size:<8} {max_ratio:<10.3}"
            );
            // 合格：簇数≥2 且 最大簇占比 < 0.9（避免单簇通吃）、孤立点 < 60%。
            if clusters >= 2 && max_ratio < 0.9 && ratio < 0.6 {
                best.push((b1, b2, theta, clusters, noise, ratio, max_ratio));
            }
        }
    }

    println!("\n==== 合格组合（簇数≥2 且最大簇占比<0.9 且孤立点<60%）====");
    if best.is_empty() {
        println!(
            "  无合格组合 —— 事件语义相似度过高，需要在聚类前增强 paraphrase 区分度或改用其他聚类策略。"
        );
    } else {
        best.sort_by(|a, b| (b.3).cmp(&a.3).then(b.4.cmp(&a.4)));
        for (b1, b2, theta, clusters, noise, ratio, max_ratio) in best.iter().take(12) {
            println!(
                "  β1={b1} β2={b2} β3={:.2} θ_nb={theta} → 簇数={clusters}, 孤立点={noise}({ratio:.2}), 最大簇占比={max_ratio:.2}",
                1.0 - b1 - b2
            );
        }
    }
    println!("  （最终参数定稿建议见下方，依据：簇分离性优先 + 系数连续性）");

    // 最终定稿建议（由负责人依据上表裁决）。
    println!("\n==== 最终参数定稿建议（供裁决）====");
    println!("  默认标定: θ_nb=0.5  β1=0.4  β2=0.3  (β3=0.3)  min_cluster_size=3");
    println!(
        "  说明: 若二维扫描出合格组合，取“簇数最多且最大簇占比最低”的 (β1,β2,θ_nb)；
         若仍无合格组合，则维持默认 β 权重并由负责人评估聚类策略是否需调整。"
    );
}
