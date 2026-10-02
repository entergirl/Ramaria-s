//! crates/ramaria-cli/src/commands/probe/report/style_metrics.rs - 探针 report 风格形态指标
//!
//! 设计特点:
//! - 只看回复文本的客观口径：长度分布 / 与 persona 参考的长度重合度 / 语气词率 /
//! - 疑问感叹率 / 复读率 / 助手腔标记率，作为语气 judge 的交叉验证口径
//! - 各 *rate 按回复条数计（非按字数），复读率检测同档位内的模板化重复
//! - 长度直方图分箱宽 5 字、60 字封顶，重合度取 Σ min(p_i, q_i)
//! - 情感维降级声明常量在此定义（描述性指标，报告必出）

use super::super::evaluate::ProbeEvaluation;
use super::super::types::ProbeExperiment;

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

/// 语气词 / 口癖字符表：回复含任一字符即计该条命中。
///
/// 说明: 取高亲密度口语语料的高频句尾语气词与笑声拟声（榆：哦哦 / 我找一下 / 是联动！/ 对啊对啊）。
pub(crate) const STYLE_TONE_PARTICLES: &[char] = &[
    '呀', '啦', '哦', '诶', '啊', '嘛', '吧', '哈', '嗯', '咦', '哇', '唉', '噢', '咯', '嘞', '嘻',
    '嘿', '呐', '嗷',
];

/// 助手腔标记词：回复含任一词即计该条为助手腔。
///
/// 说明: 用于检测短模板是否回退到助手腔。
pub(crate) const STYLE_ASSISTANT_MARKERS: &[&str] = &[
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
pub(crate) const STYLE_LEN_BIN: usize = 5;

/// 回复长度直方图封顶（字），超过按最后一箱计。
pub(crate) const STYLE_LEN_CAP: usize = 60;

/// "短回复"阈值（字）：新社交模板的目标区间为 20~30 字。
pub(crate) const STYLE_SHORT_LEN: usize = 30;

/// 档位回复的客观风格形态指标。
///
/// 说明:
/// - 用途：语气 judge（`probe-judge-v2`）在 20~30 字短回复上区分力不足，
///   本组指标只依赖回复文本本身，作为语气维的**客观对照口径**，与 judge 结论交叉验证。
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
pub(crate) fn collect_variant_replies(
    experiment: &ProbeExperiment,
    variant_id: &str,
) -> Vec<String> {
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
pub(crate) fn persona_reference_lengths(evaluation: Option<&ProbeEvaluation>) -> Vec<usize> {
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
pub(crate) fn style_len_hist(lengths: &[usize]) -> std::collections::HashMap<usize, f64> {
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
pub(crate) fn style_len_overlap(a: &[usize], b: &[usize]) -> f64 {
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
pub(crate) fn median_of(sorted: &[usize]) -> f64 {
    match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2] as f64,
        n => (sorted[n / 2 - 1] + sorted[n / 2]) as f64 / 2.0,
    }
}

/// 计算全部档位的客观风格形态指标。
pub(crate) fn compute_style_metrics(
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
