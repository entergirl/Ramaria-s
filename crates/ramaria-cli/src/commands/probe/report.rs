//! crates/ramaria-cli/src/commands/probe/report.rs - 探针 report：档位对比报告 + 定稿建议 + 校准 + 消融
//!
//! 设计特点:
//! - 档位对比报告（`probe report`）：汇总各档位评分生成对比表，给出每维最佳档位与综合定稿建议。
//! - 消融对比（--ablation）：F 组（移除）/ S 组（替代）/ I 组（净增量）三类对照分别配对
//!   做 Wilcoxon 符号秩 + Cohen's d + 95% CI + BH-FDR 判定，并按对照类型分栏表述
//!   （D-V20-006：I_* 保留 B1 基座测净增量、S_* 去 RAG 摘要测替代）。
//! - 等效性检验：并行做 TOST（双单侧 t 检验），补上显著性框架无法证明"零净增量"的盲区；
//!   等效边界取 |d_av|=0.3（合并 SD 口径），配合三态判定
//!   significant_up / significant_down / equivalent / inconclusive。
//! - 辅助指标四件套（产物可复算）：证据链可追溯率 / 行为规则命中率 / 情境路由误用率 / 画像回归。
//! - 人工抽检校准：比对 judge 与人工分数的一致性 / 偏差 / 校准系数（由校准文件驱动，可选）。
//! - 知识层质量：基于评分数值中的事实维题目评估误报 / 漏报率（目标 <10%）；
//!   双口径（含记忆注入 / 全部档位池化）× 三判据（legacy / norm / point）分栏。
//! - 风格形态指标（客观口径）：长度分布 / 与 persona 参考的长度重合度 / 语气词率 /
//!   疑问感叹率 / 复读率 / 助手腔标记率，作为语气 judge 的交叉验证口径。
//! - 数据特性与外部效度局限声明必出（D-V20-005：单 persona、不做 D3 推广）。
//! - 输出 markdown / JSON 双形态；配对非参检验等纯函数逻辑独立，便于单元测试。

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ramaria_core::error::RamariaError;

use super::evaluate::{
    FactItemScore, ItemEvaluation, ProbeEvaluation, VariantEvaluation, read_experiment,
};
use super::run::metric_stat;
use super::types::{AblationProfile, ProbeExperiment, ProbeVariantResult, VariantParams};

// =========================================================
// probe report：档位对比报告 + 定稿建议 + 校准 + 知识层质量评估
// =========================================================

/// 情感维口径声明（描述性指标，报告必出字段）。
///
/// 说明:
/// - 当前口径为「显式共情/喜悦标记词命中数」的确定性 rubric。高亲密度口语语料的
///   persona 真实回复极短且不含书面情绪词，实测全档 0.02~0.25，该口径对 persona
///   短句风格系统性不利，构造效度尚未校准。
/// - 因此情感维降级为描述性指标：报告仍展示其数值供趋势参考，但不参与层价值判定
///   与参数定稿；层价值判定采用事实维 + 语气维双判据。
pub const EMOTION_DESCRIPTIVE_NOTE: &str = "情感维口径未校准：确定性 rubric 由「显式共情/喜悦标记词命中」驱动，\
高亲密度口语语料下对 persona 短句风格系统性不利（实测全档 0.02~0.25）。该维为描述性指标，\
仅作趋势参考，不参与层价值判定与参数定稿；层价值判定采用事实维 + 语气维双判据。";

// =========================================================
// 风格形态指标（客观口径，对照语气 judge）
// =========================================================

/// 语气词 / 口癖字符表：回复含任一字符即计该条命中。
///
/// 说明: 取高亲密度口语语料的高频句尾语气词与笑声拟声（榆：哦哦 / 我找一下 / 是联动！/ 对啊对啊）。
const STYLE_TONE_PARTICLES: &[char] = &[
    '呀', '啦', '哦', '诶', '啊', '嘛', '吧', '哈', '嗯', '咦', '哇', '唉', '噢', '咯', '嘞', '嘻',
    '嘿', '呐', '嗷',
];

/// 助手腔标记词：回复含任一词即计该条为助手腔。
///
/// 说明: 与 `test-data/m8/prompt-tone-report.md` 的形态分析口径一致，用于检测短模板是否回退到助手腔。
const STYLE_ASSISTANT_MARKERS: &[&str] = &[
    "总的来说",
    "综上",
    "希望对你有帮助",
    "希望这些",
    "建议你",
    "需要注意的是",
    "首先",
    "其次",
    "作为一个",
    "我可以帮你",
    "如果你需要",
    "总结一下",
];

/// 回复长度直方图分箱宽度（字）。
const STYLE_LEN_BIN: usize = 5;
/// 回复长度直方图封顶（字），超过按最后一箱计。
const STYLE_LEN_CAP: usize = 60;
/// "短回复"阈值（字）：新社交模板的目标区间为 20~30 字。
const STYLE_SHORT_LEN: usize = 30;

/// 档位回复的客观风格形态指标。
///
/// 说明:
/// - 用途：语气 judge（`probe-judge-v2`）在 20~30 字短回复上区分力不足
///   （见 `test-data/m8/tone-judge-review.md`），本组指标只依赖回复文本本身，
///   作为语气维的**客观对照口径**，与 judge 结论交叉验证（4.1 语气维改造）。
/// - `len_ref_overlap`：回复长度直方图与 persona 参考（tone 题 `reference`，即 persona
///   原回复）长度直方图的重叠系数，分箱宽 5 字、60 字封顶，取 `Σ min(p_i, q_i)`，
///   1.0 表示两个分布完全一致；无 tone 题参考时为 `None`。
/// - 各 `*_rate` 按"回复条数"计（非按字数）；`repeat_rate` 为同档位内与其他题回复
///   完全相同的条数占比（模板化复读检测）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantStyleMetrics {
    pub variant_id: String,
    pub description: String,
    /// 参与统计的有效回复条数（跨 repeat 全轮；无逐轮明细时取末轮）
    pub reply_count: usize,
    /// 回复字符数均值
    pub len_mean: f64,
    /// 回复字符数中位数
    pub len_median: f64,
    /// ≤30 字回复占比
    pub len_le_30_rate: f64,
    /// 与 persona 参考长度分布的重叠系数（无参考时为 None）
    pub len_ref_overlap: Option<f64>,
    /// 语气词 / 口癖命中率
    pub tone_particle_rate: f64,
    /// 以 ? / ？结尾的回复占比
    pub question_rate: f64,
    /// 以 ! / ！结尾的回复占比
    pub exclaim_rate: f64,
    /// 复读率（与同档位其它题回复完全相同的占比）
    pub repeat_rate: f64,
    /// 助手腔标记词命中率
    pub assistant_marker_rate: f64,
    /// persona 参考均长（无 tone 题参考时为 None）
    pub ref_len_mean: Option<f64>,
}

/// 收集某档位的全部有效回复：优先取 `repeat` 逐轮明细（跨 N 轮），缺失时回退末轮。
fn collect_variant_replies(experiment: &ProbeExperiment, variant_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(rs) = experiment
        .repeat
        .as_ref()
        .and_then(|rep| rep.per_variant.iter().find(|r| r.variant_id == variant_id))
    {
        for round in &rs.rounds {
            for it in &round.runs {
                if it.error.is_none() && !it.reply.is_empty() {
                    out.push(it.reply.clone());
                }
            }
        }
    }
    let fallback = experiment
        .variants
        .iter()
        .find(|v| v.variant_id == variant_id)
        .filter(|_| out.is_empty());
    if let Some(vr) = fallback {
        for it in &vr.runs {
            if it.error.is_none() && !it.reply.is_empty() {
                out.push(it.reply.clone());
            }
        }
    }
    out
}

/// persona 参考长度分布（取 tone 题 `reference`，即 persona 原回复；按 item_id 去重）。
fn persona_reference_lengths(evaluation: Option<&ProbeEvaluation>) -> Vec<usize> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    if let Some(ev) = evaluation {
        for v in &ev.variants {
            for it in &v.items {
                let ref_text = it.reference.as_deref().filter(|_| it.dimension == "tone");
                if let Some(r) = ref_text {
                    seen.entry(it.item_id.clone())
                        .or_insert_with(|| r.chars().count());
                }
            }
        }
    }
    let mut out: Vec<usize> = seen.into_values().collect();
    out.sort_unstable();
    out
}

/// 长度直方图（归一化概率）。
fn style_len_hist(lengths: &[usize]) -> std::collections::HashMap<usize, f64> {
    let mut hist: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for &l in lengths {
        *hist
            .entry((l / STYLE_LEN_BIN).min(STYLE_LEN_CAP / STYLE_LEN_BIN))
            .or_insert(0) += 1;
    }
    let total = lengths.len().max(1) as f64;
    hist.into_iter()
        .map(|(k, v)| (k, v as f64 / total))
        .collect()
}

/// 两个长度分布的重叠系数（`Σ min(p_i, q_i)`，1.0 = 完全一致）。
fn style_len_overlap(a: &[usize], b: &[usize]) -> f64 {
    let (ha, hb) = (style_len_hist(a), style_len_hist(b));
    ha.keys()
        .chain(hb.keys())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .map(|k| {
            ha.get(k)
                .copied()
                .unwrap_or(0.0)
                .min(hb.get(k).copied().unwrap_or(0.0))
        })
        .sum::<f64>()
        .clamp(0.0, 1.0)
}

/// 中位数（输入需已升序）。
fn median_of(sorted: &[usize]) -> f64 {
    match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2] as f64,
        n => (sorted[n / 2 - 1] + sorted[n / 2]) as f64 / 2.0,
    }
}

