//! crates/ramaria-eval/src/report/markdown.rs - Ramaria 评测报告 markdown 渲染
//!
//! 设计特点:
//! - 章节顺序固定：摘要、条件、主表、分层、比较、预算与 oracle、结论、缺项、口径
//! - 数值格式统一走 report::format，空值写 `-`，不写 N/A
//! - 检索列优先 artifact 级，缺映射时回退消息级并在章末注明
//! - 结论与缺项直接来自 report.json，markdown 不再二次判定
//! - 不写生成时间、机器信息与过程描述

use std::fmt::Write as _;

use crate::run::AnswerModelKind;
use crate::score::{JudgeStatus, PersonaSummary, RetrievalScore};

use super::artifact::{HypothesisStatus, ReportArtifact, ReportCondition, ReportRun, RunRole};
use super::compare::split_condition_id;
use super::format;

/// 渲染完整报告。
///
/// 参数:
/// - `artifact`: 报告结构；
/// - `corpus`: 主运行语料名；未知时为 `None`；
/// - `language`: 语料语言标签；未知时为 `None`。
///
/// 返回:
/// - 以换行结尾的 markdown 文本。
pub(crate) fn render(
    artifact: &ReportArtifact,
    corpus: Option<&str>,
    language: Option<&str>,
) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "# Ramaria 评测报告：{}", artifact.run_id);
    let primary = artifact
        .runs
        .iter()
        .find(|run| run.role == RunRole::Primary);

    section_summary(&mut output, artifact, primary, corpus, language);
    section_conditions(&mut output, artifact);
    section_main_table(&mut output, artifact);
    section_categories(&mut output, artifact);
    section_comparisons(&mut output, artifact);
    section_budget_oracle(&mut output, artifact);
    section_hypotheses(&mut output, artifact);
    section_missing(&mut output, artifact);
    section_method(&mut output);
    output
}

// =========================================================
// 第 1 节：运行摘要
// =========================================================

fn section_summary(
    output: &mut String,
    artifact: &ReportArtifact,
    primary: Option<&ReportRun>,
    corpus: Option<&str>,
    language: Option<&str>,
) {
    let _ = writeln!(
        output,
        "
## 1. 运行摘要
"
    );
    let corpus_text = match (corpus, language) {
        (Some(name), Some(language)) => format!("{name}（{language}）"),
        (Some(name), None) => name.to_string(),
        _ => "未知".to_string(),
    };
    let _ = writeln!(output, "- 运行：{}", artifact.run_id);
    let _ = writeln!(output, "- 语料：{corpus_text}");
    if let Some(primary) = primary {
        let _ = writeln!(output, "- 档位：{}", join_or(&primary.tiers, "、"));
        let _ = writeln!(output, "- 管线：{}", join_or(&primary.pipelines, "、"));
        let budgets: Vec<String> = primary.budgets.iter().map(u32::to_string).collect();
        let _ = writeln!(output, "- 预算：{} 字符", join_or(&budgets, "、"));
        let questions = match primary.question_sample {
            Some(sample) => format!("每档抽样 {sample} 题"),
            None => "未抽样（题数见条件表）".to_string(),
        };
        let _ = writeln!(output, "- 题数：{questions}");
    }
    let _ = writeln!(output, "- 判分模型：{}", judge_text(&artifact.source.judge));
    let _ = writeln!(output, "- 参与运行：{}", runs_text(&artifact.runs));
}

/// 判分模型的人读描述。
fn judge_text(judge: &JudgeStatus) -> String {
    match judge {
        JudgeStatus::Disabled => "未启用（只产出确定性指标）".to_string(),
        JudgeStatus::Configured {
            model,
            kind,
            votes,
            temperature,
            ..
        } => format!(
            "{model}（{}，{votes} 票，温度 {temperature}）",
            match kind {
                AnswerModelKind::Local => "本地模型",
                AnswerModelKind::Cloud => "云端模型",
            }
        ),
    }
}

/// 参与运行的人读描述。
fn runs_text(runs: &[ReportRun]) -> String {
    if runs.is_empty() {
        return "无".to_string();
    }
    runs.iter()
        .map(|run| format!("{}:{}", run.role.as_str(), run.run_id))
        .collect::<Vec<_>>()
        .join("、")
}

// =========================================================
// 第 2 节：条件行
// =========================================================

