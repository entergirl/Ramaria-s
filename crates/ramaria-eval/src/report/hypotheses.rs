//! crates/ramaria-eval/src/report/hypotheses.rs - Ramaria 评测报告结论判定
//!
//! 设计特点:
//! - H1~H6 的判定规则写死在代码里，不随输入改变阈值与方向
//! - 输入不足一律 `not_evidenced`，部分满足写 `partial`，不推断未观测的结论
//! - 统计比较沿用报告的比较行：显著线为 p_adj < 0.05 且 95% CI 不含 0
//! - 跨档趋势只比较 layered 各档均值：非配对，每档 bootstrap 出区间，不进入 BH 家族
//! - 跨档结论注明题集重叠数与各档样本量，不把不同抽样的题当配对
//! - 每条结论固定带一句摘要与若干证据要点，方便直接抄进报告

use std::collections::BTreeMap;

use crate::score::PersonaSummary;

use super::artifact::{ComparisonRow, Hypothesis, HypothesisStatus, ReportCondition};
use super::compare::{Pool, split_condition_id};
use super::format;
use super::stats;

/// H3 的消融管线（四条记忆层消融）。
const LAYER_ABLATIONS: [&str; 4] = ["l1", "l1-l2", "l1-l2-l3", "l1-l2-l3-utt"];

// =========================================================
// 入口
// =========================================================

/// 依次判定 H1~H6。
pub(crate) fn judge(
    conditions: &[ReportCondition],
    comparisons: &[ComparisonRow],
    pool: &Pool,
) -> Vec<Hypothesis> {
    vec![
        h1_scale(conditions, pool),
        h2_compression(comparisons),
        h3_layers(comparisons),
        h4_budget(comparisons),
        h5_credibility(conditions),
        h6_persona(conditions),
    ]
}

// =========================================================
// H1 规模
// =========================================================

/// H1：layered 得分率随规模严格递减，且最小档与最大档的 95% CI 不重叠。
///
/// 说明:
/// - 各档抽样题集不同，只比较每档均值，不按题号配对，也不伪造配对键；
/// - 每档均值的区间由 2000 次 bootstrap（按题重采样、固定种子）得出；
/// - 跨档题集重叠数只作抽样透明度说明，不参与判定。
fn h1_scale(conditions: &[ReportCondition], pool: &Pool) -> Hypothesis {
    let mut by_tier: BTreeMap<String, (u32, f64)> = BTreeMap::new();
    for condition in conditions {
        if condition.pipeline != "layered" {
            continue;
        }
        let Some(ratio) = condition
            .rubric
            .as_ref()
            .and_then(|rubric| rubric.micro_mean_ratio)
        else {
            continue;
        };
        by_tier
            .entry(condition.tier.clone())
            .and_modify(|entry| {
                if condition.budget_chars < entry.0 {
                    *entry = (condition.budget_chars, ratio);
                }
            })
            .or_insert((condition.budget_chars, ratio));
    }

    let mut scaled: Vec<(u32, String, u32, f64)> = by_tier
        .into_iter()
        .filter_map(|(tier, (budget, ratio))| {
            scale_of(&tier).map(|scale| (scale, tier, budget, ratio))
        })
        .collect();
    scaled.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let points: Vec<TierPoint> = scaled
        .into_iter()
        .map(|(_, tier, budget, ratio)| {
            let values = tier_values(pool, &tier, budget);
            let mean = values
                .as_ref()
                .filter(|values| !values.is_empty())
                .map(|values| values.iter().sum::<f64>() / values.len() as f64)
                .unwrap_or(ratio);
            TierPoint {
                questions: values.as_ref().map(Vec::len).unwrap_or(0),
                ci: values.as_deref().and_then(stats::bootstrap_ci),
                tier,
                budget,
                mean,
            }
        })
        .collect();

    let mut evidence: Vec<String> = points
        .iter()
        .map(|point| {
            format!(
                "{} layered 均值 {}（95% CI {}，n={}，预算 {} 字符）",
                point.tier,
                format::fixed(Some(point.mean), 3),
                interval(point.ci),
                point.questions,
                point.budget
            )
        })
        .collect();

    if points.len() < 3 {
        return hypothesis(
            "H1",
            HypothesisStatus::NotEvidenced,
            "可比较的规模档位少于 3 个，规模效应未取证",
            evidence,
        );
    }
    let first = &points[0];
    let last = &points[points.len() - 1];
    let overlap_text = match overlap_count(pool, first, last) {
        Some(count) => format!("跨档题集重叠 n={count}（{} ∩ {}）", first.tier, last.tier),
        None => format!(
            "跨档题集重叠未取证（{} ∩ {} 缺逐题得分）",
            first.tier, last.tier
        ),
    };
    let note = format!("非配对，{overlap_text}");
    evidence.push(note.clone());

    if !points.windows(2).all(|pair| pair[0].mean > pair[1].mean) {
        return hypothesis(
            "H1",
            HypothesisStatus::NotEvidenced,
            format!("layered 得分率未随规模严格递减，规模效应未取证；{note}"),
            evidence,
        );
    }
    let disjoint = first
        .ci
        .zip(last.ci)
        .is_some_and(|(left, right)| left.0 > right.1);
    let chain = format!(
        "{} {} → {} {}",
        first.tier,
        format::fixed(Some(first.mean), 3),
        last.tier,
        format::fixed(Some(last.mean), 3)
    );
    let (status, summary) = if disjoint {
        (
            HypothesisStatus::Supported,
            format!(
                "layered 得分率随规模严格递减（{chain}），最小档与最大档 95% CI 不重叠；{note}"
            ),
        )
    } else {
        (
            HypothesisStatus::Partial,
            format!(
                "layered 得分率随规模严格递减（{chain}），但最小档与最大档 95% CI 重叠或区间不足；{note}"
            ),
        )
    };
    hypothesis("H1", status, summary, evidence)
}

