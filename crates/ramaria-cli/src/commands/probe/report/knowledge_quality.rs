//! crates/ramaria-cli/src/commands/probe/report/knowledge_quality.rs - 探针 report 知识层质量
//!
//! 设计特点:
//! - 基于评分数值中的事实维题目评估误报 / 漏报率（目标 <10%）
//! - 双口径（含记忆注入 / 全部档位池化）× 三判据（legacy / norm / point）分栏
//! - 按档位是否启用记忆注入拆分统计范围

use super::super::evaluate::FactItemScore;
use super::super::evaluate::ItemEvaluation;
use super::super::evaluate::ProbeEvaluation;
use super::super::types::AblationProfile;

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
/// - 双口径：主口径只统计含记忆注入档位；对照口径池化全部档位
///   （含无记忆基线），用于说明两者差异来源。
/// - 判据分栏：每个口径内再按 `judge_rates`（legacy / norm / point）分别给出命中/漏报，
///   使短回复模板下的长度伪影（旧判据漏报虚高）可被直接对照。
#[derive(Debug, Clone, serde::Serialize)]
pub struct KnowledgeQualityReport {
    /// 主口径：含记忆注入档位
    pub primary: KnowledgeQualityScope,
    /// 对照口径：全部档位池化（含无记忆基线，供可比性对照）
    pub pooled: KnowledgeQualityScope,
    /// 口径说明（必出）
    pub annotation: String,
}

/// 判断某档位是否含记忆注入（RAG 摘要基座 memory_rag 开启）。
///
/// 说明:
/// - 复用 `AblationProfile::apply_to` 的闸门映射作为唯一真源，避免在此重复维护
///   档位语义（B1/F0/F1~F4/I_* 为含记忆；B0/S_* 为不含记忆）。
/// - `ablation=None`（无消融）等同完整体系，含记忆注入。
/// - 未知档位名按完整体系处理（不误判为无记忆）。
pub(crate) fn variant_uses_memory_injection(ablation: Option<&str>) -> bool {
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
pub(crate) fn knowledge_judge_rates(
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
pub(crate) fn summarize_knowledge_scope(
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
pub(crate) fn assess_knowledge_quality(evaluation: &ProbeEvaluation) -> KnowledgeQualityReport {
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