fn section_conditions(output: &mut String, artifact: &ReportArtifact) {
    let _ = writeln!(
        output,
        "
## 2. 条件行
"
    );
    if artifact.conditions.is_empty() {
        let _ = writeln!(output, "没有判分产物，条件行未产出。");
        return;
    }
    let _ = writeln!(
        output,
        "| 档位 | 管线 | 预算 | 答题模型 | 题数 | 参与判分 | 无 gold | 失败题 | judge 失败 | 得分率 |"
    );
    let _ = writeln!(output, "|---|---|---|---|---|---|---|---|---|---|");
    for condition in &artifact.conditions {
        let _ = writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            condition.tier,
            condition.pipeline,
            condition.budget_chars,
            model_text(&condition.answer_model),
            condition.questions,
            condition.scored,
            condition.skipped_no_gold,
            condition.failed_items,
            condition.judge_failed,
            format::fixed(rubric_ratio(condition), 3)
        );
    }
}

// =========================================================
// 第 3 节：主表
// =========================================================

fn section_main_table(output: &mut String, artifact: &ReportArtifact) {
    let _ = writeln!(
        output,
        "
## 3. 主表
"
    );
    if artifact.conditions.is_empty() {
        let _ = writeln!(output, "没有判分产物，主表未产出。");
        return;
    }
    let _ = writeln!(
        output,
        "得分率为分项 Micro 平均；检索列为 artifact 级（缺映射时回退消息级）。
"
    );
    let _ = writeln!(
        output,
        "| 档位 | 管线 | 预算 | 得分率 | 全要点命中 | 覆盖（前） | 覆盖（后） | 精确率（后） | MRR（后） | 注入单元 | 语气 | 事实 | 情感 |"
    );
    let _ = writeln!(
        output,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for condition in &artifact.conditions {
        let retrieval = retrieval_cells(&condition.retrieval);
        let persona = persona_cells(condition.persona.as_ref());
        let _ = writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            condition.tier,
            condition.pipeline,
            condition.budget_chars,
            format::fixed(rubric_ratio(condition), 3),
            format::fixed(rubric_full_hit(condition), 3),
            retrieval.0,
            retrieval.1,
            retrieval.2,
            retrieval.3,
            retrieval.4,
            persona.0,
            persona.1,
            persona.2
        );
    }
}

/// 检索五列：覆盖前后、精确率后、MRR 后、注入单元。
fn retrieval_cells(retrieval: &RetrievalScore) -> (String, String, String, String, String) {
    match &retrieval.artifact {
        Some(artifact) => (
            format::fixed(Some(artifact.coverage_pre), 3),
            format::fixed(Some(artifact.coverage_post), 3),
            format::fixed(Some(artifact.precision_post), 3),
            format::fixed(Some(artifact.mrr_post), 3),
            format::fixed(Some(artifact.units_post_avg), 1),
        ),
        None => match &retrieval.message {
            Some(message) => (
                format::fixed(Some(message.coverage_pre), 3),
                format::fixed(Some(message.coverage_post), 3),
                format::fixed(Some(message.precision_post), 3),
                format::fixed(Some(message.mrr_post), 3),
                format::fixed(Some(message.units_post_avg), 1),
            ),
            None => (
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
            ),
        },
    }
}

/// 人格三维三列。
fn persona_cells(persona: Option<&PersonaSummary>) -> (String, String, String) {
    match persona {
        Some(persona) => (
            format::fixed(persona.tone, 2),
            format::fixed(persona.fact, 2),
            format::fixed(persona.emotion, 2),
        ),
        None => ("-".to_string(), "-".to_string(), "-".to_string()),
    }
}

// =========================================================
// 第 4 节：分层指标
// =========================================================

fn section_categories(output: &mut String, artifact: &ReportArtifact) {
    let _ = writeln!(
        output,
        "
## 4. 分层指标
"
    );
    let _ = writeln!(
        output,
        "分项要点按语料 `score_points` 自建判分，无官方判分脚本，分值与论文不可直接比较。\n"
    );
    let has_rubric = artifact
        .conditions
        .iter()
        .any(|condition| condition.rubric.is_some());
    if !has_rubric {
        let _ = writeln!(output, "本次未判分（judge 未启用或没有可判分的要点）。");
        return;
    }
    let _ = writeln!(
        output,
        "九类 Macro 为各类别分项得分率均值；多标签题计入它的每个类别。
"
    );
    let mut header = String::from("| 条件 |");
    let mut divider = String::from("|---|");
    for category in 0..=8 {
        let _ = write!(header, " C{category} |");
        divider.push_str("---|");
    }
    let _ = writeln!(output, "{header}");
    let _ = writeln!(output, "{divider}");
    for condition in &artifact.conditions {
        let mut row = format!("| {} |", condition.condition_id);
        for category in 0..=8 {
            let value = condition
                .rubric
                .as_ref()
                .and_then(|rubric| {
                    rubric
                        .macro_by_category
                        .iter()
                        .find(|entry| entry.category == category)
                })
                .and_then(|entry| entry.mean_ratio);
            let _ = write!(row, " {} |", format::fixed(value, 3));
        }
        let _ = writeln!(output, "{row}");
    }
}

