//! crates/ramaria-eval/src/report/compare.rs - Ramaria 评测报告配对比较
//!
//! 设计特点:
//! - 条件池以 (档位, 管线, 预算) 为键，同一键只取最先装载的运行，避免重复计数
//! - 只有两侧都产出有效 rubric 分的题参与配对，差值方向固定为 left - right
//! - 比较集固定：逐档消融、oracle 对照、预算轴；条件不存在时不凭空补零
//! - BH-FDR 跨全部比较一次校正，显著线为 p_adj < 0.05 且 95% CI 不含 0；d_z 只作效应量输出
//! - 样本不足只记缺项并降级为不显著，不让统计失败阻断报告

use std::collections::{BTreeMap, BTreeSet};

use crate::stats::bh_fdr_adjust;

use super::artifact::{ComparisonRow, MissingFact, MissingKind};
use super::load::{LoadedRun, condition_axes};
use super::stats::{self, MIN_CI_PAIRS, RatioMap, SIGNIFICANCE_LEVEL};

/// 消融比较涉及的管线（按展示顺序）。
pub(crate) const ABLATIONS: [&str; 6] = [
    "l1",
    "l1-l2",
    "l1-l2-l3",
    "l1-l2-l3-utt",
    "flat-rag",
    "full-context",
];

/// 预算轴固定比较对（left 预算, right 预算）。
pub(crate) const BUDGET_PAIRS: [(u32, u32); 2] = [(1600, 800), (3200, 1600)];

/// 条件池键：档位、管线、预算（字符）。
pub(crate) type ConditionKey = (String, String, u32);

/// 条件池：同一条件下的逐题得分率。
pub(crate) type Pool = BTreeMap<ConditionKey, Series>;

/// 一个条件的逐题得分率。
///
/// 字段约定:
/// - `condition_id`: 条件标识（用于比较行左右两列）；
/// - `ratios`: 题号 → 有效 rubric 得分率。
pub(crate) struct Series {
    pub condition_id: String,
    pub ratios: RatioMap,
}

// =========================================================
// 条件池
// =========================================================

/// 由主运行与对照运行建立条件池。
///
/// 说明:
/// - 只收 `ratio` 非空且 `judge_failed` 为 false 的题；
/// - 同一 (档位, 管线, 预算) 在多个运行出现时，按传入顺序取第一个运行；
/// - 题号在池内唯一，同一题重复出现以最后一次为准。
pub(crate) fn build_pool(runs: &[&LoadedRun]) -> Pool {
    let mut pool = Pool::new();
    let mut owner: BTreeMap<ConditionKey, usize> = BTreeMap::new();
    for (index, run) in runs.iter().enumerate() {
        for item in &run.items {
            let Some(rubric) = item.rubric.as_ref() else {
                continue;
            };
            if rubric.judge_failed {
                continue;
            }
            let Some(ratio) = rubric.ratio else {
                continue;
            };
            let key = (item.tier.clone(), item.pipeline.clone(), item.budget_chars);
            match owner.get(&key) {
                Some(previous) if *previous != index => continue,
                Some(_) => {}
                None => {
                    owner.insert(key.clone(), index);
                }
            }
            let series = pool.entry(key).or_insert_with(|| Series {
                condition_id: item.condition_id.clone(),
                ratios: BTreeMap::new(),
            });
            series.ratios.insert(item.question_id.clone(), ratio);
        }
    }
    pool
}

/// 条件标识 →（档位, 管线, 预算）。
///
/// 说明: 条件标识形如 `{tier}/{pipeline}/{budget}/{model_index}`，档位本身可含 `/`，
/// 因此从右侧三段解析，其余部分整体作为档位。
pub(crate) fn split_condition_id(condition_id: &str) -> Option<(String, String, u32)> {
    let parts: Vec<&str> = condition_id.split('/').collect();
    if parts.len() < 4 {
        return None;
    }
    let budget_chars = parts[parts.len() - 2].parse::<u32>().ok()?;
    let pipeline = parts[parts.len() - 3].to_string();
    let tier = parts[..parts.len() - 3].join("/");
    Some((tier, pipeline, budget_chars))
}

