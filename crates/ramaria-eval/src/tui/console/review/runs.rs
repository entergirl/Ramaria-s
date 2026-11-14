//! crates/ramaria-eval/src/tui/console/review/runs.rs - Ramaria 审查运行列表
//!
//! 设计特点:
//! - 只扫描 `.eval/runs` 一级目录与 `oracle/` 子目录，不递归
//! - 摘要只读 `run.json` 的计数与矩阵轴，不触碰逐题产物
//! - 解析失败的运行跳过并汇总条数，不阻断列表
//! - 按最近落盘时间降序，同刻按显示路径稳定排序
//! - 刷新时按运行根保持选中项

use std::path::{Path, PathBuf};

use crate::run::{RUN_FILE, RunState, RunStatus};

use super::index;

// =========================================================
// 运行列表
// =========================================================

/// 运行目录（相对评测根）。
const RUNS_DIR: &str = ".eval/runs";

/// 运行列表中的一项。
///
/// 字段约定:
/// - `display_id`: 相对 `.eval/runs` 的路径（oracle 运行写成 `父目录/oracle`）；
/// - `root`: 运行根；
/// - `status`: `run.json` 记录的状态；
/// - `conditions` / `completed` / `failed`: 条件数与定案计数；
/// - `updated_at`: 最近落盘时间（排序与展示用）；
/// - `tiers` / `pipelines` / `budgets`: 矩阵轴；
/// - `scored`: 是否已有判分产物（`score/score.json` 或 `score/scored.jsonl`）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunSummary {
    pub(crate) display_id: String,
    pub(crate) root: PathBuf,
    pub(crate) status: RunStatus,
    pub(crate) conditions: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    pub(crate) updated_at: String,
    pub(crate) tiers: Vec<String>,
    pub(crate) pipelines: Vec<String>,
    pub(crate) budgets: Vec<u32>,
    pub(crate) scored: bool,
}

/// 运行列表状态。
#[derive(Debug, Default)]
pub(crate) struct RunsState {
    pub(crate) runs: Vec<RunSummary>,
    pub(crate) selected: usize,
    pub(crate) error: Option<String>,
}

impl RunsState {
    /// 重新扫描运行目录；选中项尽量保持。
    pub(crate) fn refresh(&mut self, root: &Path) {
        let keep = self.runs.get(self.selected).map(|run| run.root.clone());
        let (runs, error) = discover_runs(root);
        self.selected = keep
            .and_then(|root| runs.iter().position(|run| run.root == root))
            .unwrap_or(0);
        self.runs = runs;
        self.error = error;
    }

    /// 移动选中项（越界钳制）。
    pub(crate) fn select(&mut self, delta: isize) {
        if self.runs.is_empty() {
            return;
        }
        let last = self.runs.len() - 1;
        self.selected = if delta >= 0 {
            (self.selected + delta as usize).min(last)
        } else {
            self.selected.saturating_sub((-delta) as usize)
        };
    }

    /// 当前选中的运行。
    pub(crate) fn selected(&self) -> Option<&RunSummary> {
        self.runs.get(self.selected)
    }

    /// 可见窗口；只按窗口返回下标区间。
    pub(crate) fn window(&self, height: usize) -> std::ops::Range<usize> {
        let len = self.runs.len();
        let height = height.max(1);
        if len <= height {
            return 0..len;
        }
        let start = if self.selected < height {
            0
        } else {
            self.selected + 1 - height
        };
        start..(start + height).min(len)
    }
}

/// 扫描运行根：`.eval/runs/*/run.json` 与 `.eval/runs/*/oracle/run.json`。
///
/// 参数:
/// - `cwd`: 评测根（当前工作目录）。
///
/// 返回:
/// - 按 `updated_at` 降序排列的运行摘要；
/// - 目录缺失或部分 `run.json` 无法解析时的提示文本。
pub(crate) fn discover_runs(cwd: &Path) -> (Vec<RunSummary>, Option<String>) {
    let dir = cwd.join(RUNS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (
            Vec::new(),
            Some(format!("未找到运行记录: {}（先运行矩阵）", dir.display())),
        );
    };

    let mut runs = Vec::new();
    let mut skipped = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        match read_summary(&path, &name) {
            Some(summary) => runs.push(summary),
            None if path.join(RUN_FILE).is_file() => skipped += 1,
            None => {}
        }
        let oracle = path.join("oracle");
        if oracle.is_dir() {
            match read_summary(&oracle, &format!("{name}/oracle")) {
                Some(summary) => runs.push(summary),
                None if oracle.join(RUN_FILE).is_file() => skipped += 1,
                None => {}
            }
        }
    }

    runs.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.display_id.cmp(&b.display_id))
    });
    let error = if skipped > 0 {
        Some(format!("{skipped} 个运行状态无法解析，已跳过"))
    } else {
        None
    };
    (runs, error)
}

/// 读取一个运行根的摘要；不可解析返回 `None`。
fn read_summary(root: &Path, display_id: &str) -> Option<RunSummary> {
    let text = std::fs::read_to_string(root.join(RUN_FILE)).ok()?;
    let state: RunState = serde_json::from_str(&text).ok()?;
    let completed = state.conditions.iter().map(|entry| entry.completed).sum();
    let failed = state.conditions.iter().map(|entry| entry.failed).sum();
    Some(RunSummary {
        display_id: display_id.to_string(),
        root: root.to_path_buf(),
        status: state.status,
        conditions: state.conditions.len(),
        completed,
        failed,
        updated_at: state.updated_at,
        tiers: state.tiers.iter().map(|tier| tier.id.clone()).collect(),
        pipelines: state
            .matrix
            .run
            .pipelines
            .iter()
            .map(|pipeline| pipeline.as_str().to_string())
            .collect(),
        budgets: state.matrix.run.budgets,
        scored: index::score_present(root),
    })
}