// =========================================================
// 第 5 节：配对比较
// =========================================================

fn section_comparisons(output: &mut String, artifact: &ReportArtifact) {
    let _ = writeln!(
        output,
        "
## 5. 配对比较
"
    );
    if artifact.comparisons.is_empty() {
        let _ = writeln!(output, "没有可配对的比较。");
        return;
    }
    let _ = writeln!(
        output,
        "差值方向为 left - right；p 为 Wilcoxon 双尾，p_adj 为 BH-FDR 校正值。
"
    );
    let _ = writeln!(
        output,
        "| 比较 | n | 均值差 | 95% CI | d_z | p | p_adj | 显著 |"
    );
    let _ = writeln!(output, "|---|---|---|---|---|---|---|---|");
    for row in &artifact.comparisons {
        let interval = match (row.ci_low, row.ci_high) {
            (Some(low), Some(high)) => {
                format!(
                    "[{}, {}]",
                    format::fixed(Some(low), 3),
                    format::fixed(Some(high), 3)
                )
            }
            _ => "-".to_string(),
        };
        let _ = writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            row.id,
            row.n_pairs,
            format::delta(row.mean_diff),
            interval,
            format::fixed(row.effect_dz, 2),
            format::p_value(row.p_value),
            format::p_value(row.p_adjusted),
            if row.significant { "是" } else { "否" }
        );
    }
}

// =========================================================
// 第 6 节：预算轴与 oracle
// =========================================================

fn section_budget_oracle(output: &mut String, artifact: &ReportArtifact) {
    let _ = writeln!(
        output,
        "
## 6. 预算轴与 oracle
"
    );

    let budget_rows: Vec<_> = artifact
        .comparisons
        .iter()
        .filter(|row| is_budget_row(row))
        .collect();
    let _ = writeln!(
        output,
        "### 预算轴
"
    );
    if budget_rows.is_empty() {
        let _ = writeln!(output, "未执行（未提供可配对的预算对照运行）。");
    } else {
        let _ = writeln!(output, "| 比较 | n | 均值差 | p_adj | 显著 |");
        let _ = writeln!(output, "|---|---|---|---|---|");
        for row in budget_rows {
            let _ = writeln!(
                output,
                "| {} | {} | {} | {} | {} |",
                row.id,
                row.n_pairs,
                format::delta(row.mean_diff),
                format::p_value(row.p_adjusted),
                if row.significant { "是" } else { "否" }
            );
        }
    }

    let oracle_rows: Vec<_> = artifact
        .comparisons
        .iter()
        .filter(|row| pipeline_of(&row.left).as_deref() == Some("oracle-l1"))
        .collect();
    let _ = writeln!(
        output,
        "
### oracle
"
    );
    if oracle_rows.is_empty() {
        let _ = writeln!(output, "未执行（没有可配对的 oracle 运行）。");
    } else {
        let _ = writeln!(
            output,
            "| 档位 | n | 均值差（oracle - layered） | p_adj | 显著 |"
        );
        let _ = writeln!(output, "|---|---|---|---|---|");
        for row in oracle_rows {
            let _ = writeln!(
                output,
                "| {} | {} | {} | {} | {} |",
                row.tier,
                row.n_pairs,
                format::delta(row.mean_diff),
                format::p_value(row.p_adjusted),
                if row.significant { "是" } else { "否" }
            );
        }
    }
}

/// 是否为预算轴比较（同档同管线的两个预算）。
fn is_budget_row(row: &super::artifact::ComparisonRow) -> bool {
    let (Some((left_tier, left_pipeline, _)), Some((right_tier, right_pipeline, _))) = (
        split_condition_id(&row.left),
        split_condition_id(&row.right),
    ) else {
        return false;
    };
    left_tier == right_tier && left_pipeline == right_pipeline
}

/// 条件标识 → 管线。
fn pipeline_of(condition_id: &str) -> Option<String> {
    split_condition_id(condition_id).map(|(_, pipeline, _)| pipeline)
}

