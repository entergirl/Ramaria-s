//! crates/ramaria-cli/src/commands/probe/report/aux_metrics.rs - 探针 report 辅助指标四件套
//!
//! 设计特点:
//! - 产物可复算的辅助指标：证据链可追溯率 / 行为规则命中率 / 情境路由误用率 / 画像回归
//! - 指标由实验产物（variant 结果 + 事件 + 规则）重算，不依赖运行期额外状态
//! - VariantAuxMetrics 为逐档位容器，AuxiliaryMetrics 为汇总容器

use super::super::evaluate::ProbeEvaluation;
use super::super::types::ProbeVariantResult;

/// 辅助指标四件套。
///
/// 口径说明（基于产物可复算的近似口径）:
/// - `evidence_traceability_rate`（证据链可追溯率）: fact 题中模型回复
///   对 golden 事件 reference 的覆盖率——以已有 `FactItemScore.score ≥ 0.5`
///   （embedding 余弦 + 关键词命中的综合分）判定"回复可回溯到注入事件"的比例。
/// - `behavior_rule_hit_rate`（行为规则命中率，代理口径）: 情绪情境题中
///   模型给出"恰当回应"（emotion rubric ≥ 0.5，即命中安慰/共情或喜悦标记）
///   的比例——代理"行为/共情规则在情境中被触发生效"。无 emotion 题为 None。
/// - `situation_route_misuse_rate`（情境路由误用率，代理口径）: 情境极性被
///   检出（situation_negative/positive）但回复为 0 分（未采用对应规则/冷漠）
///   的题占"有极性样本"的比例——代理"路由识别到情境却未生效"的误用。
/// - `profile_regression`（画像回归 / 跨轮输出稳定性）: 对带 `--repeat` 的档位，
///   取其 fact / tone / emotion **三维** `dimension_scores` 的跨轮 std 平均值
///   （越小 = 输出越稳定，画像推断驱动无随机漂移）。事实维的长度归一 / 事实点
///   重算口径（fact_norm / fact_point）属事实维内部对照，不并入本指标，保持既有口径可比。
///   无 repeat 逐轮明细时为 None。
///
/// 局限：以上为产物级近似（不读真实行为规则库/画像快照），仅用于工具链
/// 交叉验证与结构对照；规则-事件一致性、知识准确率等人工抽样指标不在探针内。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuxiliaryMetrics {
    /// 证据链可追溯率（0.0~1.0；无 fact 题时 None）
    pub evidence_traceability_rate: Option<f64>,
    /// 行为规则命中率代理（0.0~1.0；无 emotion 题时 None）
    pub behavior_rule_hit_rate: Option<f64>,
    /// 情境路由误用率代理（0.0~1.0；无有极性样本时 None）
    pub situation_route_misuse_rate: Option<f64>,
    /// 画像回归 = fact/tone/emotion 三维跨轮分数 std 均值（0.0~；无 repeat 明细时 None）
    pub profile_regression_output_stability: Option<f64>,
    /// 口径与局限说明（必出字段）
    pub annotation: String,
}

