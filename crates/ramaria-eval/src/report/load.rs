//! crates/ramaria-eval/src/report/load.rs - Ramaria 评测报告输入装载
//!
//! 设计特点:
//! - 报告只读判分产物与运行状态，不回读语料、工作库与逐题原文
//! - 每个产物单独判定：缺失记缺项并降级，内容损坏按结构化错误返回
//! - 条件轴优先取冻结矩阵，矩阵缺失时回退逐题产物与条件标识
//! - 用量属运行遥测，缺失只留空值，不阻断报告
//! - 每次运行一个装载结果，角色由调用方指定

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::run::{RUN_FILE, RunState};
use crate::score::{
    SCORE_DIR, SCORE_FILE, SCORED_FILE, ScoreReport, ScoredItem, USAGE_FILE, UsageReport,
};

use super::artifact::{MissingFact, MissingKind, ReportRun, RunRole};

// =========================================================
// 装载结果
// =========================================================

/// 一次已装载的运行。
///
/// 字段约定:
/// - `role` / `root` / `run_id`: 角色、运行根与标识；
/// - `state`: 运行状态；缺失时档位与矩阵信息由逐题产物推导；
/// - `score`: 条件汇总；缺失时条件与结论整体降级；
/// - `items`: 逐题判分（只用于配对比较）；
/// - `usage`: 条件用量；缺失只留空值。
pub(crate) struct LoadedRun {
    pub role: RunRole,
    pub root: PathBuf,
    pub run_id: String,
    pub state: Option<RunState>,
    pub score: Option<ScoreReport>,
    pub items: Vec<ScoredItem>,
    pub usage: Option<UsageReport>,
}

/// 条件轴（报告条件行与比较定位用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConditionAxis {
    pub tier: String,
    pub pipeline: String,
    pub budget_chars: u32,
    pub answer_model: String,
}

// =========================================================
// 装载
// =========================================================

/// 读取一次运行的全部报告输入。
///
/// 参数:
/// - `root`: 运行根目录（须已存在）；
/// - `role`: 运行角色；
/// - `missing`: 缺项收集器。
///
/// 返回:
/// - 运行状态、判分汇总、逐题判分与用量各自独立降级；
/// - 判分产物内容损坏（非本产品产物或字节不可解析）返回 `Validation`。
pub(crate) fn load_run(
    root: &Path,
    role: RunRole,
    missing: &mut Vec<MissingFact>,
) -> RamariaResult<LoadedRun> {
    let state = read_state(root, missing);
    let score = read_score(root, missing)?;
    let items = read_scored(root, missing)?;
    let usage = read_usage(root, missing)?;
    let run_id = score
        .as_ref()
        .map(|report| report.run_id.clone())
        .or_else(|| state.as_ref().map(|state| state.run_id.clone()))
        .or_else(|| {
            root.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| root.display().to_string());
    Ok(LoadedRun {
        role,
        root: root.to_path_buf(),
        run_id,
        state,
        score,
        items,
        usage,
    })
}

/// 读取运行状态；缺失或不可解析时记缺项并返回 `None`。
fn read_state(root: &Path, missing: &mut Vec<MissingFact>) -> Option<RunState> {
    let path = root.join(RUN_FILE);
    if !path.is_file() {
        missing.push(MissingFact::report(
            MissingKind::Failed,
            format!(
                "运行状态不存在: {}（档位与矩阵信息改由逐题产物推导）",
                path.display()
            ),
        ));
        return None;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            missing.push(MissingFact::report(
                MissingKind::Failed,
                format!("读取运行状态失败: {}: {error}", path.display()),
            ));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(state) => Some(state),
        Err(error) => {
            missing.push(MissingFact::report(
                MissingKind::Failed,
                format!("解析运行状态失败: {}: {error}", path.display()),
            ));
            None
        }
    }
}

/// 读取条件汇总；缺失记缺项，内容非法返回 `Validation`。
fn read_score(root: &Path, missing: &mut Vec<MissingFact>) -> RamariaResult<Option<ScoreReport>> {
    let path = root.join(SCORE_DIR).join(SCORE_FILE);
    if !path.is_file() {
        missing.push(MissingFact::report(
            MissingKind::Failed,
            format!(
                "判分产物不存在: {}（先运行 ramaria eval score）",
                path.display()
            ),
        ));
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(format!("读取判分产物失败: {}", path.display()), Some(error))
    })?;
    let report: ScoreReport = serde_json::from_str(&text).map_err(|error| {
        RamariaError::validation(format!("解析判分产物失败: {}: {error}", path.display()))
    })?;
    if report.artifact_kind != crate::score::SCORE_ARTIFACT_KIND {
        return Err(RamariaError::validation(format!(
            "判分产物类型不匹配: {}（期望 {}，实际 {}）",
            path.display(),
            crate::score::SCORE_ARTIFACT_KIND,
            report.artifact_kind
        )));
    }
    Ok(Some(report))
}

