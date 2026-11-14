//! crates/ramaria-eval/src/tui/console/matrix.rs - Ramaria 评测台矩阵选择与预览
//!
//! 设计特点:
//! - 只读扫描 `.eval/matrix*.toml`：文件名前缀与扩展名同时匹配，按名称排序
//! - 预览复用运行器入口：矩阵校验与档位加载同源，缺档位直接禁启动
//! - 计划规模取自 `plan_counts`：抽样与上限口径与运行器一致
//! - 耗时估计取历史运行中位数：只读运行根一级目录，不为预览解析逐题产物
//! - 无历史时用固定粗估并明确标注，不写死任何一次真实运行的数值

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Deserialize;

use crate::run::{AnswerModelKind, PlanCounts, RunMatrix, load_tiers, plan_counts};

/// 评测根目录名。
const EVAL_DIR: &str = ".eval";

/// 运行根目录（相对评测根）。
const RUNS_DIR: &str = "runs";

/// 矩阵文件名前缀。
const MATRIX_PREFIX: &str = "matrix";

/// 矩阵文件扩展名。
const MATRIX_EXT: &str = "toml";

/// 无历史运行时的单题粗估耗时（毫秒）。
pub(crate) const DEFAULT_PACE_MS: f64 = 1_500.0;

// =========================================================
// 历史均速
// =========================================================

/// 历史运行的单题均速。
///
/// 字段约定:
/// - `per_item_ms`: 中位数均速；
/// - `runs`: 参与统计的运行数（0 表示没有任何可用的历史）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Pace {
    pub(crate) per_item_ms: f64,
    pub(crate) runs: usize,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            per_item_ms: DEFAULT_PACE_MS,
            runs: 0,
        }
    }
}

impl Pace {
    /// 均速说明（无历史时明确标注粗估）。
    pub(crate) fn label(&self) -> String {
        let seconds = self.per_item_ms / 1000.0;
        if self.runs == 0 {
            format!("粗估（无历史运行）{seconds:.1} 秒/题")
        } else {
            format!("按历史 {} 次运行均速 {seconds:.1} 秒/题", self.runs)
        }
    }

    /// 按计划条目数估算耗时（毫秒）。
    pub(crate) fn eta_ms(&self, executions: usize) -> u64 {
        (self.per_item_ms * executions as f64).round() as u64
    }
}

/// 估算用到的最小状态列（忽略运行根其余字段）。
#[derive(Debug, Deserialize)]
struct PaceFile {
    elapsed_ms: u64,
    #[serde(default)]
    conditions: Vec<PaceCondition>,
}

/// 单条件计数（估算只用完成与失败）。
#[derive(Debug, Deserialize)]
struct PaceCondition {
    #[serde(default)]
    completed: usize,
    #[serde(default)]
    failed: usize,
}

/// 从历史运行估计单题均速：各运行 `elapsed_ms / (completed + failed)` 的中位数。
///
/// 参数:
/// - `runs`: 运行根目录（`.eval/runs`）。
///
/// 返回:
/// - 至少一次运行有定案条目时返回中位数与样本数；
/// - 目录缺失、没有可用运行或全部运行零条目时返回默认粗估（`runs = 0`）。
pub(crate) fn history_pace(runs: &Path) -> Pace {
    let mut paces: Vec<f64> = Vec::new();
    let Ok(entries) = std::fs::read_dir(runs) else {
        return Pace::default();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path.join("run.json")) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<PaceFile>(&text) else {
            continue;
        };
        let done: usize = file
            .conditions
            .iter()
            .map(|condition| condition.completed + condition.failed)
            .sum();
        if done == 0 {
            continue;
        }
        paces.push(file.elapsed_ms as f64 / done as f64);
    }
    if paces.is_empty() {
        return Pace::default();
    }
    paces.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let middle = paces.len() / 2;
    let median = if paces.len().is_multiple_of(2) {
        (paces[middle - 1] + paces[middle]) / 2.0
    } else {
        paces[middle]
    };
    Pace {
        per_item_ms: median,
        runs: paces.len(),
    }
}

// =========================================================
// 预览
// =========================================================

/// 一个矩阵的可展示信息。
///
/// 字段约定:
/// - 轴字段按矩阵原文展开，不在此处改写取值；
/// - `counts`: 档位加载成功时的计划规模；为 `None` 表示不可启动；
/// - `error`: 解析或档位校验失败原因，展示后禁止启动。
#[derive(Debug, Default)]
pub(crate) struct MatrixPreview {
    pub(crate) path: PathBuf,
    pub(crate) corpus: String,
    pub(crate) language: String,
    pub(crate) tiers: Vec<String>,
    pub(crate) pipelines: Vec<String>,
    pub(crate) budgets: Vec<u32>,
    pub(crate) models: Vec<String>,
    pub(crate) strict_isolation: bool,
    pub(crate) question_limit: Option<usize>,
    pub(crate) question_sample: Option<usize>,
    pub(crate) counts: Option<PlanCounts>,
    pub(crate) pace: Pace,
    pub(crate) error: Option<String>,
}