/// H1 的一个档位观测点。
///
/// 字段约定:
/// - `tier` / `budget`: 档位与主预算；
/// - `mean`: 该档 layered 的逐题均值得分率；
/// - `ci`: 均值的 95% bootstrap 区间；题数不足时为 `None`；
/// - `questions`: 参与均值的题数。
struct TierPoint {
    tier: String,
    budget: u32,
    mean: f64,
    ci: Option<(f64, f64)>,
    questions: usize,
}

/// 一档 layered 的逐题得分率（题号升序）；条件池缺失时为 `None`。
fn tier_values(pool: &Pool, tier: &str, budget: u32) -> Option<Vec<f64>> {
    pool.get(&(tier.to_string(), "layered".to_string(), budget))
        .map(|series| series.ratios.values().copied().collect())
}

/// 两档题集的重叠题数；任一档缺逐题得分时为 `None`。
fn overlap_count(pool: &Pool, left: &TierPoint, right: &TierPoint) -> Option<usize> {
    let left = pool.get(&(left.tier.clone(), "layered".to_string(), left.budget))?;
    let right = pool.get(&(right.tier.clone(), "layered".to_string(), right.budget))?;
    Some(
        left.ratios
            .keys()
            .filter(|question| right.ratios.contains_key(*question))
            .count(),
    )
}

/// 置信区间文本；区间缺失写 `-`。
fn interval(ci: Option<(f64, f64)>) -> String {
    match ci {
        Some((low, high)) => format!(
            "[{}, {}]",
            format::fixed(Some(low), 3),
            format::fixed(Some(high), 3)
        ),
        None => "-".to_string(),
    }
}

// =========================================================
// H2 压缩
// =========================================================

/// H2：oracle 相对 layered 的配对差显著为正（摘要损失可量化）。
fn h2_compression(comparisons: &[ComparisonRow]) -> Hypothesis {
    let oracle_rows: Vec<&ComparisonRow> = comparisons
        .iter()
        .filter(|row| pipeline_of(&row.left).as_deref() == Some("oracle-l1"))
        .collect();
    let mut evidence: Vec<String> = oracle_rows
        .iter()
        .map(|row| {
            format!(
                "{}: oracle - layered = {}（n={}，{}，d_z={}）",
                row.tier,
                format::delta(row.mean_diff),
                row.n_pairs,
                format::p_adj_label(row.p_adjusted),
                format::fixed(row.effect_dz, 2)
            )
        })
        .collect();
    if oracle_rows.is_empty() {
        return hypothesis(
            "H2",
            HypothesisStatus::NotEvidenced,
            "oracle 未执行或没有可配对条件，摘要损失未取证",
            evidence,
        );
    }
    let supported = oracle_rows
        .iter()
        .any(|row| row.significant && row.mean_diff.unwrap_or(0.0) > 0.0);
    evidence.push(format!(
        "共 {} 档 oracle 对照，显著为正 {} 档",
        oracle_rows.len(),
        oracle_rows
            .iter()
            .filter(|row| row.significant && row.mean_diff.unwrap_or(0.0) > 0.0)
            .count()
    ));
    let (status, summary) = if supported {
        (
            HypothesisStatus::Supported,
            "oracle 与 layered 的配对差显著为正，摘要损失可量化".to_string(),
        )
    } else {
        (
            HypothesisStatus::Partial,
            "oracle 与 layered 的差值未达显著线，摘要损失只能定性".to_string(),
        )
    };
    hypothesis("H2", status, summary, evidence)
}