/// 计算全部档位的客观风格形态指标。
pub(super) fn compute_style_metrics(
    experiment: &ProbeExperiment,
    evaluation: Option<&ProbeEvaluation>,
) -> Vec<VariantStyleMetrics> {
    let ref_lens = persona_reference_lengths(evaluation);
    let ref_len_mean = if ref_lens.is_empty() {
        None
    } else {
        Some(ref_lens.iter().sum::<usize>() as f64 / ref_lens.len() as f64)
    };
    experiment
        .variants
        .iter()
        .map(|vr| {
            let replies = collect_variant_replies(experiment, &vr.variant_id);
            let lens: Vec<usize> = replies.iter().map(|r| r.chars().count()).collect();
            let n = lens.len();
            let div = n.max(1) as f64;
            let mut sorted = lens.clone();
            sorted.sort_unstable();
            let unique = replies
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len();
            let ends_with_any = |r: &String, marks: [char; 2]| {
                let t = r.trim_end();
                t.ends_with(marks[0]) || t.ends_with(marks[1])
            };
            VariantStyleMetrics {
                variant_id: vr.variant_id.clone(),
                description: vr.description.clone(),
                reply_count: n,
                len_mean: if n == 0 {
                    0.0
                } else {
                    lens.iter().sum::<usize>() as f64 / div
                },
                len_median: median_of(&sorted),
                len_le_30_rate: lens.iter().filter(|l| **l <= STYLE_SHORT_LEN).count() as f64 / div,
                len_ref_overlap: if ref_lens.is_empty() || lens.is_empty() {
                    None
                } else {
                    Some(style_len_overlap(&lens, &ref_lens))
                },
                tone_particle_rate: replies
                    .iter()
                    .filter(|r| r.chars().any(|c| STYLE_TONE_PARTICLES.contains(&c)))
                    .count() as f64
                    / div,
                question_rate: replies
                    .iter()
                    .filter(|r| ends_with_any(r, ['？', '?']))
                    .count() as f64
                    / div,
                exclaim_rate: replies
                    .iter()
                    .filter(|r| ends_with_any(r, ['！', '!']))
                    .count() as f64
                    / div,
                repeat_rate: if n == 0 {
                    0.0
                } else {
                    (n - unique) as f64 / div
                },
                assistant_marker_rate: replies
                    .iter()
                    .filter(|r| STYLE_ASSISTANT_MARKERS.iter().any(|m| r.contains(m)))
                    .count() as f64
                    / div,
                ref_len_mean,
            }
        })
        .collect()
}

/// 档位对比报告（`probe report` 的输出，markdown/JSON 双形态）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeReport {
    pub results_file: String,
    pub evaluation_file: Option<String>,
    pub persona_uid: String,
    pub dataset_seed: u64,
    pub judge_used: bool,
    pub embedding_used: bool,
    pub generated_at: String,
    /// 各档位评分汇总表
    pub variants: Vec<VariantReportRow>,
    /// 定稿建议（每维度的推荐档位 + 理由）
    pub recommendation: Recommendation,
    /// 人工抽检校准结果（未提供校准文件时为 None）
    pub calibration: Option<CalibrationResult>,
    /// 知识层抽取质量评估（基于 fact 题误报/漏报；可选）
    pub knowledge_quality: Option<KnowledgeQualityReport>,
    /// 消融对比报告（`probe report --ablation`；普通模式为 None）
    pub ablation: Option<AblationReport>,
    /// 数据特性与外部效度局限声明（D-V20-005：仅单 persona 高信号数据，
    /// 不做 D3 跨 persona 推广；judge/embedding 可用性等评估限制）。
    pub limitations: Vec<String>,
    /// 描述性指标（不参与层价值判定）的口径声明（必出）。
    pub descriptive_metrics: Vec<String>,
    /// 辅助指标四件套（D-V20-006：证据链可追溯率 / 行为规则命中率 /
    /// 情境路由误用率 / 画像回归）。基于 run/eval 产物可复算的近似口径，
    /// 语义与局限见 `AuxiliaryMetrics.annotation`。
    pub auxiliary: AuxiliaryMetrics,
    /// 客观风格形态指标（对照语气 judge；无回复样本时为空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub style_metrics: Vec<VariantStyleMetrics>,
}

// =========================================================
// 辅助指标四件套（M2-005，产物可复算近似）
// =========================================================

/// 辅助指标四件套。
///
/// 口径说明（技术报告 §16.4 定义的探针可复算近似，J 时未产出）:
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
pub(super) fn compute_auxiliary_metrics(evaluation: &ProbeEvaluation) -> AuxiliaryMetrics {
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
        "辅助指标为探针产物可复算近似：证据链可追溯率=fact 回复对 golden 覆盖率 \
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

/// 档位报告行（评分对比表）。
///
/// 字段约定:
/// - `fact_score`: 事实维旧口径（2-gram 覆盖率）均分，冻结不变以支持历史口径对照。
/// - `fact_score_norm` / `fact_score_point`: 事实维两个重算口径（长度归一 / 事实点），
///   与 `fact_score` 并排展示，便于在新旧判据下核对；旧产物或未评分时为 None。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantReportRow {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    pub fact_score: Option<f64>,
    /// 事实维长度归一均分（0.0~1.0；旧产物或未评分时 None）
    pub fact_score_norm: Option<f64>,
    /// 事实维事实点均分（0.0~1.0；旧产物或未评分时 None）
    pub fact_score_point: Option<f64>,
    pub tone_score: Option<f64>,
    /// 情感表达维均分（0.0~1.0；无 emotion 题时为 None）
    pub emotion_score: Option<f64>,
    pub success_count: usize,
    pub total_count: usize,
    pub failed_count: usize,
}

/// 定稿建议。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Recommendation {
    /// 每维度的最佳档位 id 与理由
    pub per_dimension: Vec<DimensionRecommendation>,
    /// 综合建议（兼顾各维的平衡档位）
    pub overall: String,
}

/// 单维度定稿建议。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DimensionRecommendation {
    pub dimension: String,
    pub best_variant: Option<String>,
    pub best_score: Option<f64>,
    pub reason: String,
}

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

/// 知识层质量评估的单一统计口径（含样本范围与档位集合）。
///
/// 说明:
/// - 同一份评分数值可按不同档位集合切分统计，口径差异必须显式标注，否则
///   无记忆基线档位会把漏报率抬高、使指标不可比。
#[derive(Debug, Clone, serde::Serialize)]
pub struct KnowledgeQualityScope {
    /// 口径标识："memory_injected"（含记忆注入）/ "pooled_all"（全部档位池化）
    pub scope: String,
    /// 人类可读口径说明（含档位集合与样本数）
    pub description: String,
    /// 该口径覆盖的档位 id 列表
    pub variant_ids: Vec<String>,
    /// 样本数（事实维题数）
    pub sample_count: usize,
    /// 事实命中数（旧判据 `score ≥ 0.5`）
    pub fact_hit_count: usize,
    /// 误报率（旧判据 `score < 0.3`）
    pub false_positive_rate: f64,
    /// 漏报率（旧判据 `score < 0.4`）
    pub false_negative_rate: f64,
    /// 是否达漏报 <10% 目标（旧判据）
    pub miss_target_met: bool,
    /// 按判据口径（legacy / norm / point）分别汇总的质量指标。
    ///
    /// 说明:
    /// - `0` 号元素恒为 `legacy`，其三个率与上面的扁平字段一一对应（扁平字段保留
    ///   以兼容既有 JSON 消费方）。
    /// - 旧评分数值文件无 `score_norm` / `score_point` → 对应判据样本数为 0，
    ///   达标标记为 false。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge_rates: Vec<KnowledgeJudgeRates>,
}

/// 单一口径下、按判据口径分别汇总的知识层质量指标。
///
/// 说明:
/// - 三套判据共用同一题集：`legacy`（旧 2-gram 覆盖，冻结）/ `norm`（长度归一
///   命中率）/ `point`（子句级事实点召回）。
/// - 阈值沿用旧口径（命中 ≥0.5 / 误报 <0.3 / 漏报 <0.4），使三口径在同一阈值下
///   可比；口径差异只来自综合分的"关键词项"，cosine 权重三口径相同。
#[derive(Debug, Clone, serde::Serialize)]
pub struct KnowledgeJudgeRates {
    /// 判据口径标识："legacy" / "norm" / "point"
    pub judge: String,
    /// 该口径下可用的样本数（字段缺失的旧产物为 0）
    pub sample_count: usize,
    /// 事实命中率（综合分 ≥0.5）
    pub hit_rate: f64,
    /// 误报率（综合分 <0.3）
    pub false_positive_rate: f64,
    /// 漏报率（综合分 <0.4）
    pub false_negative_rate: f64,
    /// 是否达漏报 <10% 目标
    pub miss_target_met: bool,
}

/// 知识层抽取质量评估报告。
///
/// 说明:
/// - 基于事实维探针题评估知识层抽取质量：以「回复是否涵盖事件事实」判定命中/漏报。
/// - `false_positive_rate`（误报）：回复未涵盖应有事实（score < 0.3）。
/// - `false_negative_rate`（漏报）：回复信息不足（score < 0.4），目标 <10%。
/// - 双口径：主口径只统计含记忆注入档位（M8-006 终验口径）；对照口径池化全部档位
///   （含无记忆基线），用于说明两者差异来源。
/// - 判据分栏：每个口径内再按 `judge_rates`（legacy / norm / point）分别给出命中/漏报，
///   使短回复模板下的长度伪影（旧判据漏报虚高）可被直接对照。
#[derive(Debug, Clone, serde::Serialize)]
pub struct KnowledgeQualityReport {
    /// 主口径：含记忆注入档位（M8-006 终验口径）
    pub primary: KnowledgeQualityScope,
    /// 对照口径：全部档位池化（含无记忆基线，供可比性对照）
    pub pooled: KnowledgeQualityScope,
    /// 口径说明（必出）
    pub annotation: String,
}

// =========================================================
// 消融对比报告（M5a T-004：配对 Wilcoxon + Cohen's d + CI + FDR）
// =========================================================

/// 消融对比报告（`probe report --ablation`）。
///
/// 结构:
/// - `baseline_variant`: 主基线档位 id（优先 F0；仅含 S/I 组时为 B1）。
/// - `rows`: 消融 vs 基线的逐"消融档位 × 维度"统计判定行，
///   每行自带 `comparison_type`（removal / substitution / increment）与 `base_variant`，
///   供报告把三类对照分栏表述。
/// - `aux`: 参与对比各档位的辅助指标（回复长度/耗时/空回复率）。
///
/// 对照语义（D-V20-006，口径见 `docs/dev-2.0/ablation-profile-mapping.md`）:
/// - removal（F 组，基线 F0）: 全开中逐层关闭 → 回答"去掉某一层的边际损失"；
/// - substitution（S 组，基线 B1）: 去 RAG 摘要、仅单专属层 → 回答"单层能否替代 RAG"；
/// - increment（I 组，基线 B1）: B1 基座 + 单专属层 → 回答"在 RAG 之上叠加一层的净增量"。
///
/// 判定线（D-V17-009）: `p_fdr < 0.05 ∧ |cohens_d| ≥ 0.3 ∧ CI 不含 0` → 显著；
/// 贡献方向见 `AblationComparisonRow.direction`。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationReport {
    /// 主基线档位 id（F0 或 B1）
    pub baseline_variant: String,
    /// 逐消融档位 × 维度统计判定
    pub rows: Vec<AblationComparisonRow>,
    /// 参与对比档位的辅助指标（mean ± CI / 空回复率）
    pub aux: Vec<VariantAuxMetrics>,
    /// 参与层价值判定的维度（事实维三口径 fact / fact_norm / fact_point + 语气维 tone）。
    pub judgment_dimensions: Vec<String>,
    /// 展示但不参与判定的描述性维度（情感维，口径未校准）。
    pub descriptive_dimensions: Vec<String>,
    /// 维度范围说明（必出）。
    pub dimension_scope_note: String,
    /// 等效性检验口径说明（必出）：TOST + 等效边界语义。
    pub equivalence_note: String,
}

