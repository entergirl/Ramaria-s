//! crates/ramaria-eval/src/report/mod.rs - Ramaria 评测报告入口
//!
//! 设计特点:
//! - 只读判分产物与运行状态：报告不重算指标，只把产物编成结论视图
//! - 报告不含时间戳与机器信息，同一输入两次生成逐字节一致
//! - 缺项分三态记录（未执行 / 失败 / 不支持），不以 N/A 代替
//! - H1~H6 规则写死在代码里，输入不足一律 not_evidenced
//! - 产物先临时文件再改名，中断不会留下半份报告

mod artifact;
mod compare;
mod format;
mod hypotheses;
mod load;
mod markdown;
mod stats;

pub use artifact::{
    ComparisonRow, Hypothesis, HypothesisStatus, MissingFact, MissingKind, REPORT_ARTIFACT_KIND,
    REPORT_DIR, REPORT_FILE, REPORT_MARKDOWN_FILE, REPORT_SCHEMA_VERSION, ReportArtifact,
    ReportCondition, ReportRubric, ReportRun, ReportSource, RunRole,
};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::score::{ConditionUsage, JudgeStatus};

use load::LoadedRun;

#[cfg(test)]
mod tests;

// =========================================================
// 选项与结果
// =========================================================

/// 报告选项。
///
/// 字段约定:
/// - `run_id`: 主运行标识，也可直接给运行根目录路径；
/// - `oracle`: 压缩层 oracle 的运行标识；缺省尝试主运行根下的 `oracle` 子目录；
/// - `compare`: 额外对照运行（可重复），用于挂预算轴或其它运行；
/// - `output`: 报告输出目录；缺省为 `<run>/report`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportOptions {
    pub run_id: String,
    pub oracle: Option<String>,
    pub compare: Vec<String>,
    pub output: Option<PathBuf>,
}

/// 报告生成结果。
///
/// 字段约定:
/// - `artifact`: 已写出的报告结构；
/// - `dir` / `json` / `markdown`: 输出目录与两份产物路径。
pub struct ReportOutcome {
    pub artifact: ReportArtifact,
    pub dir: PathBuf,
    pub json: PathBuf,
    pub markdown: PathBuf,
}

impl ReportOutcome {
    /// 渲染为人读摘要（路径 + 计数 + 缺项数）。
    pub fn render(&self) -> String {
        let mut supported = 0;
        let mut partial = 0;
        let mut not_evidenced = 0;
        for hypothesis in &self.artifact.hypotheses {
            match hypothesis.status {
                HypothesisStatus::Supported => supported += 1,
                HypothesisStatus::Partial => partial += 1,
                HypothesisStatus::NotEvidenced => not_evidenced += 1,
            }
        }
        format!(
            "报告 {} 完成:\n  产物: {}\n  条件: {} / 比较: {} / 结论: supported {} / partial {} / not_evidenced {}\n  缺项: {} 条\n",
            self.artifact.run_id,
            self.dir.display(),
            self.artifact.conditions.len(),
            self.artifact.comparisons.len(),
            supported,
            partial,
            not_evidenced,
            self.artifact.missing.len()
        )
    }
}

// =========================================================
// 入口
// =========================================================