// =========================================================
// H3 层级
// =========================================================

/// H3：layered 相对记忆层消融的显著优势条数。
///
/// 说明: 分母只计已出比较的消融条数，未出比较的单列，不按「不显著」合并计算。
fn h3_layers(comparisons: &[ComparisonRow]) -> Hypothesis {
    let mut by_tier: BTreeMap<String, BTreeMap<String, &ComparisonRow>> = BTreeMap::new();
    for row in comparisons {
        let Some((tier, pipeline)) = row.id.rsplit_once("/layered-vs-") else {
            continue;
        };
        if LAYER_ABLATIONS.contains(&pipeline) {
            by_tier
                .entry(tier.to_string())
                .or_default()
                .insert(pipeline.to_string(), row);
        }
    }
    if by_tier.is_empty() {
        return hypothesis(
            "H3",
            HypothesisStatus::NotEvidenced,
            "没有可用的消融比较，层级效应未取证",
            Vec::new(),
        );
    }

    let mut tiers: Vec<(&String, &BTreeMap<String, &ComparisonRow>)> = by_tier.iter().collect();
    tiers.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| scale_rank(left.0).cmp(&scale_rank(right.0)))
            .then_with(|| left.0.cmp(right.0))
    });
    let headline = tiers[0];
    let compared: Vec<&ComparisonRow> = LAYER_ABLATIONS
        .iter()
        .filter_map(|pipeline| headline.1.get(*pipeline).copied())
        .collect();
    let total = compared.len();
    let wins = compared
        .iter()
        .filter(|row| row.significant && row.mean_diff.unwrap_or(0.0) > 0.0)
        .count();

    let mut evidence = Vec::new();
    for (tier, rows) in &tiers {
        for pipeline in LAYER_ABLATIONS {
            match rows.get(pipeline) {
                Some(row) => evidence.push(format!(
                    "{tier} vs {pipeline}: 均值差 {}（n={}，{}，{}）",
                    format::delta(row.mean_diff),
                    row.n_pairs,
                    format::p_adj_label(row.p_adjusted),
                    if row.significant {
                        "显著"
                    } else {
                        "不显著"
                    }
                )),
                None => evidence.push(format!("{tier} vs {pipeline}: 未出比较")),
            }
        }
    }

    let uncompared = LAYER_ABLATIONS.len() - total;
    let (status, summary) = if total == 0 {
        (
            HypothesisStatus::NotEvidenced,
            format!("在 {} 上没有可用的消融比较，层级效应未取证", headline.0),
        )
    } else if total == LAYER_ABLATIONS.len() && wins == total {
        (
            HypothesisStatus::Supported,
            format!("在 {} 上显著优于 {wins}/{total} 条已比较消融", headline.0),
        )
    } else if wins > 0 {
        (
            HypothesisStatus::Partial,
            if uncompared > 0 {
                format!(
                    "在 {} 上显著优于 {wins}/{total} 条已比较消融（{uncompared} 条未比较）",
                    headline.0
                )
            } else {
                format!("在 {} 上显著优于 {wins}/{total} 条已比较消融", headline.0)
            },
        )
    } else if uncompared > 0 {
        (
            HypothesisStatus::NotEvidenced,
            format!(
                "在 {} 上未取得对消融的显著优势（0/{total}，{uncompared} 条未比较）",
                headline.0
            ),
        )
    } else {
        (
            HypothesisStatus::NotEvidenced,
            format!("在 {} 上未取得对消融的显著优势（0/{total}）", headline.0),
        )
    };
    hypothesis("H3", status, summary, evidence)
}