// =========================================================
// 比较集
// =========================================================

/// 组装全部比较并完成 BH 校正。
///
/// 参数:
/// - `primary`: 主运行；
/// - `oracle`: 压缩层 oracle（目录不存在或未装载时为 `None`）；
/// - `pool`: 主运行与对照运行的条件池；
/// - `budget_axis`: 是否启用预算轴（显式传入预算对照运行时为 true）；
/// - `missing`: 缺项收集器。
pub(crate) fn build_comparisons(
    primary: &LoadedRun,
    oracle: Option<&LoadedRun>,
    pool: &Pool,
    budget_axis: bool,
    missing: &mut Vec<MissingFact>,
) -> Vec<ComparisonRow> {
    let anchors = tier_anchors(primary);
    let mut rows = Vec::new();
    rows.extend(ablation_rows(&anchors, pool));
    rows.extend(oracle_rows(&anchors, oracle, pool, missing));
    if budget_axis {
        rows.extend(budget_rows(pool, missing));
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    finalize(&mut rows, missing);
    rows
}

/// 每个档位的主预算（该档全部条件中的最小预算）。
///
/// 说明: 主运行只给一个预算时即为该预算；同一档位多预算时取最小预算作主表锚点，
/// 其余预算由预算轴比较覆盖。
fn tier_anchors(primary: &LoadedRun) -> BTreeMap<String, u32> {
    let mut anchors: BTreeMap<String, u32> = BTreeMap::new();
    let Some(score) = &primary.score else {
        return anchors;
    };
    let axes = condition_axes(primary);
    for condition in &score.conditions {
        let Some(axis) = axes.get(&condition.condition_id) else {
            continue;
        };
        anchors
            .entry(axis.tier.clone())
            .and_modify(|budget| *budget = (*budget).min(axis.budget_chars))
            .or_insert(axis.budget_chars);
    }
    anchors
}

/// 逐档消融比较：layered 相对各消融管线。
fn ablation_rows(anchors: &BTreeMap<String, u32>, pool: &Pool) -> Vec<ComparisonRow> {
    let mut rows = Vec::new();
    for (tier, budget) in anchors {
        let Some(layered) = pool.get(&(tier.clone(), "layered".to_string(), *budget)) else {
            continue;
        };
        for pipeline in ABLATIONS {
            let Some(other) = pool.get(&(tier.clone(), pipeline.to_string(), *budget)) else {
                continue;
            };
            rows.push(make_row(
                format!("{tier}/layered-vs-{pipeline}"),
                tier.clone(),
                layered,
                other,
            ));
        }
    }
    rows
}

/// oracle 对照：每档 oracle-l1 相对 layered。
fn oracle_rows(
    anchors: &BTreeMap<String, u32>,
    oracle: Option<&LoadedRun>,
    pool: &Pool,
    missing: &mut Vec<MissingFact>,
) -> Vec<ComparisonRow> {
    let mut rows = Vec::new();
    let oracle_scored = oracle.is_some_and(|run| run.score.is_some());
    for (tier, budget) in anchors {
        let Some(layered) = pool.get(&(tier.clone(), "layered".to_string(), *budget)) else {
            continue;
        };
        let exact = pool.get(&(tier.clone(), "oracle-l1".to_string(), *budget));
        let fallback = pool
            .iter()
            .filter(|((candidate_tier, pipeline, _), _)| {
                candidate_tier == tier && pipeline == "oracle-l1"
            })
            .min_by_key(|((_, _, candidate_budget), _)| *candidate_budget)
            .map(|(_, series)| series);
        match exact.or(fallback) {
            Some(other) => rows.push(make_row(
                format!("{tier}/oracle-l1-vs-layered"),
                tier.clone(),
                other,
                layered,
            )),
            None if oracle_scored => missing.push(MissingFact::report(
                MissingKind::NotRun,
                format!("oracle 运行缺少档位 {tier} 的 oracle-l1 条件"),
            )),
            None => {}
        }
    }
    rows
}

/// 预算轴：目标预算对的同档同管线比较。
///
/// 说明:
/// - 只在显式挂载预算对照运行后启用；
/// - 左条件取目标预算（如 1600），右条件取对照预算（如 800），同档同管线，按题号配对；
/// - 缺一侧时只为 layered 记缺项，避免与消融管线无关的预算缺口刷屏。
fn budget_rows(pool: &Pool, missing: &mut Vec<MissingFact>) -> Vec<ComparisonRow> {
    let mut rows = Vec::new();
    let keys: BTreeSet<(String, String)> = pool
        .keys()
        .map(|(tier, pipeline, _)| (tier.clone(), pipeline.clone()))
        .collect();
    for (tier, pipeline) in keys {
        for (left_budget, right_budget) in BUDGET_PAIRS {
            let left = pool.get(&(tier.clone(), pipeline.clone(), left_budget));
            let right = pool.get(&(tier.clone(), pipeline.clone(), right_budget));
            match (left, right) {
                (Some(left), Some(right)) => rows.push(make_row(
                    format!("{tier}/{pipeline}/b{left_budget}-vs-b{right_budget}"),
                    tier.clone(),
                    left,
                    right,
                )),
                (Some(_), None) if pipeline == "layered" => {
                    missing.push(MissingFact::report(
                        MissingKind::NotRun,
                        format!(
                            "预算轴缺少条件: {tier}/{pipeline}/{right_budget}（可用 --compare 挂载对应运行）"
                        ),
                    ));
                }
                (None, Some(_)) if pipeline == "layered" => {
                    missing.push(MissingFact::report(
                        MissingKind::NotRun,
                        format!(
                            "预算轴缺少条件: {tier}/{pipeline}/{left_budget}（可用 --compare 挂载对应运行）"
                        ),
                    ));
                }
                _ => {}
            }
        }
    }
    rows
}

/// 构造一条比较行（统计量在 `finalize` 中补校正值）。
fn make_row(id: String, tier: String, left: &Series, right: &Series) -> ComparisonRow {
    let stats = stats::paired(&left.ratios, &right.ratios);
    ComparisonRow {
        id,
        tier,
        left: left.condition_id.clone(),
        right: right.condition_id.clone(),
        n_pairs: stats.n,
        mean_diff: stats.mean,
        ci_low: stats.ci.map(|(low, _)| low),
        ci_high: stats.ci.map(|(_, high)| high),
        effect_dz: stats.dz,
        p_value: stats.p,
        p_adjusted: None,
        significant: false,
    }
}

/// 跨全部比较做 BH-FDR 校正并写入显著标记；样本不足记缺项。
fn finalize(rows: &mut [ComparisonRow], missing: &mut Vec<MissingFact>) {
    let p_values: Vec<f64> = rows.iter().map(|row| row.p_value.unwrap_or(1.0)).collect();
    let adjusted = bh_fdr_adjust(&p_values);
    for (row, adjusted) in rows.iter_mut().zip(adjusted) {
        row.p_adjusted = Some(adjusted);
        row.significant = is_significant(row);
        if row.n_pairs < MIN_CI_PAIRS {
            missing.push(MissingFact::report(
                MissingKind::Unsupported,
                "样本不足（n<10），不参与显著判定",
            ));
        }
    }
}

/// 显著线：p_adj < 0.05 且 95% CI 不含 0 同时成立。
///
/// 说明: d_z 照常输出作效应量参考，不参与判定——小样本量下差异可能稳定显著而效应量偏小，
/// 以区间是否跨零为准。
fn is_significant(row: &ComparisonRow) -> bool {
    let (Some(p), Some(low), Some(high)) = (row.p_adjusted, row.ci_low, row.ci_high) else {
        return false;
    };
    p < SIGNIFICANCE_LEVEL && (low > 0.0 || high < 0.0)
}
