//! crates/ramaria-cli/src/commands/probe/run/experiment/stats.rs - 探针实验跨轮统计聚合
//!
//! 设计特点:
//! - 按档位与题项跨多轮配对，输出均值 / 样本标准差 / 95% 置信区间。
//! - 学生氏 t 分布临界值内置查表，超出范围用近似值。

use crate::commands::probe::types::{
    ItemRepeatStats, MetricStat, ProbeExperiment, ProbeVariantResult, VariantRepeatStats,
};

/// 跨 N 次运行聚合档位逐题统计（按 variant_id + item_id 配对）。
///
/// 配对规则:
/// - 档位按 `variant_id` 对齐（数据集同档位过滤，各轮档位集合一致）。
/// - 题项按 `item_id` 对齐（同为该档位的前 `limit`/全部题）。
/// - 若某轮缺失某 item 的指标（正常不应发生），仅以实际出现的样本聚合，`n`
///   反映真实样本量（`n >= 1`；`n == 1` 时置信区间退化为该样本均值，stddev=0）。
///
/// `rounds` 保留该档位**每一轮**的完整 `ProbeVariantResult`
/// （含逐轮全量 reply），供 evaluate/report 对每轮 reply 分别语义评分后聚合 fact_score
/// 的均值 ± 置信区间（配对统计口径）。
pub(crate) fn aggregate_repeat_stats(rounds: &[ProbeExperiment]) -> Vec<VariantRepeatStats> {
    let mut out = Vec::new();
    // 以最后一轮的档位顺序为准（各轮一致）
    let last = rounds.last().expect("至少一轮");
    for vr in &last.variants {
        let mut per_item = Vec::with_capacity(vr.runs.len());
        // 收集该档位在各轮中的完整结果（逐轮全量 reply，供逐轮评分聚合）。
        let mut round_results: Vec<ProbeVariantResult> = rounds
            .iter()
            .filter_map(|round| {
                round
                    .variants
                    .iter()
                    .find(|v| v.variant_id == vr.variant_id)
                    .cloned()
            })
            .collect();
        for run in &vr.runs {
            // 跨轮收集该 item 的指标
            let mut chars = Vec::new();
            let mut ms = Vec::new();
            for round in rounds {
                let hit = round
                    .variants
                    .iter()
                    .find(|v| v.variant_id == vr.variant_id)
                    .and_then(|vr2| vr2.runs.iter().find(|r| r.item_id == run.item_id));
                if let Some(r) = hit {
                    chars.push(r.metrics.reply_chars as f64);
                    ms.push(r.metrics.elapsed_ms as f64);
                }
            }
            per_item.push(ItemRepeatStats {
                item_id: run.item_id.clone(),
                reply_chars: metric_stat(&chars),
                elapsed_ms: metric_stat(&ms),
            });
        }
        // rounds 与 per_item 仅与"该档位在各轮中实际出现"对齐，序列化按此保留；
        // 若某轮重复运行了同档位多次，round_results 会多于 per_item 行——按档位保留逐轮，
        // 逐轮评分聚合（evaluate/report）以 round_results 内容为准。
        out.push(VariantRepeatStats {
            variant_id: vr.variant_id.clone(),
            per_item,
            rounds: std::mem::take(&mut round_results),
        });
    }
    out
}

/// 计算一组 f64 样本的均值 / 样本标准差 / 95% 置信区间。
///
/// 说明:
/// - 空样本 → 全零（`n=0`）。
/// - `n == 1` → mean 为该样本值、stddev=0、CI 退化为 [sample, sample]。
/// - `n >= 2` → 学生氏 t 分布 95% 置信区间 `mean ± t_{n-1,0.975} * (std/sqrt(n))`，
///   内置临界值（n 3~12 表查，超出用近似 `t ≈ 2.0`）。
pub(crate) fn metric_stat(samples: &[f64]) -> MetricStat {
    let n = samples.len();
    if n == 0 {
        return MetricStat {
            mean: 0.0,
            stddev: 0.0,
            ci_low: 0.0,
            ci_high: 0.0,
            n: 0,
        };
    }
    let mean = samples.iter().sum::<f64>() / n as f64;
    if n == 1 {
        return MetricStat {
            mean,
            stddev: 0.0,
            ci_low: mean,
            ci_high: mean,
            n,
        };
    }
    let variance = samples.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / (n - 1) as f64;
    let stddev = variance.sqrt();
    let t = t_critical_975(n);
    let half = t * stddev / (n as f64).sqrt();
    MetricStat {
        mean,
        stddev,
        ci_low: mean - half,
        ci_high: mean + half,
        n,
    }
}

/// 学生氏 t 分布 95% 双尾临界值（自由度 n-1）。
///
/// 覆盖 n=2..=12（探针 N=2~5 常用）；更大 n 用近似 2.0。
pub(crate) fn t_critical_975(n: usize) -> f64 {
    // 下标 = n-2（自由度 1..=10）
    const T: [f64; 11] = [
        12.706, // n=2, df=1
        4.303,  // n=3, df=2
        3.182,  // n=4, df=3
        2.776,  // n=5, df=4
        2.571,  // n=6, df=5
        2.447,  // n=7, df=6
        2.365,  // n=8, df=7
        2.306,  // n=9, df=8
        2.262,  // n=10, df=9
        2.228,  // n=11, df=10
        2.201,  // n=12, df=11
    ];
    if n >= 2 && n - 2 < T.len() {
        T[n - 2]
    } else {
        2.0
    }
}