/// 单条消融对比（某消融档位 × 某维度，按题目配对）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AblationComparisonRow {
    /// 消融档位 id（如 F1 / S_behavior / I_behavior）
    pub ablation_variant: String,
    /// 消融档位描述
    pub description: String,
    /// 对照类型：removal（F 组逐层移除 vs F0）/ substitution（S 组替代，去 RAG 摘要 vs B1）/
    /// increment（I 组净增量，B1 基座 + 单专属层 vs B1）。
    pub comparison_type: String,
    /// 实际对照基线档位 id（F 组为 F0；S/I 组为 B1）。
    pub base_variant: String,
    /// 维度（fact / fact_norm / fact_point / tone；emotion 为描述性维度，仅在报告中展示）
    pub dimension: String,
    /// 配对题数
    pub n_pairs: usize,
    /// 基线均值（F0 或 B1）
    pub base_mean: f64,
    /// 消融后均值
    pub ablated_mean: f64,
    /// 均值差（消融 − 基线）
    pub mean_diff: f64,
    /// 配对 Wilcoxon 符号秩检验 p 值（双尾，正态近似）
    pub wilcoxon_p: f64,
    /// FDR 校正后 p 值（Benjamini–Hochberg）
    pub p_fdr: f64,
    /// Cohen's d（配对 d_z = mean(diff)/sd(diff)；sd=0 时 ±10 标记远超阈值）
    pub cohens_d: f64,
    /// 合并标准差标准化的 Cohen's d（d_av = mean(diff) / sd_av；sd_av = 两档位配对样本合并 SD）
    pub cohens_d_pooled: f64,
    /// TOST 等效边界（原始差分量纲；= 0.3 × sd_av，对应 |d_av| = 0.3）
    pub equiv_bound: f64,
    /// TOST 等效性检验 p 值（双单侧，t 分布 df = n_pairs − 1）
    pub tost_p: f64,
    /// 是否可判定等效（tost_p < 0.05）
    pub equivalent: bool,
    /// 综合判定：significant_up / significant_down / equivalent / inconclusive
    pub verdict: String,
    /// 均值差 95% 置信区间（t 分布）
    pub ci95_low: f64,
    /// 均值差 95% 置信区间上界
    pub ci95_high: f64,
    /// 是否显著（p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0）
    pub significant: bool,
    /// 该层贡献方向结论（up = 消融后提升 / down = 消融后下降 / none）
    pub direction: String,
    /// 人类可读结论
    pub annotation: String,
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

// =========================================================
// 执行 `probe report`
// =========================================================

/// 执行 `probe report`。
///
/// 流程:
/// 1. 读取实验结果（probe run 产物）。
/// 2. 读取评分数值（probe evaluate 产物；缺失则仅汇总 run 指标；
///    `--ablation` 模式必须提供评分数值，否则业务校验失败）。
/// 3. 生成档位对比表 + 定稿建议（每维最佳档位）。
/// 4. 若提供校准文件 → 计算 judge/人工一致性、偏差、校准系数。
/// 5. 若提供评分数值 → 基于 fact 题评估知识层误报/漏报。
/// 6. `--ablation` 模式 → 自动识别 F0/B1 基线生成消融对比统计。
/// 7. 输出 markdown / JSON 双形态。
pub(super) async fn run_report(
    _app: &Arc<ramaria_app::App>,
    results_path: &Path,
    evaluation_path: Option<&Path>,
    calibration_path: Option<&Path>,
    output: Option<&str>,
    ablation: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Step 1: 读取实验结果
    let experiment = read_experiment(results_path)?;

    // --ablation 模式前置校验：需要评分数值文件（含逐题明细）。
    if ablation && evaluation_path.is_none() {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "消融对比报告（--ablation）需要评分数值文件：请先运行 `ramaria probe evaluate --results <run> --dataset <ds> --output <eval>`"
        )));
    }

    // Step 2: 读取评分数值（可选）
    let evaluation: Option<ProbeEvaluation> = match evaluation_path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| {
                anyhow::anyhow!(RamariaError::validation(format!(
                    "读取评分数值失败: {}（请先运行 `ramaria probe evaluate` 生成）: {e}",
                    p.display()
                )))
            })?;
            match serde_json::from_str(&text) {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!(error = %e, "评分数值解析失败，报告仅含运行指标");
                    None
                }
            }
        }
        None => None,
    };

    // Step 3: 档位对比表
    let mut rows = Vec::with_capacity(experiment.variants.len());
    for vr in &experiment.variants {
        let ev = evaluation
            .as_ref()
            .and_then(|e| e.variants.iter().find(|v| v.variant_id == vr.variant_id));
        let success = vr.runs.len().saturating_sub(vr.failed_count);
        rows.push(VariantReportRow {
            variant_id: vr.variant_id.clone(),
            description: vr.description.clone(),
            params: vr.params.clone(),
            fact_score: ev.and_then(|v| v.fact_score),
            fact_score_norm: ev.and_then(|v| v.fact_score_norm),
            fact_score_point: ev.and_then(|v| v.fact_score_point),
            tone_score: ev.and_then(|v| v.tone_score),
            emotion_score: ev.and_then(|v| v.emotion_score),
            success_count: success,
            total_count: vr.runs.len(),
            failed_count: vr.failed_count,
        });
    }

    // Step 4: 定稿建议（基于评分，无评分时基于运行指标）
    let recommendation = build_recommendation(&rows);

    // Step 5: 人工抽检校准（可选）
    let calibration = match calibration_path {
        Some(p) => {
            let manual = read_manual_scores(p)?;
            Some(compute_calibration(&manual, evaluation.as_ref()))
        }
        None => None,
    };

    // Step 6: 知识层质量评估（基于评分数值 fact 题）
    let knowledge_quality = evaluation.as_ref().map(assess_knowledge_quality);

    // Step 6.5: 消融对比报告（--ablation 模式）
    // 评分数值解析失败时评估为 None → 消融段缺省（记 warn 已在上游输出）。
    let ablation_report = if ablation {
        evaluation
            .as_ref()
            .map(|eval| build_ablation_report(&experiment, eval))
    } else {
        None
    };

    // 数据特性与外部效度局限声明（D-V20-005，报告必出字段）：
    // 消融结论基于单 persona 高信号数据，不做 D3 跨 persona 推广。
    let judge_used = evaluation.as_ref().map(|e| e.judge_used).unwrap_or(false);
    let embedding_used = evaluation
        .as_ref()
        .map(|e| e.embedding_used)
        .unwrap_or(false);
    let limitations = build_limitations(&experiment, judge_used, embedding_used);
    // 描述性指标口径声明（必出）：情感维未校准，仅作展示、不参与层价值判定。
    let descriptive_metrics = vec![EMOTION_DESCRIPTIVE_NOTE.to_string()];

    // 辅助指标四件套（D-V20-006）：有评分数值时可复算；缺失时给出空指标 + 说明。
    let auxiliary = match evaluation.as_ref() {
        Some(ev) => compute_auxiliary_metrics(ev),
        None => AuxiliaryMetrics {
            evidence_traceability_rate: None,
            behavior_rule_hit_rate: None,
            situation_route_misuse_rate: None,
            profile_regression_output_stability: None,
            annotation: "未提供评分数值文件（--evaluation），辅助指标不可计算".to_string(),
        },
    };

    // 客观风格形态指标：仅依赖 run 产物（回复文本）+ eval 产物（persona 参考长度），
    // 用于对照区分力不足的短回复语气 judge。
    let style_metrics = compute_style_metrics(&experiment, evaluation.as_ref());

    let report = ProbeReport {
        results_file: results_path.display().to_string(),
        evaluation_file: evaluation_path.map(|p| p.display().to_string()),
        persona_uid: experiment.persona_uid.clone(),
        dataset_seed: experiment.dataset_seed,
        judge_used,
        embedding_used,
        generated_at: super::now_iso8601(),
        variants: rows,
        recommendation,
        calibration,
        knowledge_quality,
        ablation: ablation_report,
        limitations,
        descriptive_metrics,
        style_metrics,
        auxiliary,
    };

    // Step 7: 输出
    if let Some(out) = output {
        // 按扩展名判断输出形态：.json → JSON；.md → markdown；其他按 --json 决定
        let is_json_file = out.ends_with(".json");
        if is_json_file || (json && !out.ends_with(".md")) {
            write_report_json(out, &report)?;
        } else {
            write_report_markdown(out, &report)?;
        }
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": report.persona_uid,
                "variants": report.variants.len(),
                "calibration": report.calibration.is_some(),
                "knowledge_quality": report.knowledge_quality.is_some(),
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "探针报告已写入 {}（{} 档位对比，{}）",
            out,
            report.variants.len(),
            if report.ablation.is_some() {
                "含消融对比统计"
            } else if report.calibration.is_some() {
                "含人工抽检校准"
            } else {
                "未校准"
            }
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&report);
    }

    print_report_summary(&report);
    Ok(())
}

/// 构建数据特性与外部效度局限声明（D-V20-005，报告必出字段）。
///
/// 内容（与任务验收口径一致）:
/// - 仅单 persona 高信号数据 → 只作 D2 高信号效度，不做 D3 跨 persona 推广；
/// - 语气维 judge / 事实维 embedding 可用性影响维度覆盖；
/// - 采样规模（repeat 次数）决定统计法置信度。
pub(super) fn build_limitations(
    experiment: &ProbeExperiment,
    judge_used: bool,
    embedding_used: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    // 外部效度边界：仅一份单人对单人记录，显式声明不推广跨 persona。
    out.push(format!(
        "外部效度局限：评估基于单 persona（{}）高信号数据，仅作 D2 高信号效度，\
         不做 D3 跨 persona 普遍性推广；结论不得外推为产品级普遍主张",
        experiment.persona_uid
    ));
    if !judge_used {
        out.push(
            "语气维缺失：本地 judge 不可用或未提供（tone 分空缺），语气维结论需人工抽检补足"
                .to_string(),
        );
    }
    if !embedding_used {
        out.push("事实维降级：embedding 不可用，事实维退化为关键词命中（无语义余弦）".to_string());
    }
    if let Some(rep) = &experiment.repeat {
        out.push(format!(
            "统计法样本：repeat=N={}，逐轮评分聚合 n 以实际有效轮数为准",
            rep.count
        ));
    } else {
        out.push("统计法样本：本次为单次运行（无 --repeat），结论未做多次配对统计".to_string());
    }
    out
}

