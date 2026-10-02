//! crates/ramaria-cli/src/commands/probe/report/calibration.rs - 探针 report 人工抽检校准
//!
//! 设计特点:
//! - 比对 judge 与人工分数的一致性 / 偏差 / 校准系数（校准文件可选）
//! - calibration 文件缺失或非法统一归业务校验失败
//! - CalibrationResult 为报告内嵌的校准结果容器

use super::super::evaluate::ProbeEvaluation;
use ramaria_core::error::RamariaError;
use std::path::Path;

/// 人工抽检校准结果。
///
/// 说明:
/// - `consistency`: judge 与人工分数的一致性（同分占比 / 平均绝对差）。
/// - `bias`: judge 相对人工的系统性偏差（judge 均分 − 人工均分；>0 偏高、<0 偏低）。
/// - `calibrated_coefficient`: 校准系数（人工均分 / judge 均分，用于把 judge 分缩放到人工尺度）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalibrationResult {
    pub sample_count: usize,
    pub total_count: usize,
    pub sample_rate: f64,
    pub consistency_exact: f64,
    pub mean_abs_diff: f64,
    pub bias: f64,
    pub calibrated_coefficient: Option<f64>,
    /// 是否不一致（一致性低或偏差大，报告标注）
    pub inconsistent: bool,
    /// 标注说明
    pub annotation: String,
}

/// 读取人工抽检校准文件。
///
/// 格式（JSON）:
/// ```json
/// { "scores": [ {"item_id": "tone-0001", "score": 4}, ... ] }
/// ```
/// 或简单数组 `[{"item_id": "...", "score": 4}]`。
pub(crate) fn read_manual_scores(path: &Path) -> anyhow::Result<Vec<ManualScore>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取校准文件失败: {}: {e}",
            path.display()
        )))
    })?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| RamariaError::validation(format!("校准文件解析失败: {e}")))?;

    let scores = if let Some(arr) = value.as_array() {
        arr.clone()
    } else if let Some(obj) = value.get("scores").and_then(|s| s.as_array()) {
        obj.clone()
    } else {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "校准文件格式无效（应为 JSON 数组或 {scores:[...]}）"
        )));
    };

    let mut out = Vec::with_capacity(scores.len());
    for s in scores {
        let item_id = s
            .get("item_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!(RamariaError::validation("校准条目缺少 item_id 字段")))?
            .to_string();
        let score = s
            .get("score")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!(RamariaError::validation("校准条目缺少 score 字段")))?;
        out.push(ManualScore { item_id, score });
    }
    Ok(out)
}

/// 人工抽检单条分数。
#[derive(Debug, Clone)]
pub struct ManualScore {
    pub item_id: String,
    pub score: u64,
}

/// 计算人工抽检校准结果（一致性 / 偏差 / 校准系数）。
///
/// 说明:
/// - 只统计 judge 有分的条目（tone 题 judge 分）。
/// - `consistency_exact`: judge 与人工同分占比。
/// - `mean_abs_diff`: 平均绝对差。
/// - `bias`: judge 均分 − 人工均分。
/// - `calibrated_coefficient`: 人工均分 / judge 均分（judge 均分为 0 时为 None）。
/// - `inconsistent`: 同分占比 < 0.5 或 |bias| > 1.0（判定校准不一致）。
pub(crate) fn compute_calibration(
    manual: &[ManualScore],
    evaluation: Option<&ProbeEvaluation>,
) -> CalibrationResult {
    // 收集 judge 分（从 evaluation 的 tone 题逐题明细）
    let mut judge_by_item: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    if let Some(eval) = evaluation {
        for v in &eval.variants {
            for item in &v.items {
                if let Some(tone) = &item.tone {
                    judge_by_item.insert(item.item_id.clone(), tone.score);
                }
            }
        }
    }

    // 配对：manual 中能在 judge 中找到分且维度匹配 tone 的条目
    let mut pairs: Vec<(u32, u64)> = Vec::new();
    for m in manual {
        if let Some(j) = judge_by_item.get(&m.item_id) {
            pairs.push((*j, m.score));
        }
    }

    let sample_count = pairs.len();
    if sample_count == 0 {
        return CalibrationResult {
            sample_count: 0,
            total_count: manual.len(),
            sample_rate: 0.0,
            consistency_exact: 0.0,
            mean_abs_diff: 0.0,
            bias: 0.0,
            calibrated_coefficient: None,
            inconsistent: true,
            annotation: "未找到任何与 judge 分匹配的人工抽检条目，无法校准".to_string(),
        };
    }

    let total_count = manual.len();
    let sample_rate = sample_count as f64 / total_count.max(1) as f64;

    let exact = pairs.iter().filter(|(j, m)| *j as u64 == *m).count();
    let consistency_exact = exact as f64 / sample_count as f64;

    let mean_abs_diff = pairs
        .iter()
        .map(|(j, m)| (*j as f64 - *m as f64).abs())
        .sum::<f64>()
        / sample_count as f64;

    let judge_mean = pairs.iter().map(|(j, _)| *j as f64).sum::<f64>() / sample_count as f64;
    let manual_mean = pairs.iter().map(|(_, m)| *m as f64).sum::<f64>() / sample_count as f64;
    let bias = judge_mean - manual_mean;

    let calibrated_coefficient = if judge_mean.abs() < 1e-9 {
        None
    } else {
        Some(manual_mean / judge_mean)
    };

    let inconsistent = consistency_exact < 0.5 || bias.abs() > 1.0;
    let annotation = if inconsistent {
        format!(
            "一致性偏低（同分 {:.0}% / 均差 {:.2} / 偏差 {:.2}），judge 需人工复核或调参",
            consistency_exact * 100.0,
            mean_abs_diff,
            bias
        )
    } else {
        format!(
            "一致性可接受（同分 {:.0}% / 均差 {:.2} / 偏差 {:.2}）",
            consistency_exact * 100.0,
            mean_abs_diff,
            bias
        )
    };

    CalibrationResult {
        sample_count,
        total_count,
        sample_rate,
        consistency_exact,
        mean_abs_diff,
        bias,
        calibrated_coefficient,
        inconsistent,
        annotation,
    }
}