/// 计算辅助指标四件套（基于 evaluation 产物，纯函数、可复算）。
///
/// 参数:
/// - `evaluation`: probe evaluate 产物（含逐题 fact/emotion 评分与 repeat
///   逐轮聚合 `dimension_scores`）。
///
/// 说明:
/// - 画像回归只取 fact / tone / emotion 三维（口径固定），
///   `fact_norm` / `fact_point` 为事实维内部对照口径，不参与该指标。
///
/// 返回:
/// - 四件套指标；无对应样本的单项为 None（标注口径而非报错）。
pub(crate) fn compute_auxiliary_metrics(evaluation: &ProbeEvaluation) -> AuxiliaryMetrics {
    // ---- 证据链可追溯率：fact 题回复对 golden 事件的覆盖率 ----
    let mut fact_total = 0usize;
    let mut fact_traceable = 0usize;
    // ---- 行为规则命中 / 情境路由误用：emotion 题（含极性检测与 rubric）----
    let mut emotion_total = 0usize;
    let mut emotion_appropriate = 0usize;
    let mut polarized_total = 0usize;
    let mut polarized_misuse = 0usize;
    // ---- 画像回归：跨轮 fact/tone/emotion 三维 std 均值 ----
    let mut cross_round_stds: Vec<f64> = Vec::new();

    for v in &evaluation.variants {
        for item in &v.items {
            if item.error.is_some() {
                continue;
            }
            match item.dimension.as_str() {
                "fact" => {
                    if let Some(f) = &item.fact {
                        fact_total += 1;
                        if f.score >= 0.5 {
                            fact_traceable += 1;
                        }
                    }
                }
                "emotion" => {
                    if let Some(e) = &item.emotion {
                        emotion_total += 1;
                        if e.score >= 0.5 {
                            emotion_appropriate += 1;
                        }
                        // 路由误用代理：检测到情境极性但回复 0 分（规则未生效）。
                        if e.situation_negative || e.situation_positive {
                            polarized_total += 1;
                            if e.score < 0.5 {
                                polarized_misuse += 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // 画像回归：该档位若带 repeat 逐轮聚合，取 fact/tone/emotion 三维跨轮 std 的均值；
        // fact_norm / fact_point 是事实维内部重算口径，计入会改变维度数、使指标不可比，故排除。
        if let Some(scores) = &v.dimension_scores {
            let base: Vec<f64> = scores
                .iter()
                .filter(|d| matches!(d.dimension.as_str(), "fact" | "tone" | "emotion"))
                .map(|d| d.std)
                .collect();
            if !base.is_empty() {
                let mean_std = base.iter().sum::<f64>() / base.len() as f64;
                cross_round_stds.push(mean_std);
            }
        }
    }

    let evidence_traceability_rate = if fact_total > 0 {
        Some(fact_traceable as f64 / fact_total as f64)
    } else {
        None
    };
    let behavior_rule_hit_rate = if emotion_total > 0 {
        Some(emotion_appropriate as f64 / emotion_total as f64)
    } else {
        None
    };
    let situation_route_misuse_rate = if polarized_total > 0 {
        Some(polarized_misuse as f64 / polarized_total as f64)
    } else {
        None
    };
    let profile_regression_output_stability = if cross_round_stds.is_empty() {
        None
    } else {
        Some(cross_round_stds.iter().sum::<f64>() / cross_round_stds.len() as f64)
    };

    // 口径与局限说明（必出）。
    let mut note = String::from(
        "辅助指标为探针产物可复算近似（代理口径）：证据链可追溯率=fact 回复对 golden 覆盖率 \
         (score≥0.5)；行为规则命中/情境路由误用=emotion rubric 代理（读回复文本与极性，\
         不读真实规则库）；画像回归=repeat 跨轮 fact/tone/emotion 三维 std 均值。规则-事件一致性/知识准确率等\
         人工抽样指标不在探针内。",
    );
    if profile_regression_output_stability.is_none() {
        note.push_str(" 画像回归缺项：未检测到 --repeat 逐轮明细（dimension_scores）。");
    }
    if evaluation.embedding_used {
        note.push_str(" 事实维已含语义余弦。");
    } else {
        note.push_str(" 事实维为纯关键词命中（embedding 不可用）。");
    }

    AuxiliaryMetrics {
        evidence_traceability_rate,
        behavior_rule_hit_rate,
        situation_route_misuse_rate,
        profile_regression_output_stability,
        annotation: note,
    }
}

/// 档位辅助指标（消融报告交叉验证用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantAuxMetrics {
    pub variant_id: String,
    pub description: String,
    /// 平均回复字符数
    pub reply_chars_mean: f64,
    /// 平均耗时（毫秒）
    pub elapsed_ms_mean: f64,
    /// 空回复率（0.0~1.0）
    pub empty_reply_rate: f64,
    /// 成功题数 / 总题数
    pub success_count: usize,
    pub total_count: usize,
}

/// 计算单档位辅助指标（平均回复长度 / 平均耗时 / 空回复率）。
pub(crate) fn variant_aux_metrics(vr: &ProbeVariantResult) -> VariantAuxMetrics {
    let total = vr.runs.len();
    let success = total.saturating_sub(vr.failed_count);
    let mut chars = 0usize;
    let mut ms: u128 = 0;
    let mut empty = 0usize;
    for run in &vr.runs {
        chars += run.metrics.reply_chars;
        ms += run.metrics.elapsed_ms;
        if run.reply.trim().is_empty() {
            empty += 1;
        }
    }
    VariantAuxMetrics {
        variant_id: vr.variant_id.clone(),
        description: vr.description.clone(),
        reply_chars_mean: if total > 0 {
            chars as f64 / total as f64
        } else {
            0.0
        },
        elapsed_ms_mean: if total > 0 {
            ms as f64 / total as f64
        } else {
            0.0
        },
        empty_reply_rate: if total > 0 {
            empty as f64 / total as f64
        } else {
            0.0
        },
        success_count: success,
        total_count: total,
    }
}