/// 构建定稿建议（每维最佳档位 + 综合建议）。
fn build_recommendation(rows: &[VariantReportRow]) -> Recommendation {
    let mut per_dimension = Vec::new();

    // 事实维：取 fact_score 最高档位
    let fact_best = rows
        .iter()
        .filter(|r| r.fact_score.is_some())
        .max_by(|a, b| {
            a.fact_score
                .partial_cmp(&b.fact_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "fact".to_string(),
        best_variant: fact_best.map(|r| r.variant_id.clone()),
        best_score: fact_best.and_then(|r| r.fact_score),
        reason: match fact_best {
            Some(r) => format!(
                "事实维最高分 {:.2}（档位 {}）；综合 embedding 余弦与关键词命中",
                r.fact_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => {
                "无有效事实维评分（embedding 不可用或全部失败），无法给出事实维建议".to_string()
            }
        },
    });

    // 语气维：取 tone_score 最高档位
    let tone_best = rows
        .iter()
        .filter(|r| r.tone_score.is_some())
        .max_by(|a, b| {
            a.tone_score
                .partial_cmp(&b.tone_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    per_dimension.push(DimensionRecommendation {
        dimension: "tone".to_string(),
        best_variant: tone_best.map(|r| r.variant_id.clone()),
        best_score: tone_best.and_then(|r| r.tone_score),
        reason: match tone_best {
            Some(r) => format!(
                "语气维最高分 {:.2}（档位 {}）；judge rubric 1~5 评分",
                r.tone_score.unwrap_or(0.0),
                r.variant_id
            ),
            None => "语气维 judge 不可用或已跳过，无法给出语气维建议".to_string(),
        },
    });

    // 情感维口径未校准（描述性指标）：不给最佳档位建议，仅声明口径。
    per_dimension.push(DimensionRecommendation {
        dimension: "emotion".to_string(),
        best_variant: None,
        best_score: None,
        reason: EMOTION_DESCRIPTIVE_NOTE.to_string(),
    });

    // 综合建议：以层价值判定维度（事实维 + 语气维）为准，两者最佳档位一致 → 取该档位；
    // 否则提示需人工权衡。情感维为描述性指标，不参与一致性判定。
    let all_same = |best: Option<&VariantReportRow>, id: &str| {
        best.map(|r| r.variant_id == id).unwrap_or(false)
    };
    let overall = match fact_best {
        Some(f) if all_same(tone_best, &f.variant_id) => {
            format!(
                "综合建议档位 {}（事实/语气均最优）；需人工抽检校准后定稿",
                f.variant_id
            )
        }
        _ => "各维最佳档位不一致，需结合人工抽检与消融实验权衡取舍".to_string(),
    };

    Recommendation {
        per_dimension,
        overall,
    }
}

// =========================================================
// 消融对比报告实现（M5a T-004）
// =========================================================

/// 供消融配对的逐维度"item_id → 分数"索引。
type VariantDimScores = std::collections::HashMap<String, f64>;

/// 从评分数值档位提取某维度的逐题分数（仅成功题）。
///
/// 说明:
/// - tone 维 judge 分 1~5 直接作连续分使用；fact/emotion 维 0~1。
/// - `fact_norm` / `fact_point` 为事实维的重算口径，取自同一 fact 子评分的
///   `score_norm` / `score_point`；旧产物缺该字段的题不参与配对。
fn collect_variant_dim_scores(ev: &VariantEvaluation, dim: &str) -> VariantDimScores {
    let mut map = VariantDimScores::new();
    for item in &ev.items {
        if item.error.is_some() {
            continue;
        }
        let score = match dim {
            "fact" => item.fact.as_ref().map(|s| s.score),
            "fact_norm" => item.fact.as_ref().and_then(|s| s.score_norm),
            "fact_point" => item.fact.as_ref().and_then(|s| s.score_point),
            "tone" => item.tone.as_ref().map(|s| s.score as f64),
            "emotion" => item.emotion.as_ref().map(|s| s.score),
            _ => None,
        };
        if let Some(s) = score {
            map.insert(item.item_id.clone(), s);
        }
    }
    map
}

/// 按题目配对两个档位在某维度的差分样本与配对分数。
///
/// 配对规则: 仅取两端都成功评分的 item_id（同一题目），
/// `diffs = ablated − base`；两端任一缺失的题不参与配对。
/// 返回 (diffs, base_mean, ablated_mean, base_scores, ablated_scores)。
fn pair_dimension_diffs(
    ablated: &VariantDimScores,
    base: &VariantDimScores,
) -> (Vec<f64>, f64, f64, Vec<f64>, Vec<f64>) {
    let mut diffs = Vec::new();
    let mut base_scores = Vec::new();
    let mut ablated_scores = Vec::new();
    for (item_id, base_score) in base {
        if let Some(ablated_score) = ablated.get(item_id) {
            diffs.push(ablated_score - base_score);
            base_scores.push(*base_score);
            ablated_scores.push(*ablated_score);
        }
    }
    let n = diffs.len() as f64;
    if n == 0.0 {
        return (diffs, 0.0, 0.0, base_scores, ablated_scores);
    }
    let base_mean = base_scores.iter().sum::<f64>() / n;
    let ablated_mean = ablated_scores.iter().sum::<f64>() / n;
    (diffs, base_mean, ablated_mean, base_scores, ablated_scores)
}

// =========================================================
// 等效性检验（TOST）：证明"零净增量"
// =========================================================

/// 合并标准差标准化的 Cohen's d（d_av）。
///
/// 说明:
/// - `sd_av = sqrt((var_base + var_ablated) / 2)`（样本方差，n≥2）。
/// - 该口径反映"两个条件各自的离散度"，是等效边界的自然标尺；
///   显著性判定仍用配对 d_z（`cohens_d_paired`），两者语义不同、不可互替。
/// - 样本不足（n<2）或合并 SD 为 0 时返回 0.0。
pub(super) fn cohens_d_pooled(base: &[f64], ablated: &[f64]) -> f64 {
    let n = base.len().min(ablated.len());
    if n < 2 {
        return 0.0;
    }
    let mean = |xs: &[f64]| xs.iter().take(n).sum::<f64>() / n as f64;
    let var = |xs: &[f64]| {
        let m = mean(xs);
        xs.iter().take(n).map(|x| (x - m) * (x - m)).sum::<f64>() / (n as f64 - 1.0)
    };
    let sd_av = ((var(base) + var(ablated)) / 2.0).sqrt();
    if sd_av < 1e-12 {
        return 0.0;
    }
    let diff_mean = ablated
        .iter()
        .take(n)
        .zip(base.iter().take(n))
        .map(|(a, b)| a - b)
        .sum::<f64>()
        / n as f64;
    diff_mean / sd_av
}

/// TOST 等效性检验结果。
#[derive(Debug, Clone, Copy)]
pub(super) struct TostOutcome {
    /// 原始差分量纲的等效边界（= bound_d × sd_av）
    pub bound: f64,
    /// TOST p 值（max(两个单侧 p)）
    pub p: f64,
    /// 是否可判定等效（p < 0.05）
    pub equivalent: bool,
}

/// 配对样本的 TOST 等效性检验（t 分布，df = n − 1）。
///
/// 参数:
/// - `diffs`: 配对差分样本（ablated − base）。
/// - `base` / `ablated`: 配对分数向量（用于合并 SD 计算等效边界）。
/// - `bound_d`: 标准化等效边界。
///
/// 返回:
/// - n<2 或 SD 非法 → None（样本不足，调用方按不可判定处理）。
pub(super) fn tost_equivalence(
    diffs: &[f64],
    base: &[f64],
    ablated: &[f64],
    bound_d: f64,
) -> Option<TostOutcome> {
    let n = diffs.len();
    if n < 2 {
        return None;
    }
    let n_f = n as f64;
    let mean = diffs.iter().sum::<f64>() / n_f;
    let var = diffs.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / (n_f - 1.0);
    let sd = var.sqrt();
    if sd < 1e-12 {
        // 差分为常数：无抽样波动，无法做 t 检验
        return None;
    }
    // 合并 SD（两条件离散度）作为标准化标尺；只取前 n 项，
    // 保证与 `diffs` 一一配对的样本对齐（正常调用路径三者等长）。
    let var_of = |xs: &[f64]| {
        let m = xs.iter().take(n).sum::<f64>() / n_f;
        xs.iter().take(n).map(|x| (x - m) * (x - m)).sum::<f64>() / (n_f - 1.0)
    };
    let sd_av = ((var_of(base) + var_of(ablated)) / 2.0).sqrt();
    if sd_av < 1e-12 {
        return None;
    }
    let bound = bound_d * sd_av;
    let se = sd / n_f.sqrt();
    let df = n_f - 1.0;
    // H0_low: mean <= -bound（上侧检验）；H0_high: mean >= +bound（下侧检验）
    let t_low = (mean + bound) / se;
    let t_high = (mean - bound) / se;
    let p_low = 1.0 - student_t_cdf(t_low, df);
    let p_high = student_t_cdf(t_high, df);
    let p = p_low.max(p_high).clamp(0.0, 1.0);
    Some(TostOutcome {
        bound,
        p,
        equivalent: p < 0.05,
    })
}

/// 学生氏 t 分布累积分布函数。
///
/// 说明:
/// - 用正则化不完全贝塔函数计算（含 Lanczos ln Γ 与连分数），精度足以支撑
///   p 值判定（相对误差 <1e-6 量级）。
/// - df <= 0 或 t 为 NaN → 返回 0.5（退化，不 panic）。
pub(super) fn student_t_cdf(t: f64, df: f64) -> f64 {
    if !t.is_finite() || df <= 0.0 {
        return 0.5;
    }
    let x = df / (df + t * t);
    let ib = betai(df / 2.0, 0.5, x);
    if t > 0.0 { 1.0 - 0.5 * ib } else { 0.5 * ib }
}

/// 正则化不完全贝塔函数 I_x(a,b)（Numerical Recipes 6.4）。
fn betai(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_bt = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    let bt = ln_bt.exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * betacf(a, b, x) / a
    } else {
        1.0 - bt * betacf(b, a, 1.0 - x) / b
    }
}

/// 不完全贝塔连分数（Numerical Recipes 6.4 betacf）。
fn betacf(a: f64, b: f64, x: f64) -> f64 {
    const MAX_ITER: usize = 200;
    const EPS: f64 = 3.0e-12;
    const FPMIN: f64 = 1.0e-300;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=MAX_ITER {
        let m_f = m as f64;
        let m2 = 2.0 * m_f;
        let aa = m_f * (b - m_f) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m_f) * (qab + m_f) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// ln Γ(x)（Lanczos 近似，Numerical Recipes 6.1）。
fn ln_gamma(x: f64) -> f64 {
    const COF: [f64; 6] = [
        76.18009172947146,
        -86.50532032941677,
        24.01409824083091,
        -1.231739572450155,
        0.1208650973866179e-2,
        -0.5395239384953e-5,
    ];
    let mut y = x;
    let mut tmp = x + 5.5;
    tmp -= (x + 0.5) * tmp.ln();
    let mut ser = 1.000000000190015;
    for c in COF {
        y += 1.0;
        ser += c / y;
    }
    -tmp + (2.5066282746310005 * ser / x).ln()
}

/// 未达显著差异时的结论文案：区分"已证等效"与"样本量不足以判定"。
///
/// 说明:
/// - 显著与等效互斥，本函数只在 `significant == false` 时被调用；
/// - 等效分支报出等效边界（原始差分量纲）与 |d_av|，便于人工核对边界是否合理；
/// - 不可判定分支同时报 p_fdr 与 tost_p，指向"提高重复次数/题量"的下一步。
fn equivalence_annotation(
    type_label: &str,
    p_fdr: f64,
    tost_p: f64,
    equiv_bound: f64,
    cohens_d_pooled: f64,
    cohens_d: f64,
    equivalent: bool,
) -> String {
    if equivalent {
        format!(
            "{type_label}：等效（TOST p={tost_p:.3} < 0.05，等效边界 Δ=±{equiv_bound:.4}，\
             |d_av|={:.2}）→ 可判定该对照无实质净增量",
            cohens_d_pooled.abs()
        )
    } else {
        format!(
            "{type_label}：既未达显著差异、亦未达统计等效（p_fdr={p_fdr:.3}, tost_p={tost_p:.3}, \
             |d|={cohens_d:.2}）→ 样本量不足以判定"
        )
    }
}

/// 构建消融对比报告。
///
/// 基线识别:
/// - F 组: F0（完整体系）为基线，F1~F4 为逐层消融；
/// - S 组: B1（压缩摘要基座）为基线，S_behavior/S_knowledge/S_expression/
///   S_narrative 为单层注入。
///
/// 统计（按题目配对）:
/// - 配对 Wilcoxon 符号秩检验（双尾，正态近似）；
/// - Cohen's d（配对 d_z）；
/// - 均值差 95% CI（t 分布，复用 `metric_stat`）；
/// - 全部行 p 值经 Benjamini–Hochberg FDR 校正。
///
/// 判定线（D-V17-009）: `p_fdr < 0.05 ∧ |d| ≥ 0.3 ∧ CI 不含 0` → 显著。
///
/// 等效性检验: 显著性框架只能证明"存在差异"，无法证明"零净增量"（相关对照长期
/// 只得到不显著）。故并行做 TOST（双单侧 t 检验），等效边界取 |d_av|=0.3
/// （d_av 为合并 SD 口径），`tost_p < 0.05` 即判定"等效（无实质净增量）"。
///
/// 判定维度: 情感维口径未校准，不参与判定（仅报告展示数值）；事实维按三套判据分别
/// 成行——`fact`（旧 2-gram 覆盖口径，计算逻辑未变）、`fact_norm`（长度归一口径）、
/// `fact_point`（子句级事实点口径），另加语气维 `tone`。三套事实口径共用同一 FDR
/// 校正池，故 `fact` 的 `p_fdr` 与只跑两维时的原报告可能略有差异，属预期。
pub(super) fn build_ablation_report(
    experiment: &ProbeExperiment,
    evaluation: &ProbeEvaluation,
) -> AblationReport {
    // 索引评分数值档位（id → evaluation）
    let by_id: std::collections::HashMap<&str, &VariantEvaluation> = evaluation
        .variants
        .iter()
        .map(|v| (v.variant_id.as_str(), v))
        .collect();

    // 基线识别（F0 / B1）
    let find_baseline = |names: &[&str]| -> Option<&VariantEvaluation> {
        names
            .iter()
            .find_map(|n| by_id.get(*n).copied())
            .or_else(|| {
                // 兼容：id 非 F0/B1 但 params.ablation 标注了基线名的档位
                evaluation.variants.iter().find(|v| {
                    v.params
                        .ablation
                        .as_deref()
                        .map(|a| names.contains(&a))
                        .unwrap_or(false)
                })
            })
    };
    let f0 = find_baseline(&["F0"]);
    let b1 = find_baseline(&["B1"]);

    // 待比较组：F 组（F1~F4 vs F0）与 S 组（S_* vs B1），按数据集实际出现的档位驱动。
    // 判定维度取事实维三口径 + 语气维：情感维口径未校准，已移出层价值判定，仅在报告中展示数值。
    //
    // 事实维同时跑三套判据口径，用于在新旧判据下分栏核对：
    // - `fact` = 旧 2-gram 覆盖口径，计算逻辑未变；只是把它与两个重算口径一并纳入
    //   FDR 校正池，故其 `p_fdr` 与原（两维）报告可能略有差异，属预期；
    // - `fact_norm` = 长度归一口径（分母不随回复长度单调衰减）；
    // - `fact_point` = 子句级事实点口径（回复覆盖参考事实点的比例）。
    let dims = ["fact", "fact_norm", "fact_point", "tone"];

    // 先收集全部"候选行"（含未校正 p 值），再统一 FDR 校正后补判定字段。
    struct RawRow<'a> {
        ablation: &'a VariantEvaluation,
        dimension: &'a str,
        comparison_type: &'a str,
        base_variant: &'a str,
        diffs: Vec<f64>,
        base_scores: Vec<f64>,
        ablated_scores: Vec<f64>,
        base_mean: f64,
        ablated_mean: f64,
        wilcoxon_p: f64,
        cohens_d: f64,
        ci_low: f64,
        ci_high: f64,
    }

    let mut raw_rows: Vec<RawRow> = Vec::new();
    let mut compared_ids: Vec<String> = Vec::new();

    // F 组（removal）：F1~F4 逐层关闭 vs F0——回答"去掉某一层的边际损失"。
    if let Some(base) = f0 {
        for name in ["F1", "F2", "F3", "F4"] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean, base_scores, ablated_scores) =
                        pair_dimension_diffs(
                            &collect_variant_dim_scores(ablated, dim),
                            &collect_variant_dim_scores(base, dim),
                        );
                    if diffs.len() < 2 {
                        tracing::debug!(
                            ablation = name,
                            dimension = dim,
                            pairs = diffs.len(),
                            "消融对比配对样本不足，跳过该行"
                        );
                        continue;
                    }
                    raw_rows.push(RawRow {
                        ablation: ablated,
                        dimension: dim,
                        comparison_type: "removal",
                        base_variant: base.variant_id.as_str(),
                        base_scores,
                        ablated_scores,
                        base_mean,
                        ablated_mean,
                        wilcoxon_p: wilcoxon_signed_rank_p(&diffs).unwrap_or(1.0),
                        cohens_d: cohens_d_paired(&diffs),
                        ci_low: metric_stat(&diffs).ci_low,
                        ci_high: metric_stat(&diffs).ci_high,
                        diffs,
                    });
                }
            }
        }
    } else {
        tracing::warn!("消融对比报告：未找到 F0 基线档位，F 组（F1~F4）无法对比");
    }

    // S 组（substitution）与 I 组（increment）均对照 B1，但对照口径不同：
    // - S_*（替代）＝去 RAG 摘要、仅单专属层——回答"单层能否替代 RAG"；
    // - I_*（净增量）＝B1 基座 + 单专属层——回答"在 RAG 之上叠加一层的净增量"。
    if let Some(base) = b1 {
        for (name, comparison_type) in [
            ("S_behavior", "substitution"),
            ("S_knowledge", "substitution"),
            ("S_expression", "substitution"),
            ("S_narrative", "substitution"),
            ("I_behavior", "increment"),
            ("I_knowledge", "increment"),
            ("I_expression", "increment"),
            ("I_narrative", "increment"),
        ] {
            if let Some(ablated) = by_id.get(name) {
                compared_ids.push(ablated.variant_id.clone());
                for dim in dims {
                    let (diffs, base_mean, ablated_mean, base_scores, ablated_scores) =
                        pair_dimension_diffs(
                            &collect_variant_dim_scores(ablated, dim),
                            &collect_variant_dim_scores(base, dim),
                        );
                    if diffs.len() < 2 {
                        tracing::debug!(
                            ablation = name,
                            dimension = dim,
                            pairs = diffs.len(),
                            "消融对比配对样本不足，跳过该行"
                        );
                        continue;
                    }
                    raw_rows.push(RawRow {
                        ablation: ablated,
                        dimension: dim,
                        comparison_type,
                        base_variant: base.variant_id.as_str(),
                        base_scores,
                        ablated_scores,
                        base_mean,
                        ablated_mean,
                        wilcoxon_p: wilcoxon_signed_rank_p(&diffs).unwrap_or(1.0),
                        cohens_d: cohens_d_paired(&diffs),
                        ci_low: metric_stat(&diffs).ci_low,
                        ci_high: metric_stat(&diffs).ci_high,
                        diffs,
                    });
                }
            }
        }
    } else {
        tracing::warn!("消融对比报告：未找到 B1 基线档位，S 组（替代）与 I 组（净增量）无法对比");
    }

    // 多比较 FDR 校正（Benjamini–Hochberg，作用于全部候选行）。
    let p_raw: Vec<f64> = raw_rows.iter().map(|r| r.wilcoxon_p).collect();
    let p_fdr = bh_fdr_adjust(&p_raw);

    let mut rows = Vec::with_capacity(raw_rows.len());
    for (raw, p_fdr) in raw_rows.into_iter().zip(p_fdr) {
        let ablation_name = raw.ablation.variant_id.as_str();
        // 显著性判定线：p_fdr<0.05 ∧ |d_z|≥0.3 ∧ CI 不含 0
        let ci_excludes_zero = raw.ci_low > 0.0 || raw.ci_high < 0.0;
        let significant = p_fdr < 0.05 && raw.cohens_d.abs() >= 0.3 && ci_excludes_zero;
        // 等效性判定：TOST 只能证伪"存在实质净增量"，用于补上显著性框架的盲区
        // （长期只得到"不显著"时，无法区分"真的没增量"与"样本不足"）。
        let tost = tost_equivalence(&raw.diffs, &raw.base_scores, &raw.ablated_scores, 0.3);
        let (equiv_bound, tost_p, equivalent) = match tost {
            Some(t) => (t.bound, t.p, t.equivalent),
            None => (0.0, 1.0, false),
        };
        let cohens_d_pooled = cohens_d_pooled(&raw.base_scores, &raw.ablated_scores);
        // 均值差（消融档 − 基线档）。
        let mean_diff = raw.ablated_mean - raw.base_mean;
        // 对照类型名（人类可读），三态结论文案共用。
        let type_label = match raw.comparison_type {
            "removal" => "移除对照",
            "substitution" => "替代对照",
            _ => "净增量对照",
        };
        // 未达显著时的结论文案（等效 / 不可判定），三类对照共用；
        // 显著分支不使用，故只在需要时 clone。
        let equivalence_text = equivalence_annotation(
            type_label,
            p_fdr,
            tost_p,
            equiv_bound,
            cohens_d_pooled,
            raw.cohens_d,
            equivalent,
        );
        // 方向语义按对照类型区分（D-V20-006 口径）：
        // - removal（F 组 vs F0）：关注"移除后是否下降"；
        // - substitution（S 组 vs B1）：去 RAG 摘要只留单层，关注"能否替代 RAG 基座"；
        // - increment（I 组 vs B1）：B1 基座 + 单层，关注"叠加后是否净增"。
        let (direction, annotation) = match raw.comparison_type {
            "removal" => {
                if significant && mean_diff < 0.0 {
                    (
                        "down".to_string(),
                        format!(
                            "移除该层后质量显著下降（{:.3}），该层对「{}」有贡献",
                            mean_diff, raw.dimension
                        ),
                    )
                } else if significant {
                    (
                        "up".to_string(),
                        format!(
                            "移除该层后质量反升（{:.3}），该层在本维度疑似冗余/负作用",
                            mean_diff
                        ),
                    )
                } else {
                    ("none".to_string(), equivalence_text.clone())
                }
            }
            "substitution" => {
                // S 组：目标层在无 RAG 摘要时单独注入，与 B1（仅 RAG 摘要）比较。
                if significant && mean_diff < 0.0 {
                    (
                        "down".to_string(),
                        format!(
                            "替代对照：去 RAG 摘要仅该层显著低于 B1（{:.3}），该层无法独立替代 RAG 摘要基座",
                            mean_diff
                        ),
                    )
                } else if significant {
                    (
                        "up".to_string(),
                        format!(
                            "替代对照：去 RAG 摘要仅该层显著高于 B1（{:.3}），该层可独立替代 RAG 摘要基座",
                            mean_diff
                        ),
                    )
                } else {
                    ("none".to_string(), equivalence_text.clone())
                }
            }
            _ => {
                // increment（I 组）：B1 基座 + 该层，与 B1 比较净增量。
                if significant && mean_diff < 0.0 {
                    (
                        "down".to_string(),
                        format!(
                            "净增量对照：在 B1 基座上叠加该层显著下降（{:.3}），层叠加为负向（压缩/干扰）",
                            mean_diff
                        ),
                    )
                } else if significant {
                    (
                        "up".to_string(),
                        format!(
                            "净增量对照：在 B1 基座上叠加该层显著提升（{:.3}），该层有正向净增量",
                            mean_diff
                        ),
                    )
                } else {
                    ("none".to_string(), equivalence_text)
                }
            }
        };
        // 综合判定：显著优先（显著与等效互斥），否则区分"证得等效"与"证据不足"。
        let verdict = if significant {
            format!("significant_{direction}")
        } else if equivalent {
            "equivalent".to_string()
        } else {
            "inconclusive".to_string()
        };

        rows.push(AblationComparisonRow {
            ablation_variant: ablation_name.to_string(),
            description: raw.ablation.description.clone(),
            comparison_type: raw.comparison_type.to_string(),
            base_variant: raw.base_variant.to_string(),
            dimension: raw.dimension.to_string(),
            n_pairs: raw.diffs.len(),
            base_mean: raw.base_mean,
            ablated_mean: raw.ablated_mean,
            mean_diff: raw.ablated_mean - raw.base_mean,
            wilcoxon_p: raw.wilcoxon_p,
            p_fdr,
            cohens_d: raw.cohens_d,
            cohens_d_pooled,
            equiv_bound,
            tost_p,
            equivalent,
            verdict,
            ci95_low: raw.ci_low,
            ci95_high: raw.ci_high,
            significant,
            direction,
            annotation,
        });
    }

    // 辅助指标：覆盖所有参与对比档位 + 基线档位（从 run 实验明细取回复指标）。
    let mut compared: Vec<String> = compared_ids;
    if let Some(b) = f0 {
        compared.push(b.variant_id.clone());
    }
    if let Some(b) = b1 {
        compared.push(b.variant_id.clone());
    }
    let mut aux = Vec::new();
    for vr in &experiment.variants {
        if !compared.contains(&vr.variant_id) {
            continue;
        }
        aux.push(variant_aux_metrics(vr));
    }

    AblationReport {
        baseline_variant: f0
            .map(|v| v.variant_id.clone())
            .or_else(|| b1.map(|v| v.variant_id.clone()))
            .unwrap_or_default(),
        rows,
        aux,
        judgment_dimensions: dims.iter().map(|d| d.to_string()).collect(),
        descriptive_dimensions: vec!["emotion".to_string()],
        dimension_scope_note: EMOTION_DESCRIPTIVE_NOTE.to_string(),
        equivalence_note: "等效性检验：TOST（双单侧 t 检验，df=配对数−1），等效边界取 |d_av|=0.3，\
            即原始差分 ±0.3×合并SD；tost_p<0.05 判定「等效（无实质净增量）」。\
            显著性仍按配对 Wilcoxon + d_z + 95%CI + BH-FDR。"
            .to_string(),
    }
}