// =========================================================
// H4 预算
// =========================================================

/// H4：1600 相对 800 显著为正，且 3200 相对 1600 无显著增量。
fn h4_budget(comparisons: &[ComparisonRow]) -> Hypothesis {
    let mut first: Vec<&ComparisonRow> = Vec::new();
    let mut second: Vec<&ComparisonRow> = Vec::new();
    for row in comparisons {
        let (Some((_, left_pipeline, left_budget)), Some((_, right_pipeline, right_budget))) = (
            split_condition_id(&row.left),
            split_condition_id(&row.right),
        ) else {
            continue;
        };
        if left_pipeline != "layered" || right_pipeline != "layered" {
            continue;
        }
        if (left_budget, right_budget) == super::compare::BUDGET_PAIRS[0] {
            first.push(row);
        } else if (left_budget, right_budget) == super::compare::BUDGET_PAIRS[1] {
            second.push(row);
        }
    }

    let mut evidence = Vec::new();
    for row in first.iter().chain(second.iter()) {
        evidence.push(format!(
            "{}: 均值差 {}（n={}，{}，{}）",
            row.id,
            format::delta(row.mean_diff),
            row.n_pairs,
            format::p_adj_label(row.p_adjusted),
            if row.significant {
                "显著"
            } else {
                "不显著"
            }
        ));
    }

    if first.is_empty() {
        return hypothesis(
            "H4",
            HypothesisStatus::NotEvidenced,
            "预算轴缺少 1600 与 800 的可配对条件，预算结论未取证",
            evidence,
        );
    }
    let first_ok = first
        .iter()
        .any(|row| row.significant && row.mean_diff.unwrap_or(0.0) > 0.0);
    if !first_ok {
        return hypothesis(
            "H4",
            HypothesisStatus::NotEvidenced,
            "1600 相对 800 未取得显著提升，预算结论未取证",
            evidence,
        );
    }
    let second_exists = !second.is_empty();
    let second_significant = second.iter().any(|row| row.significant);
    let status = if second_exists && !second_significant {
        HypothesisStatus::Supported
    } else {
        HypothesisStatus::Partial
    };
    let summary = if status == HypothesisStatus::Supported {
        "1600 相对 800 显著提升，3200 相对 1600 无显著增量（1600 够用）".to_string()
    } else if second_significant {
        "1600 相对 800 显著提升，但 3200 相对 1600 仍有显著增量".to_string()
    } else {
        "1600 相对 800 显著提升，但缺少 3200 相对 1600 的比较".to_string()
    };
    hypothesis("H4", status, summary, evidence)
}

// =========================================================
// H5 可信
// =========================================================

/// H5：UA / CD 判定率；每类正负例不足 3 条时为 partial。
fn h5_credibility(conditions: &[ReportCondition]) -> Hypothesis {
    let mut ua_judged = 0usize;
    let mut ua_hits = 0usize;
    let mut cd_judged = 0usize;
    let mut cd_hits = 0usize;
    for condition in conditions {
        if let Some(rate) = &condition.credibility.ua_judge {
            ua_judged += rate.judged;
            ua_hits += rate.hits;
        }
        if let Some(rate) = &condition.credibility.cd_judge {
            cd_judged += rate.judged;
            cd_hits += rate.hits;
        }
    }
    if ua_judged == 0 && cd_judged == 0 {
        return hypothesis(
            "H5",
            HypothesisStatus::NotEvidenced,
            "没有 UA / CD 判定，可信层指标未取证",
            Vec::new(),
        );
    }

    let mut evidence = Vec::new();
    let mut small = false;
    let mut parts = Vec::new();
    if ua_judged > 0 {
        let positives = ua_hits;
        let negatives = ua_judged - ua_hits;
        small |= positives.min(negatives) < 3;
        parts.push(format!(
            "UA 拒答率 {}（{}/{}）",
            format::percent(Some(ua_hits as f64 / ua_judged as f64)),
            ua_hits,
            ua_judged
        ));
        evidence.push(format!(
            "UA 判定 {ua_judged} 题：正例 {positives} / 负例 {negatives}"
        ));
    }
    if cd_judged > 0 {
        let positives = cd_hits;
        let negatives = cd_judged - cd_hits;
        small |= positives.min(negatives) < 3;
        parts.push(format!(
            "CD 错误前提接受率 {}（{}/{}）",
            format::percent(Some(cd_hits as f64 / cd_judged as f64)),
            cd_hits,
            cd_judged
        ));
        evidence.push(format!(
            "CD 判定 {cd_judged} 题：正例 {positives} / 负例 {negatives}"
        ));
    }
    let status = if small {
        HypothesisStatus::Partial
    } else {
        HypothesisStatus::Supported
    };
    let summary = if small {
        format!("{}；正负例不足 3 条/类，比例只作参考", parts.join("；"))
    } else {
        parts.join("；")
    };
    hypothesis("H5", status, summary, evidence)
}

