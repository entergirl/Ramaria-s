//! crates/ramaria-eval/src/tui/console/running.rs - Ramaria 评测台运行中屏状态
//!
//! 设计特点:
//! - 只消费运行事件：不读 run.json、不重算产物，计数以事件携带的当前值为准
//! - 计数按条件分表保存：同一题重试成功后失败数随事件回退，界面不自行累加
//! - 事件尾固定 8 条：长跑只保留最近进度，内存不随运行时长增长
//! - 结束态冻结已用时间：中断或完成后不再随时间增长
//! - 剩余估计按同一进度口径计算：没有完成样本时不给出数字

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::run::{ItemEvent, RunReport, RunStarted, RunStatus};

use super::matrix::format_duration;

/// 事件尾保留条数。
pub(crate) const TAIL_LIMIT: usize = 8;

// =========================================================
// 条件表
// =========================================================

/// 一个条件的实时计数。
///
/// 字段约定:
/// - `planned`: 计划题数（来自 `Started`）；
/// - `pending`: 本次待跑题数（来自 `ConditionStarted`，续跑后可能小于计划数）；
/// - `completed` / `failed`: 该条件当前计数（以事件携带值为准）；
/// - `active`: 是否为当前正在执行的条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunningCondition {
    pub(crate) id: String,
    pub(crate) planned: usize,
    pub(crate) pending: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    pub(crate) active: bool,
}

// =========================================================
// 运行状态
// =========================================================

/// 运行中屏的全部状态。
#[derive(Debug, Default)]
pub(crate) struct RunningState {
    started_at: Option<Instant>,
    finished_at: Option<Instant>,
    run_id: String,
    root: Option<PathBuf>,
    total_conditions: usize,
    total_items: usize,
    total_executions: usize,
    conditions: Vec<RunningCondition>,
    condition_index: Option<usize>,
    tail: VecDeque<String>,
    warnings: usize,
    cancel_requested: bool,
    finished: bool,
    outcome: Option<RunStatus>,
}

impl RunningState {
    /// 新一次运行：清空上次计数并记录起点时刻。
    pub(crate) fn prepare(&mut self, output: &Path) {
        *self = Self {
            started_at: Some(Instant::now()),
            root: Some(output.to_path_buf()),
            ..Self::default()
        };
    }

    /// 应用运行开始事件：记录口径、计划规模与续跑初始计数。
    pub(crate) fn started(&mut self, started: RunStarted) {
        self.run_id = started.run_id;
        self.root = Some(started.root);
        self.total_conditions = started.total_conditions;
        self.total_items = started.total_items;
        self.total_executions = started.total_executions;
        self.conditions = started
            .conditions
            .into_iter()
            .map(|plan| RunningCondition {
                id: plan.id,
                planned: plan.total,
                pending: plan.total.saturating_sub(plan.completed + plan.failed),
                completed: plan.completed,
                failed: plan.failed,
                active: false,
            })
            .collect();
    }

    /// 应用条件开始事件。
    pub(crate) fn condition_started(
        &mut self,
        index: usize,
        total: usize,
        id: String,
        items: usize,
    ) {
        self.total_conditions = total;
        self.condition_index = Some(index);
        if let Some(entry) = self.conditions.iter_mut().find(|entry| entry.id == id) {
            entry.pending = items;
            entry.active = true;
        } else {
            self.conditions.push(RunningCondition {
                id: id.clone(),
                planned: items,
                pending: items,
                completed: 0,
                failed: 0,
                active: true,
            });
        }
        self.push_tail(format!(
            "开始条件 {}/{} {id}（本次 {items} 题）",
            index + 1,
            total
        ));
    }

    /// 应用单题写出事件。
    pub(crate) fn item_done(&mut self, event: ItemEvent) {
        if let Some(entry) = self
            .conditions
            .iter_mut()
            .find(|entry| entry.id == event.condition_id)
        {
            entry.planned = event.total;
            entry.completed = event.completed;
            entry.failed = event.failed;
            entry.active = true;
        }
        let mark = match (&event.error_kind, event.success) {
            (_, true) => "ok".to_string(),
            (Some(kind), false) => format!("failed:{kind}"),
            (None, false) => "failed".to_string(),
        };
        self.push_tail(format!(
            "[{mark}] {} {}ms（第 {} 次）",
            event.question_id, event.elapsed_ms, event.attempts
        ));
    }

    /// 应用条件结束事件。
    pub(crate) fn condition_done(&mut self, id: &str, completed: usize, failed: usize) {
        if let Some(entry) = self.conditions.iter_mut().find(|entry| entry.id == id) {
            entry.completed = completed;
            entry.failed = failed;
            entry.active = false;
        }
        self.push_tail(format!("条件 {id} 结束：完成 {completed} / 失败 {failed}"));
    }