/// 计算单档位辅助指标（平均回复长度 / 平均耗时 / 空回复率）。
fn variant_aux_metrics(vr: &ProbeVariantResult) -> VariantAuxMetrics {
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

// =========================================================
// 配对非参检验与效应量（纯函数，可单测）
// =========================================================

/// 配对 Wilcoxon 符号秩检验双尾 p 值（正态近似，无零差分）。
///
/// 算法:
/// - 剔除零差分后取绝对值排序，相同绝对值取平均秩；
/// - W+ = 正差分秩和；W 均值/方差（不含结校正的近似）→ z → 双尾 p。
/// - 样本量过小（n<5）时近似偏保守/不可靠，返回 `None`（调用方按 p=1.0 处理）。
pub(super) fn wilcoxon_signed_rank_p(diffs: &[f64]) -> Option<f64> {
    // 剔除零差分
    let mut abs_pairs: Vec<(f64, bool)> = diffs
        .iter()
        .filter(|d| d.abs() > 1e-12)
        .map(|d| (d.abs(), *d > 0.0))
        .collect();
    let n = abs_pairs.len();
    if n < 5 {
        return None; // 样本过小，正态近似不可靠
    }
    abs_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // 平均秩（处理相同绝对值）
    let mut w_plus = 0.0f64;
    let mut i = 0usize;
    while i < n {
        let mut j = i;
        while j + 1 < n && (abs_pairs[j + 1].0 - abs_pairs[i].0).abs() < 1e-12 {
            j += 1;
        }
        let rank_avg = (i + j + 2) as f64 / 2.0; // 1-based 位置平均
        for pair in &abs_pairs[i..=j] {
            if pair.1 {
                w_plus += rank_avg;
            }
        }
        i = j + 1;
    }

    // 无结近似：mean = n(n+1)/4，var = n(n+1)(2n+1)/24
    let n_f = n as f64;
    let mean = n_f * (n_f + 1.0) / 4.0;
    let variance = n_f * (n_f + 1.0) * (2.0 * n_f + 1.0) / 24.0;
    if variance <= 0.0 {
        return None;
    }
    let z = (w_plus - mean) / variance.sqrt();
    Some(2.0 * (1.0 - normal_cdf(z.abs())))
}

/// 标准正态分布 CDF（erf 近似）。
pub(super) fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf_approx(z / std::f64::consts::SQRT_2))
}