// =========================================================
// H6 人格
// =========================================================

/// H6：人格三维回归线；有数据即 partial，不做显著性。
fn h6_persona(conditions: &[ReportCondition]) -> Hypothesis {
    let mut by_tier: BTreeMap<String, Vec<&PersonaSummary>> = BTreeMap::new();
    for condition in conditions {
        if let Some(persona) = &condition.persona {
            by_tier
                .entry(condition.tier.clone())
                .or_default()
                .push(persona);
        }
    }
    if by_tier.is_empty() {
        return hypothesis(
            "H6",
            HypothesisStatus::NotEvidenced,
            "没有人格三维数据，回归线未取证",
            Vec::new(),
        );
    }

    let mut evidence = Vec::new();
    let mut summaries = Vec::new();
    for (tier, persona) in &by_tier {
        let tone = mean_of(persona, |item| item.tone);
        let fact = mean_of(persona, |item| item.fact);
        let emotion = mean_of(persona, |item| item.emotion);
        let tone_spread = spread_of(persona, |item| item.tone);
        let fact_spread = spread_of(persona, |item| item.fact);
        let emotion_spread = spread_of(persona, |item| item.emotion);
        summaries.push(format!(
            "{tier} 语气 {} / 事实 {} / 情感 {}（跨管线差 {} / {} / {}）",
            format::fixed(tone, 2),
            format::fixed(fact, 2),
            format::fixed(emotion, 2),
            format::fixed(tone_spread, 2),
            format::fixed(fact_spread, 2),
            format::fixed(emotion_spread, 2)
        ));
        evidence.push(format!("{tier} 参与三维判分条件数 {}", persona.len()));
    }
    hypothesis(
        "H6",
        HypothesisStatus::Partial,
        format!("人格三维为回归线（不做显著性）：{}", summaries.join("；")),
        evidence,
    )
}

// =========================================================
// 辅助
// =========================================================

/// 构造一条结论。
fn hypothesis(
    id: &str,
    status: HypothesisStatus,
    summary: impl Into<String>,
    evidence: Vec<String>,
) -> Hypothesis {
    Hypothesis {
        id: id.to_string(),
        status,
        summary: summary.into(),
        evidence,
    }
}

/// 档位标识 → 规模值（`u1-y1` → 1）。
fn scale_of(tier: &str) -> Option<u32> {
    tier.split('-').next()?.strip_prefix('u')?.parse().ok()
}

/// 档位排序键：有规模值的按数值，其余排最后。
fn scale_rank(tier: &str) -> u32 {
    scale_of(tier).unwrap_or(u32::MAX)
}

/// 条件标识 → 管线。
fn pipeline_of(condition_id: &str) -> Option<String> {
    split_condition_id(condition_id).map(|(_, pipeline, _)| pipeline)
}

/// 一组人格汇总中某一维的均值；该维无值时返回 `None`。
fn mean_of(
    summaries: &[&PersonaSummary],
    pick: impl Fn(&PersonaSummary) -> Option<f64>,
) -> Option<f64> {
    let values: Vec<f64> = summaries.iter().filter_map(|item| pick(item)).collect();
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// 一组人格汇总中某一维的跨管线极差；少于两个值时为 `None`。
fn spread_of(
    summaries: &[&PersonaSummary],
    pick: impl Fn(&PersonaSummary) -> Option<f64>,
) -> Option<f64> {
    let mut values: Vec<f64> = summaries.iter().filter_map(|item| pick(item)).collect();
    if values.len() < 2 {
        return None;
    }
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    Some(values[values.len() - 1] - values[0])
}