/// 生成一次运行的评测报告。
///
/// 参数:
/// - `cwd`: 当前工作目录（解析相对运行 id 用）；
/// - `options`: 报告选项。
///
/// 返回:
/// - 成功时把 `report.json` 与 `report.md` 写到输出目录；
/// - 主运行目录不存在返回 `Validation`；
/// - 判分产物内容损坏返回 `Validation`；缺产物只降级并记缺项。
pub fn run(cwd: &Path, options: &ReportOptions) -> RamariaResult<ReportOutcome> {
    let primary_root = crate::score::resolve_run_root(cwd, &options.run_id);
    if !primary_root.is_dir() {
        return Err(RamariaError::validation(format!(
            "运行目录不存在: {}（请确认运行 id）",
            primary_root.display()
        )));
    }
    let out_dir = options
        .output
        .clone()
        .unwrap_or_else(|| primary_root.join(REPORT_DIR));
    let mut missing = Vec::new();
    let primary = load::load_run(&primary_root, RunRole::Primary, &mut missing)?;

    let oracle_root = match &options.oracle {
        Some(run_id) => crate::score::resolve_run_root(cwd, run_id),
        None => primary_root.join("oracle"),
    };
    let oracle = if oracle_root.is_dir() {
        Some(load::load_run(&oracle_root, RunRole::Oracle, &mut missing)?)
    } else {
        missing.push(MissingFact::report(
            MissingKind::NotRun,
            format!("压缩层 oracle 未执行: {}", oracle_root.display()),
        ));
        None
    };

    let mut compares = Vec::new();
    for run_id in &options.compare {
        let root = crate::score::resolve_run_root(cwd, run_id);
        if !root.is_dir() {
            missing.push(MissingFact::report(
                MissingKind::NotRun,
                format!("对照运行未执行: {}（--compare {run_id}）", root.display()),
            ));
            continue;
        }
        compares.push(load::load_run(&root, RunRole::Compare, &mut missing)?);
    }

    for run in std::iter::once(&primary)
        .chain(oracle.iter())
        .chain(compares.iter())
    {
        if run
            .score
            .as_ref()
            .is_some_and(|score| matches!(score.judge, JudgeStatus::Disabled))
        {
            missing.push(MissingFact::report(
                MissingKind::Unsupported,
                format!(
                    "运行 {} 未启用 judge，分项判分列为空（本次未判分）",
                    run.run_id
                ),
            ));
        }
    }

    let conditions = build_conditions(&primary, &mut missing);
    let mut runs: Vec<&LoadedRun> = vec![&primary];
    runs.extend(oracle.iter());
    runs.extend(compares.iter());
    let pool = compare::build_pool(&runs);
    let budget_axis = compares.iter().any(|run| {
        run.items.iter().any(|item| {
            compare::BUDGET_PAIRS
                .iter()
                .any(|(left, right)| item.budget_chars == *left || item.budget_chars == *right)
        })
    });
    let comparisons =
        compare::build_comparisons(&primary, oracle.as_ref(), &pool, budget_axis, &mut missing);
    let hypotheses = hypotheses::judge(&conditions, &comparisons, &pool);

    let mut run_entries = vec![load::run_entry(&primary)];
    if let Some(oracle) = &oracle {
        run_entries.push(load::run_entry(oracle));
    }
    for run in &compares {
        run_entries.push(load::run_entry(run));
    }

    let source = ReportSource {
        items_sha256: primary
            .score
            .as_ref()
            .map(|score| score.items_sha256.clone())
            .unwrap_or_default(),
        ruleset_version: primary
            .score
            .as_ref()
            .map(|score| score.ruleset_version.clone())
            .unwrap_or_default(),
        score_schema_version: primary
            .score
            .as_ref()
            .map(|score| score.schema_version)
            .unwrap_or(0),
        judge: primary
            .score
            .as_ref()
            .map(|score| score.judge.clone())
            .unwrap_or(JudgeStatus::Disabled),
    };

    missing.sort_by(|left, right| {
        left.scope
            .cmp(&right.scope)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    missing.dedup();

    let artifact = ReportArtifact {
        artifact_kind: REPORT_ARTIFACT_KIND.to_string(),
        schema_version: REPORT_SCHEMA_VERSION,
        run_id: primary.run_id.clone(),
        source,
        runs: run_entries,
        conditions,
        comparisons,
        hypotheses,
        missing,
    };

    let json_body =
        serde_json::to_string_pretty(&artifact).map_err(|error| RamariaError::Serialization {
            context: format!("序列化评测报告失败: {}", artifact.run_id),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;
    let json = crate::score::write_atomic(&out_dir, REPORT_FILE, &format!("{json_body}\n"))?;

    let corpus = primary
        .state
        .as_ref()
        .map(|state| state.matrix.run.corpus.clone());
    let language = primary.state.as_ref().map(|state| state.language.clone());
    let markdown_body = markdown::render(&artifact, corpus.as_deref(), language.as_deref());
    let markdown = crate::score::write_atomic(&out_dir, REPORT_MARKDOWN_FILE, &markdown_body)?;

    Ok(ReportOutcome {
        artifact,
        dir: out_dir,
        json,
        markdown,
    })
}

// =========================================================
// 条件行
// =========================================================

/// 由主运行的判分汇总构造条件行。
fn build_conditions(primary: &LoadedRun, missing: &mut Vec<MissingFact>) -> Vec<ReportCondition> {
    let axes = load::condition_axes(primary);
    let usage: BTreeMap<String, ConditionUsage> = primary
        .usage
        .as_ref()
        .map(|report| {
            report
                .conditions
                .iter()
                .map(|entry| {
                    (
                        entry.condition_id.clone(),
                        ConditionUsage {
                            answer: entry.answer,
                            judge: entry.judge,
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let Some(score) = &primary.score else {
        return Vec::new();
    };

    let mut conditions = Vec::new();
    for condition in &score.conditions {
        let axis = axes.get(&condition.condition_id);
        if axis.is_none() {
            missing.push(MissingFact::condition(
                &condition.condition_id,
                MissingKind::Failed,
                "条件轴无法解析：档位 / 管线 / 预算留空",
            ));
        }
        conditions.push(ReportCondition {
            condition_id: condition.condition_id.clone(),
            tier: axis.map(|axis| axis.tier.clone()).unwrap_or_default(),
            pipeline: axis.map(|axis| axis.pipeline.clone()).unwrap_or_default(),
            budget_chars: axis.map(|axis| axis.budget_chars).unwrap_or(0),
            answer_model: axis
                .map(|axis| axis.answer_model.clone())
                .unwrap_or_default(),
            questions: condition.questions,
            scored: condition.scored,
            skipped_no_gold: condition.skipped_no_gold,
            failed_items: condition.failed_items,
            judge_failed: condition.judge_failed,
            rubric: condition.rubric.as_ref().map(ReportRubric::from_summary),
            retrieval: condition.retrieval.clone(),
            persona: condition.persona.clone(),
            credibility: condition.credibility.clone(),
            usage: usage.get(&condition.condition_id).copied(),
        });
    }
    conditions
}