// =========================================================
// 第 7 节：结论
// =========================================================

fn section_hypotheses(output: &mut String, artifact: &ReportArtifact) {
    let _ = writeln!(
        output,
        "
## 7. 结论
"
    );
    for hypothesis in &artifact.hypotheses {
        let _ = writeln!(
            output,
            "- **{}** `{}`：{}",
            hypothesis.id,
            hypothesis.status.as_str(),
            hypothesis.summary
        );
        for item in &hypothesis.evidence {
            let _ = writeln!(output, "  - {item}");
        }
    }
}

// =========================================================
// 第 8 节：缺项与负面结果
// =========================================================

fn section_missing(output: &mut String, artifact: &ReportArtifact) {
    let _ = writeln!(
        output,
        "
## 8. 缺项与负面结果
"
    );
    let _ = writeln!(
        output,
        "缺项三态：未执行（not_run）/ 失败（failed）/ 不支持（unsupported）。
"
    );
    if artifact.missing.is_empty() {
        let _ = writeln!(output, "无缺项。");
    } else {
        let _ = writeln!(output, "| 范围 | 状态 | 说明 |");
        let _ = writeln!(output, "|---|---|---|");
        for entry in &artifact.missing {
            let _ = writeln!(
                output,
                "| {} | {} | {} |",
                entry.scope,
                entry.kind.as_str(),
                escape_cell(&entry.detail)
            );
        }
    }
    let unsupported: Vec<String> = artifact
        .hypotheses
        .iter()
        .filter(|hypothesis| hypothesis.status != HypothesisStatus::Supported)
        .map(|hypothesis| format!("{}（{}）", hypothesis.id, hypothesis.status.as_str()))
        .collect();
    let _ = writeln!(output);
    if unsupported.is_empty() {
        let _ = writeln!(output, "负面结果：无（全部结论均有证据支持）。");
    } else {
        let _ = writeln!(
            output,
            "负面结果：未达支持状态的结论有 {}。",
            unsupported.join("、")
        );
    }
}

// =========================================================
// 第 9 节：口径说明
// =========================================================

fn section_method(output: &mut String) {
    let _ = writeln!(
        output,
        "
## 9. 口径说明
"
    );
    let _ = writeln!(
        output,
        "- 分项要点按语料 `score_points` 自建判分，无官方判分脚本，分值与论文不可直接比较。"
    );
    let _ = writeln!(
        output,
        "- 配对键为（档位, 题号）；只取两侧都有有效 rubric 分的题（ratio 非空且判分未失败）。"
    );
    let _ = writeln!(
        output,
        "- 差值方向为 left - right；置信区间为 2000 次 bootstrap 百分位（2.5 / 97.5），固定种子；少于 10 对留空。"
    );
    let _ = writeln!(
        output,
        "- p 值为 Wilcoxon 符号秩检验（正态近似）；多重比较用 BH-FDR 跨全部比较一次校正。"
    );
    let _ = writeln!(
        output,
        "- 显著判定：p_adj < 0.05 且 95% CI 不含 0；d_z 只作效应量列输出，不参与判定。"
    );
    let _ = writeln!(
        output,
        "- H1 的跨档趋势只比较 layered 各档均值（非配对，每档均值区间由 2000 次 bootstrap 得出），不进入 BH 家族。"
    );
    let _ = writeln!(
        output,
        "- 预算单位为字符；full-context 与检索管线共用同一预算，语义是预算内最近内容，不是全量上下文。"
    );
    let _ = writeln!(
        output,
        "- 人格三维为回归线，不做显著性；缺值留空，不进主证据。"
    );
}

// =========================================================
// 辅助
// =========================================================

/// 分项平均得分率。
fn rubric_ratio(condition: &ReportCondition) -> Option<f64> {
    condition
        .rubric
        .as_ref()
        .and_then(|rubric| rubric.micro_mean_ratio)
}

/// 分项全要点命中率。
fn rubric_full_hit(condition: &ReportCondition) -> Option<f64> {
    condition
        .rubric
        .as_ref()
        .and_then(|rubric| rubric.micro_full_hit_rate)
}

/// 答题模型的人读文本（空串写 `-`）。
fn model_text(model: &str) -> &str {
    if model.is_empty() { "-" } else { model }
}

/// 列表拼接；空列表写 `无`。
fn join_or(items: &[String], separator: &str) -> String {
    if items.is_empty() {
        "无".to_string()
    } else {
        items.join(separator)
    }
}

/// markdown 单元格转义（竖线与换行）。
fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}