/// erf 近似（Abramowitz–Stegun 7.1.26，最大误差 ~1.5e-7）。
pub(super) fn erf_approx(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    if x > 6.0 {
        return sign;
    }
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    sign * (1.0 - poly * (-x * x).exp())
}

/// 配对 Cohen's d（d_z = mean(diff) / sd(diff)）。
///
/// 说明: 差分为零（sd≈0）且均值非零时以 ±10 标记"远超效应量阈值"
/// （避免 inf 破坏判定与序列化）；均值亦为零 → 0.0。
pub(super) fn cohens_d_paired(diffs: &[f64]) -> f64 {
    let n = diffs.len();
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f64;
    let mean = diffs.iter().sum::<f64>() / n_f;
    if n == 1 {
        return if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        };
    }
    let variance = diffs.iter().map(|d| (d - mean) * (d - mean)).sum::<f64>() / (n_f - 1.0);
    let sd = variance.sqrt();
    if sd < 1e-12 {
        if mean.abs() < 1e-12 {
            0.0
        } else {
            mean.signum() * 10.0
        }
    } else {
        mean / sd
    }
}

/// Benjamini–Hochberg FDR 校正。
///
/// 返回与输入等长的校正后 q 值；空输入返回空。
pub(super) fn bh_fdr_adjust(p_values: &[f64]) -> Vec<f64> {
    let m = p_values.len();
    if m == 0 {
        return Vec::new();
    }
    // 索引排序（小 → 大）
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|a, b| {
        p_values[*a]
            .partial_cmp(&p_values[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut q = vec![1.0f64; m];
    // 从最大 p 反向累计取最小
    let mut running_min = f64::INFINITY;
    for (rank_idx, &orig_idx) in order.iter().enumerate().rev() {
        let raw = p_values[orig_idx];
        let adjusted = (raw * m as f64 / (rank_idx + 1) as f64).min(1.0);
        running_min = running_min.min(adjusted);
        q[orig_idx] = running_min;
    }
    q
}

// =========================================================
// 人工抽检校准（T-V16-4-004）
// =========================================================

/// 读取人工抽检校准文件。
///
/// 格式（JSON）:
/// ```json
/// { "scores": [ {"item_id": "tone-0001", "score": 4}, ... ] }
/// ```
/// 或简单数组 `[{"item_id": "...", "score": 4}]`。
pub(super) fn read_manual_scores(path: &Path) -> anyhow::Result<Vec<ManualScore>> {
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
fn compute_calibration(
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

// =========================================================
// 知识层抽取质量评估（T-V16-4-005）
// =========================================================

/// 判断某档位是否含记忆注入（RAG 摘要基座 memory_rag 开启）。
///
/// 说明:
/// - 复用 `AblationProfile::apply_to` 的闸门映射作为唯一真源，避免在此重复维护
///   档位语义（B1/F0/F1~F4/I_* 为含记忆；B0/S_* 为不含记忆）。
/// - `ablation=None`（无消融）等同完整体系，含记忆注入。
/// - 未知档位名按完整体系处理（不误判为无记忆）。
fn variant_uses_memory_injection(ablation: Option<&str>) -> bool {
    match ablation {
        None => true,
        Some(name) => match AblationProfile::parse_name(name) {
            Some(profile) => {
                let mut cfg = ramaria_core::config::RamariaConfig::default();
                profile.apply_to(&mut cfg);
                cfg.injection.memory_rag
            }
            None => true,
        },
    }
}

/// 汇总单一判据口径下的命中 / 误报 / 漏报。
///
/// 说明:
/// - `selector` 从单题事实维评分中取出该判据的综合分；字段缺失（旧评分数值文件）
///   返回 None，该题不计入该口径样本。
/// - 阈值：命中 `≥0.5` / 误报 `<0.3` / 漏报 `<0.4`（沿用旧口径，三判据共用）。
fn knowledge_judge_rates(
    items: &[&ItemEvaluation],
    judge: &str,
    selector: impl Fn(&FactItemScore) -> Option<f64>,
) -> KnowledgeJudgeRates {
    let vals: Vec<f64> = items
        .iter()
        .filter_map(|i| i.fact.as_ref().and_then(&selector))
        .collect();
    let sample_count = vals.len();
    let divisor = sample_count.max(1) as f64;
    let false_negative_rate = vals.iter().filter(|v| **v < 0.4).count() as f64 / divisor;
    KnowledgeJudgeRates {
        judge: judge.to_string(),
        sample_count,
        hit_rate: vals.iter().filter(|v| **v >= 0.5).count() as f64 / divisor,
        false_positive_rate: vals.iter().filter(|v| **v < 0.3).count() as f64 / divisor,
        false_negative_rate,
        miss_target_met: sample_count > 0 && false_negative_rate < 0.10,
    }
}

/// 按给定事实维题集与档位集合汇总单一口径的质量指标。
pub(super) fn summarize_knowledge_scope(
    scope: &str,
    label: &str,
    variant_ids: Vec<String>,
    items: &[&ItemEvaluation],
) -> KnowledgeQualityScope {
    // 判据口径：legacy（旧 2-gram 覆盖）/ norm（长度归一）/ point（子句级事实点）。
    let judge_rates = vec![
        knowledge_judge_rates(items, "legacy", |f| Some(f.score)),
        knowledge_judge_rates(items, "norm", |f| f.score_norm),
        knowledge_judge_rates(items, "point", |f| f.score_point),
    ];
    // 扁平字段（兼容既有消费方）取 legacy 口径。
    let legacy = &judge_rates[0];
    let sample_count = legacy.sample_count;
    let range = if variant_ids.is_empty() {
        "（无样本档位）".to_string()
    } else {
        format!("档位 [{}]", variant_ids.join(", "))
    };
    KnowledgeQualityScope {
        scope: scope.to_string(),
        description: format!("{label}：{range}，样本 {sample_count} 题"),
        variant_ids,
        sample_count,
        fact_hit_count: (legacy.hit_rate * sample_count as f64).round() as usize,
        false_positive_rate: legacy.false_positive_rate,
        false_negative_rate: legacy.false_negative_rate,
        miss_target_met: legacy.miss_target_met,
        judge_rates,
    }
}

/// 评估知识层抽取质量（双口径：含记忆注入 / 全部档位池化）。
pub(super) fn assess_knowledge_quality(evaluation: &ProbeEvaluation) -> KnowledgeQualityReport {
    let mut all_items: Vec<&ItemEvaluation> = Vec::new();
    let mut memory_items: Vec<&ItemEvaluation> = Vec::new();
    let mut all_ids: Vec<String> = Vec::new();
    let mut memory_ids: Vec<String> = Vec::new();

    for v in &evaluation.variants {
        let memory = variant_uses_memory_injection(v.params.ablation.as_deref());
        let mut counted = false;
        for item in &v.items {
            if item.dimension == "fact" && item.fact.is_some() {
                all_items.push(item);
                counted = true;
                if memory {
                    memory_items.push(item);
                }
            }
        }
        if counted {
            all_ids.push(v.variant_id.clone());
            if memory {
                memory_ids.push(v.variant_id.clone());
            }
        }
    }

    let pooled = summarize_knowledge_scope(
        "pooled_all",
        "对照口径：全部档位池化（含无记忆基线）",
        all_ids,
        &all_items,
    );
    let primary = summarize_knowledge_scope(
        "memory_injected",
        "主口径：含记忆注入档位",
        memory_ids,
        &memory_items,
    );

    let annotation = if primary.sample_count == 0 {
        format!(
            "无含记忆注入档位样本，主口径不可用；对照口径（全部档位池化）样本 {} 题（漏报 {:.1}%）。\
             无事实维样本时两口径均不可评估。",
            pooled.sample_count,
            pooled.false_negative_rate * 100.0
        )
    } else {
        format!(
            "主口径（含记忆注入档位，{} 档）漏报 {:.1}%（目标 <10% → {}）、误报 {:.1}%；\
             对照口径（全部档位池化，含无记忆基线）漏报 {:.1}%、误报 {:.1}%。\
             两口径差异全部来自无记忆档位，指标不可比；终验采用含记忆注入口径。",
            primary.variant_ids.len(),
            primary.false_negative_rate * 100.0,
            if primary.miss_target_met {
                "达标"
            } else {
                "未达标"
            },
            primary.false_positive_rate * 100.0,
            pooled.false_negative_rate * 100.0,
            pooled.false_positive_rate * 100.0,
        )
    };

    // 判据口径附注：逐判据列主口径漏报与达标情况（旧产物缺 norm/point 字段时自动略去）。
    let judge_note = {
        let rows: Vec<String> = primary
            .judge_rates
            .iter()
            .filter(|r| r.sample_count > 0)
            .map(|r| {
                format!(
                    "{} 漏报 {:.1}%（{}）",
                    r.judge,
                    r.false_negative_rate * 100.0,
                    if r.miss_target_met {
                        "达标"
                    } else {
                        "未达标"
                    }
                )
            })
            .collect();
        if rows.is_empty() {
            String::new()
        } else {
            format!(
                "事实维判据口径（主口径）：{}；三口径仅关键词项不同（legacy 旧 2-gram 覆盖 / \
                 norm 长度归一 / point 子句级事实点），余弦权重相同。",
                rows.join("、")
            )
        }
    };
    let annotation = format!("{annotation}{judge_note}");

    KnowledgeQualityReport {
        primary,
        pooled,
        annotation,
    }
}

// =========================================================
// 报告输出辅助
// =========================================================

/// 写 JSON 报告到文件。
fn write_report_json(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(report).context("报告 JSON 序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n")).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 写 markdown 报告到文件。
fn write_report_markdown(out: &str, report: &ProbeReport) -> anyhow::Result<()> {
    let md = render_report_markdown(report);
    if out == "-" {
        print!("{md}");
    } else {
        std::fs::write(out, md).with_context(|| format!("写入报告失败: {out}"))?;
    }
    Ok(())
}

/// 渲染 markdown 报告（档位对比表 + 定稿建议 + 校准 + 知识层质量）。
pub(super) fn render_report_markdown(report: &ProbeReport) -> String {
    let mut md = String::new();
    md.push_str("# Ramaria 探针档位对比报告\n\n");
    md.push_str(&format!("- persona: `{}`\n", report.persona_uid));
    md.push_str(&format!("- 数据集 seed: {}\n", report.dataset_seed));
    md.push_str(&format!(
        "- 语气 judge: {} / 事实 embedding: {}\n",
        report.judge_used, report.embedding_used
    ));
    md.push_str(&format!("- 生成时间: {}\n\n", report.generated_at));

    // 档位对比表
    md.push_str("## 档位评分对比\n\n");
    md.push_str(
        "| 档位 | 事实维 | 事实(归一) | 事实(事实点) | 语气维 | 情感维 | 成功/总 | 失败 | 说明 |\n",
    );
    md.push_str("|------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|------|\n");
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_norm = r
            .fact_score_norm
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_point = r
            .fact_score_point
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {}/{} | {} | {} |\n",
            r.variant_id,
            fact,
            fact_norm,
            fact_point,
            tone,
            emotion,
            r.success_count,
            r.total_count,
            r.failed_count,
            r.description.replace('|', "\\|")
        ));
    }
    md.push('\n');

    md.push_str("> 情感维为描述性指标（口径未校准），不参与层价值判定。\n\n");

    // 定稿建议
    md.push_str("## 定稿建议\n\n");
    for d in &report.recommendation.per_dimension {
        md.push_str(&format!(
            "**{}**：{}（最佳档位 {}）\n\n",
            d.dimension,
            d.reason,
            d.best_variant.as_deref().unwrap_or("—")
        ));
    }
    md.push_str(&format!("**综合**：{}\n\n", report.recommendation.overall));

    // 人工抽检校准
    if let Some(c) = &report.calibration {
        md.push_str("## 人工抽检校准\n\n");
        md.push_str(&format!(
            "- 抽检样本：{}/{}（{:.0}%）\n",
            c.sample_count,
            c.total_count,
            c.sample_rate * 100.0
        ));
        md.push_str(&format!(
            "- 同分一致性：{:.0}%\n",
            c.consistency_exact * 100.0
        ));
        md.push_str(&format!("- 平均绝对差：{:.2}\n", c.mean_abs_diff));
        md.push_str(&format!("- 偏差（judge−人工）：{:.2}\n", c.bias));
        if let Some(coef) = c.calibrated_coefficient {
            md.push_str(&format!("- 校准系数：{:.3}\n", coef));
        }
        md.push_str(&format!("- 标注：{}\n\n", c.annotation));
    }

    // 知识层质量（双口径：主口径含记忆注入 / 对照口径全部档位池化；每口径按判据分栏）
    if let Some(kq) = &report.knowledge_quality {
        md.push_str("## 知识层抽取质量评估（双口径）— 按判据口径分栏\n\n");
        md.push_str(
            "- 判据口径：legacy（旧 2-gram 覆盖，冻结）/ norm（长度归一）/ point（子句级事实点）；\n",
        );
        md.push_str("  阈值三口径共用：命中 ≥0.5 / 误报 <0.3 / 漏报 <0.4；余弦权重相同。\n\n");
        // description 已自带「主口径/对照口径」标签，标题不再重复拼接 tag。
        for scope in [&kq.primary, &kq.pooled] {
            md.push_str(&format!("### {}\n\n", scope.description));
            md.push_str("| 判据口径 | 样本 | 命中率(≥0.5) | 误报率(<0.3) | 漏报率(<0.4) | 漏报目标(<10%) |\n");
            md.push_str("|---|---|---|---|---|---|\n");
            // 旧 JSON 无判据明细 → 由 legacy 扁平字段回填单行，保证渲染路径恒有输出。
            let rows: Vec<KnowledgeJudgeRates> = if scope.judge_rates.is_empty() {
                vec![KnowledgeJudgeRates {
                    judge: "legacy".to_string(),
                    sample_count: scope.sample_count,
                    hit_rate: if scope.sample_count == 0 {
                        0.0
                    } else {
                        scope.fact_hit_count as f64 / scope.sample_count as f64
                    },
                    false_positive_rate: scope.false_positive_rate,
                    false_negative_rate: scope.false_negative_rate,
                    miss_target_met: scope.miss_target_met,
                }]
            } else {
                scope.judge_rates.clone()
            };
            for r in &rows {
                md.push_str(&format!(
                    "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {} |\n",
                    r.judge,
                    r.sample_count,
                    r.hit_rate * 100.0,
                    r.false_positive_rate * 100.0,
                    r.false_negative_rate * 100.0,
                    if r.sample_count == 0 {
                        "—"
                    } else if r.miss_target_met {
                        "达标"
                    } else {
                        "未达标"
                    }
                ));
            }
            md.push('\n');
        }
        md.push_str(&format!("- 结论：{}\n\n", kq.annotation));
    }

    // 客观风格形态指标（对照语气 judge；judge 在 20~30 字短回复上区分力不足）
    if !report.style_metrics.is_empty() {
        md.push_str("## 风格形态指标（客观口径，对照语气 judge）\n\n");
        md.push_str(
            "- 口径：只依赖回复文本，不依赖 judge。`参考重合` = 回复长度分布与 persona 参考\n",
        );
        md.push_str(
            "  （tone 题 `reference`，persona 原回复）长度分布的重叠系数（分箱 5 字、60 字封顶），\n",
        );
        md.push_str("  1.0 表示分布一致；`≤30字` 为新社交模板的目标区间占比。\n");
        md.push_str("- 用途：与语气维 judge 结论交叉验证；judge 判「无差异」时，本表可佐证差异确实不存在。\n\n");
        let ref_mean = report
            .style_metrics
            .iter()
            .find_map(|m| m.ref_len_mean)
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "—".to_string());
        md.push_str(&format!("- persona 参考均长：{ref_mean} 字\n\n"));
        md.push_str(
            "| 档位 | 均长 | 中位 | ≤30字 | 参考重合 | 语气词 | 疑问 | 感叹 | 复读 | 助手腔 |\n",
        );
        md.push_str("|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|\n");
        for m in &report.style_metrics {
            let overlap = m
                .len_ref_overlap
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "—".to_string());
            md.push_str(&format!(
                "| {} | {:.1} | {:.1} | {:.3} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
                m.variant_id,
                m.len_mean,
                m.len_median,
                m.len_le_30_rate,
                overlap,
                m.tone_particle_rate,
                m.question_rate,
                m.exclaim_rate,
                m.repeat_rate,
                m.assistant_marker_rate
            ));
        }
        md.push('\n');
    }

    // 消融对比统计（按对照类型分栏：removal 移除 / substitution 替代 / increment 净增量）
    if let Some(ab) = &report.ablation {
        md.push_str("## 消融对比统计\n\n");
        md.push_str(
            "- 方法：按题目配对 Wilcoxon 符号秩检验 + Cohen's d + 95% CI；\
             多比较经 Benjamini–Hochberg FDR 校正\n",
        );
        md.push_str("- 判定线：p_fdr<0.05 ∧ |d|≥0.3 ∧ CI 不含 0\n");
        md.push_str(&format!("- {}\n\n", ab.equivalence_note));
        md.push_str("- 对照语义（D-V20-006）：\n");
        md.push_str("  - **removal（移除，基线 F0）**：全开中逐层关闭 → 去掉某一层的边际损失；\n");
        md.push_str(
            "  - **substitution（替代，基线 B1）**：去 RAG 摘要、仅单专属层 → 单层能否替代 RAG；\n",
        );
        md.push_str("  - **increment（净增量，基线 B1）**：B1 基座 + 单专属层 → RAG 之上叠加一层的净增量。\n\n");
        md.push_str(&format!(
            "- 判定维度：{}（描述性展示：{}）\n\n",
            ab.judgment_dimensions.join(" / "),
            ab.descriptive_dimensions.join(" / ")
        ));

        let render_rows = |md: &mut String, label: &str, rows: &[&AblationComparisonRow]| {
            md.push_str(&format!("### {label}\n\n"));
            md.push_str(
                "| 消融档位 | 基线 | 维度 | 基线均分 | 档位均分 | Δ | p_fdr | d | 95%CI | 判定 |\n",
            );
            md.push_str("|------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|------|\n");
            for r in rows {
                // 判定列按三态 verdict 渲染，并附 TOST p 便于核对等效结论。
                let verdict_cell = {
                    let v = match r.verdict.as_str() {
                        "significant_down" => "↓ 显著下降",
                        "significant_up" => "↑ 显著提升",
                        "equivalent" => "≡ 等效（无净增量）",
                        _ => "? 不确定",
                    };
                    format!("{v}(n={}, TOST p={:.3})", r.n_pairs, r.tost_p)
                };
                md.push_str(&format!(
                    "| {} | {} | {} | {:.3} | {:.3} | {:.3} | {:.4} | {:.2} | [{:.3}, {:.3}] | {} |\n",
                    r.ablation_variant,
                    r.base_variant,
                    r.dimension,
                    r.base_mean,
                    r.ablated_mean,
                    r.mean_diff,
                    r.p_fdr,
                    r.cohens_d,
                    r.ci95_low,
                    r.ci95_high,
                    verdict_cell,
                ));
            }
            md.push('\n');
        };

        for (label, ctype) in [
            ("移除对照（F 组 vs F0）", "removal"),
            ("替代对照（S 组 vs B1）", "substitution"),
            ("净增量对照（I 组 vs B1）", "increment"),
        ] {
            let group: Vec<&AblationComparisonRow> = ab
                .rows
                .iter()
                .filter(|r| r.comparison_type == ctype)
                .collect();
            if !group.is_empty() {
                render_rows(&mut md, label, &group);
            }
        }

        md.push_str("### 辅助指标\n\n");
        md.push_str("| 档位 | 平均回复(字符) | 平均耗时(ms) | 空回复率 | 成功/总 |\n");
        md.push_str("|------|:---:|:---:|:---:|:---:|\n");
        for a in &ab.aux {
            md.push_str(&format!(
                "| {} | {:.1} | {:.1} | {:.1}% | {}/{} |\n",
                a.variant_id,
                a.reply_chars_mean,
                a.elapsed_ms_mean,
                a.empty_reply_rate * 100.0,
                a.success_count,
                a.total_count
            ));
        }
        md.push('\n');
    }

    // 描述性指标（口径未校准，不参与层价值判定）
    md.push_str("## 描述性指标（不参与层价值判定）\n\n");
    for note in &report.descriptive_metrics {
        md.push_str(&format!("- {note}\n"));
    }
    md.push('\n');

    // 辅助指标四件套（D-V20-006，产物可复算近似）
    md.push_str("## 辅助指标（产物可复算）\n\n");
    let fmt_opt = |v: Option<f64>| {
        v.map(|x| format!("{:.1}%", x * 100.0))
            .unwrap_or_else(|| "-".to_string())
    };
    md.push_str(&format!(
        "- 证据链可追溯率：{}\n",
        fmt_opt(report.auxiliary.evidence_traceability_rate)
    ));
    md.push_str(&format!(
        "- 行为规则命中率（代理）：{}\n",
        fmt_opt(report.auxiliary.behavior_rule_hit_rate)
    ));
    md.push_str(&format!(
        "- 情境路由误用率（代理）：{}\n",
        fmt_opt(report.auxiliary.situation_route_misuse_rate)
    ));
    match report.auxiliary.profile_regression_output_stability {
        Some(s) => md.push_str(&format!(
            "- 画像回归（跨轮 fact/tone/emotion std 均值）：{:.4}\n",
            s
        )),
        None => {
            md.push_str("- 画像回归（跨轮 fact/tone/emotion std 均值）：-（无 --repeat 明细）\n")
        }
    }
    md.push_str(&format!(
        "- 口径与局限：{}\n\n",
        report.auxiliary.annotation
    ));

    // 数据特性与外部效度局限（D-V20-005 必出字段）
    md.push_str("## 数据特性与外部效度局限\n\n");
    if report.limitations.is_empty() {
        md.push_str("- （无附加局限说明）\n");
    } else {
        for l in &report.limitations {
            md.push_str(&format!("- {l}\n"));
        }
    }
    md.push('\n');

    md.push_str("---\n*由 `ramaria probe report` 自动生成，供 M5 定稿实验参考。*\n");
    md
}