/// 读取逐题判分；缺失记缺项，坏行返回 `Validation`。
fn read_scored(root: &Path, missing: &mut Vec<MissingFact>) -> RamariaResult<Vec<ScoredItem>> {
    let path = root.join(SCORE_DIR).join(SCORED_FILE);
    if !path.is_file() {
        missing.push(MissingFact::report(
            MissingKind::Failed,
            format!(
                "逐题判分产物不存在: {}（报告无法做配对比较）",
                path.display()
            ),
        ));
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(
            format!("读取逐题判分产物失败: {}", path.display()),
            Some(error),
        )
    })?;
    let mut items = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let item: ScoredItem = serde_json::from_str(line).map_err(|error| {
            RamariaError::validation(format!(
                "解析逐题判分产物失败: {} 第 {} 行: {error}",
                path.display(),
                index + 1
            ))
        })?;
        items.push(item);
    }
    Ok(items)
}

/// 读取用量产物；缺失只返回 `None`，不可解析记缺项后返回 `None`。
fn read_usage(root: &Path, missing: &mut Vec<MissingFact>) -> RamariaResult<Option<UsageReport>> {
    let path = root.join(SCORE_DIR).join(USAGE_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            missing.push(MissingFact::report(
                MissingKind::Failed,
                format!("读取用量产物失败: {}: {error}", path.display()),
            ));
            return Ok(None);
        }
    };
    let report: UsageReport = match serde_json::from_str(&text) {
        Ok(report) => report,
        Err(error) => {
            missing.push(MissingFact::report(
                MissingKind::Failed,
                format!("解析用量产物失败: {}: {error}", path.display()),
            ));
            return Ok(None);
        }
    };
    if report.artifact_kind != crate::score::USAGE_ARTIFACT_KIND {
        missing.push(MissingFact::report(
            MissingKind::Failed,
            format!(
                "用量产物类型不匹配: {}（期望 {}，实际 {}）",
                path.display(),
                crate::score::USAGE_ARTIFACT_KIND,
                report.artifact_kind
            ),
        ));
        return Ok(None);
    }
    Ok(Some(report))
}

// =========================================================
// 条件轴与运行摘要
// =========================================================

/// 条件标识 → 条件轴。
///
/// 说明: 依次以冻结矩阵、逐题产物、条件标识解析为来源，先命中者优先。
pub(crate) fn condition_axes(run: &LoadedRun) -> BTreeMap<String, ConditionAxis> {
    let mut axes = BTreeMap::new();
    if let Some(state) = &run.state {
        for condition in state.matrix.conditions() {
            axes.insert(
                condition.id,
                ConditionAxis {
                    tier: condition.tier,
                    pipeline: condition.pipeline.as_str().to_string(),
                    budget_chars: condition.budget_chars,
                    answer_model: condition.answer_model.model,
                },
            );
        }
    }
    for item in &run.items {
        axes.entry(item.condition_id.clone())
            .or_insert_with(|| ConditionAxis {
                tier: item.tier.clone(),
                pipeline: item.pipeline.clone(),
                budget_chars: item.budget_chars,
                answer_model: item.answer_model.clone(),
            });
    }
    if let Some(score) = &run.score {
        for condition in &score.conditions {
            axes.entry(condition.condition_id.clone())
                .or_insert_with(|| {
                    let (tier, pipeline, budget_chars) =
                        super::compare::split_condition_id(&condition.condition_id)
                            .unwrap_or_else(|| (String::new(), String::new(), 0));
                    ConditionAxis {
                        tier,
                        pipeline,
                        budget_chars,
                        answer_model: String::new(),
                    }
                });
        }
    }
    axes
}

/// 一次运行的报告摘要行。
pub(crate) fn run_entry(run: &LoadedRun) -> ReportRun {
    let (tiers, pipelines, budgets, question_sample) = match &run.state {
        Some(state) => (
            state.matrix.run.tiers.clone(),
            state
                .matrix
                .run
                .pipelines
                .iter()
                .map(|pipeline| pipeline.as_str().to_string())
                .collect(),
            state.matrix.run.budgets.clone(),
            state.matrix.run.question_sample,
        ),
        None => {
            let tiers: Vec<String> = run
                .items
                .iter()
                .map(|item| item.tier.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let pipelines: Vec<String> = run
                .items
                .iter()
                .map(|item| item.pipeline.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let budgets: Vec<u32> = run
                .items
                .iter()
                .map(|item| item.budget_chars)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            (tiers, pipelines, budgets, None)
        }
    };
    ReportRun {
        role: run.role,
        run_id: run.run_id.clone(),
        root: run.root.to_string_lossy().to_string(),
        tiers,
        pipelines,
        budgets,
        question_sample,
    }
}