    /// 应用条件级提示事件。
    pub(crate) fn warning(&mut self, condition: Option<String>, message: String) {
        self.warnings += 1;
        match condition {
            Some(condition) => self.push_tail(format!("[warn] {condition}: {message}")),
            None => self.push_tail(format!("[warn] {message}")),
        }
    }

    /// 收尾：以报告状态定案，冻结已用时间。
    pub(crate) fn finish(&mut self, report: &RunReport) {
        self.finished_at = Some(Instant::now());
        self.finished = true;
        self.outcome = Some(report.status);
        self.run_id = report.run_id.clone();
        self.root = Some(report.root.clone());
        self.total_executions = report.total;
        self.conditions
            .iter_mut()
            .for_each(|entry| entry.active = false);
        let line = match report.status {
            RunStatus::Completed => "运行完成".to_string(),
            RunStatus::Aborted => "已中断（已写产物保留，可续跑）".to_string(),
            RunStatus::Running => "运行结束".to_string(),
        };
        self.push_tail(line);
    }

    /// 运行入口返回错误：标记结束并保留错误原文。
    pub(crate) fn fail(&mut self, message: &str) {
        self.finished_at = Some(Instant::now());
        self.finished = true;
        self.push_tail(format!("运行失败: {message}"));
    }

    /// 请求中断：界面提示等待在飞题完成。
    pub(crate) fn request_cancel(&mut self) {
        self.cancel_requested = true;
    }

    // ===== 读取 =====

    /// 是否已结束（完成、中断或失败）。
    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    /// 是否已请求中断。
    pub(crate) fn is_cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    /// 运行标识（未知时为空串）。
    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    /// 运行根（未知时为 `None`）。
    pub(crate) fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// 计划题目数。
    pub(crate) fn total_items(&self) -> usize {
        self.total_items
    }

    /// 计划条目数。
    pub(crate) fn total_executions(&self) -> usize {
        self.total_executions
    }

    /// 条件总数。
    pub(crate) fn total_conditions(&self) -> usize {
        self.total_conditions
    }

    /// 条件表。
    pub(crate) fn conditions(&self) -> &[RunningCondition] {
        &self.conditions
    }

    /// 当前条件序号（从 0 起）。
    pub(crate) fn condition_index(&self) -> Option<usize> {
        self.condition_index
    }

    /// 最近事件（最多 [`TAIL_LIMIT`] 条）。
    pub(crate) fn tail(&self) -> impl Iterator<Item = &str> {
        self.tail.iter().map(String::as_str)
    }

    /// 告警条数。
    pub(crate) fn warnings(&self) -> usize {
        self.warnings
    }

    /// 汇总完成数（各条件之和）。
    pub(crate) fn completed(&self) -> usize {
        self.conditions.iter().map(|entry| entry.completed).sum()
    }

    /// 汇总失败数（各条件之和）。
    pub(crate) fn failed(&self) -> usize {
        self.conditions.iter().map(|entry| entry.failed).sum()
    }

    /// 已定案条目数。
    pub(crate) fn done_total(&self) -> usize {
        self.completed() + self.failed()
    }

    /// 已用时间；未开始时为零。
    pub(crate) fn elapsed(&self) -> Duration {
        match (self.started_at, self.finished_at) {
            (Some(start), Some(end)) => end.duration_since(start),
            (Some(start), None) => start.elapsed(),
            _ => Duration::ZERO,
        }
    }

    /// 已用时间的人读文本。
    pub(crate) fn elapsed_text(&self) -> String {
        format_duration(self.elapsed().as_millis() as u64)
    }

    /// 剩余估计；没有完成样本或已结束时为 `None`。
    pub(crate) fn eta(&self) -> Option<Duration> {
        let done = self.done_total();
        if done == 0 || self.finished || self.total_executions == 0 {
            return None;
        }
        let elapsed = self.elapsed().as_millis() as u64;
        let remaining = self.total_executions.saturating_sub(done) as u64;
        Some(Duration::from_millis(
            elapsed.saturating_mul(remaining) / done as u64,
        ))
    }

    /// 剩余估计的人读文本。
    pub(crate) fn eta_text(&self) -> String {
        match self.eta() {
            Some(eta) => format_duration(eta.as_millis() as u64),
            None => "待估算".to_string(),
        }
    }

    /// 状态文案（标题行使用）。
    pub(crate) fn status_text(&self) -> &'static str {
        if self.finished {
            return match self.outcome {
                Some(RunStatus::Completed) => "已完成",
                Some(RunStatus::Aborted) => "已中断",
                _ => "已结束",
            };
        }
        if self.cancel_requested {
            "中断中…等待在飞题完成"
        } else {
            "运行中"
        }
    }

    // ===== 内部 =====

    /// 追加一条事件并裁掉超出上限的旧行。
    fn push_tail(&mut self, line: String) {
        self.tail.push_back(line);
        while self.tail.len() > TAIL_LIMIT {
            self.tail.pop_front();
        }
    }
}