/// 文本模式打印报告摘要（stdout 只输出数据）。
fn print_report_summary(report: &ProbeReport) {
    println!(
        "探针报告: persona={} | {} 档位对比 | judge={} | embedding={}",
        report.persona_uid,
        report.variants.len(),
        report.judge_used,
        report.embedding_used
    );
    for r in &report.variants {
        let fact = r
            .fact_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_norm = r
            .fact_score_norm
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let fact_point = r
            .fact_score_point
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let tone = r
            .tone_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        let emotion = r
            .emotion_score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  档位 {:<14} 事实={:<6} 事实归一={:<6} 事实点={:<6} 语气={:<6} 情感={:<6} 成功={}/{} — {}",
            r.variant_id,
            fact,
            fact_norm,
            fact_point,
            tone,
            emotion,
            r.success_count,
            r.total_count,
            r.description
        );
    }
    println!("定稿建议: {}", report.recommendation.overall);
    if let Some(c) = &report.calibration {
        println!(
            "校准: 样本 {}/{} 同分 {:.0}% 偏差 {:.2} {}",
            c.sample_count,
            c.total_count,
            c.consistency_exact * 100.0,
            c.bias,
            if c.inconsistent {
                "⚠ 不一致"
            } else {
                "✓ 一致"
            }
        );
    }
    if let Some(kq) = &report.knowledge_quality {
        println!(
            "知识层（主口径 含记忆注入）: 样本 {} 误报 {:.1}% 漏报 {:.1}% {} | 对照口径（全量池化）漏报 {:.1}%",
            kq.primary.sample_count,
            kq.primary.false_positive_rate * 100.0,
            kq.primary.false_negative_rate * 100.0,
            if kq.primary.miss_target_met {
                "（达标）"
            } else {
                "（未达标）"
            },
            kq.pooled.false_negative_rate * 100.0
        );
    }
    if let Some(ab) = &report.ablation {
        let sig = ab.rows.iter().filter(|r| r.significant).count();
        println!(
            "消融对比: {} 行对比（基线 {}），显著 {} 行",
            ab.rows.len(),
            ab.baseline_variant,
            sig
        );
    }
    // 辅助指标四件套摘要（产物可复算近似）
    let fmt_opt = |v: Option<f64>| {
        v.map(|x| format!("{:.1}%", x * 100.0))
            .unwrap_or_else(|| "-".to_string())
    };
    println!(
        "辅助指标: 可追溯率={} 规则命中(代理)={} 路由误用(代理)={} 画像回归(std)={}",
        fmt_opt(report.auxiliary.evidence_traceability_rate),
        fmt_opt(report.auxiliary.behavior_rule_hit_rate),
        fmt_opt(report.auxiliary.situation_route_misuse_rate),
        report
            .auxiliary
            .profile_regression_output_stability
            .map(|s| format!("{:.4}", s))
            .unwrap_or_else(|| "-".to_string())
    );
    crate::ui::info("用 --output 生成 markdown/JSON 报告文件");
}