impl MatrixPreview {
    /// 从已加载的矩阵填充轴字段。
    fn from_matrix(path: &Path, matrix: &RunMatrix) -> Self {
        let models = matrix
            .answer_models
            .iter()
            .map(|model| {
                let kind = match model.kind {
                    AnswerModelKind::Local => "本地",
                    AnswerModelKind::Cloud => "云端",
                };
                if model.model.trim().is_empty() {
                    format!("{kind} 自动选择")
                } else {
                    format!("{kind} {}", model.model)
                }
            })
            .collect();
        Self {
            path: path.to_path_buf(),
            corpus: matrix.run.corpus.clone(),
            language: matrix.run.language.clone(),
            tiers: matrix.run.tiers.clone(),
            pipelines: matrix
                .run
                .pipelines
                .iter()
                .map(|pipeline| pipeline.as_str().to_string())
                .collect(),
            budgets: matrix.run.budgets.clone(),
            models,
            strict_isolation: matrix.run.strict_isolation,
            question_limit: matrix.run.question_limit,
            question_sample: matrix.run.question_sample,
            counts: None,
            pace: Pace::default(),
            error: None,
        }
    }

    /// 预计耗时（毫秒）；计划规模未知时为 `None`。
    pub(crate) fn eta_ms(&self) -> Option<u64> {
        self.counts
            .as_ref()
            .map(|counts| self.pace.eta_ms(counts.executions))
    }
}

/// 读取一个矩阵并校验档位；失败原因写进预览，不抛出。
fn preview(path: &Path, root: &Path) -> MatrixPreview {
    let matrix = match RunMatrix::load(path) {
        Ok(matrix) => matrix,
        Err(error) => {
            return MatrixPreview {
                path: path.to_path_buf(),
                error: Some(error.to_string()),
                ..MatrixPreview::default()
            };
        }
    };
    let mut preview = MatrixPreview::from_matrix(path, &matrix);
    preview.pace = history_pace(&root.join(EVAL_DIR).join(RUNS_DIR));
    match load_tiers(root, &matrix, None) {
        Ok(tiers) => preview.counts = Some(plan_counts(&matrix, &tiers)),
        Err(error) => preview.error = Some(error.to_string()),
    }
    preview
}

// =========================================================
// 选择
// =========================================================

/// 矩阵屏状态。
///
/// 字段约定:
/// - `entries`: 扫描到的矩阵文件（按名称排序）；
/// - `selected`: 当前选中项下标；
/// - `preview`: 当前选中项的预览，扫描失败时为 `None`；
/// - `error`: 扫描层面的提示（无矩阵文件等）。
#[derive(Debug, Default)]
pub(crate) struct MatrixState {
    pub(crate) entries: Vec<PathBuf>,
    pub(crate) selected: usize,
    pub(crate) preview: Option<MatrixPreview>,
    pub(crate) error: Option<String>,
}

impl MatrixState {
    /// 重新扫描矩阵目录并预览首项。
    pub(crate) fn refresh(&mut self, root: &Path) {
        self.entries = discover(root);
        self.selected = 0;
        self.error = if self.entries.is_empty() {
            Some(format!(
                "未找到矩阵: {}（新建 matrix*.toml 后按 r 重扫）",
                root.join(EVAL_DIR).display()
            ))
        } else {
            None
        };
        self.preview = self.entries.first().map(|path| preview(path, root));
    }

    /// 按偏移移动选择并刷新预览（循环）。
    pub(crate) fn select(&mut self, offset: isize, root: &Path) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as isize;
        self.selected = ((self.selected as isize + offset).rem_euclid(len)) as usize;
        self.preview = self
            .entries
            .get(self.selected)
            .map(|path| preview(path, root));
    }

    /// 启动参数：矩阵原文路径 + 自动生成的输出根。
    ///
    /// 返回:
    /// - 预览存在、无错误且有计划规模时返回 `(matrix, output)`；
    /// - 否则返回 `None`（界面据此禁用启动）。
    pub(crate) fn start_request(&self, root: &Path) -> Option<(PathBuf, PathBuf)> {
        let preview = self.preview.as_ref()?;
        if preview.error.is_some() || preview.counts.is_none() {
            return None;
        }
        let stamp = Utc::now().format("%Y%m%d-%H%M%S");
        let output = root
            .join(EVAL_DIR)
            .join(RUNS_DIR)
            .join(format!("run-{stamp}"));
        Some((preview.path.clone(), output))
    }
}

/// 扫描评测根下文件名以 `matrix` 开头、扩展名为 `.toml` 的文件。
fn discover(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(EVAL_DIR)) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && matches_matrix(path))
        .collect();
    paths.sort();
    paths
}

/// 文件名是否形如 `matrix*.toml`。
fn matches_matrix(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.starts_with(MATRIX_PREFIX) && lower.ends_with(&format!(".{MATRIX_EXT}"))
}

// =========================================================
// 格式化
// =========================================================

/// 毫秒转人读时长（小时 / 分 / 秒三档）。
pub(crate) fn format_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours} 小时 {minutes} 分")
    } else if minutes > 0 {
        format!("{minutes} 分 {seconds} 秒")
    } else {
        format!("{seconds} 秒")
    }
}
