//! crates/ramaria-eval/src/run/state.rs - Ramaria 评测运行状态
//!
//! 设计特点:
//! - 状态文件只记运行口径与计数，不记题目正文与回复
//! - 重入时比对矩阵序列化值：矩阵变了就拒绝复用旧目录，避免口径混用
//! - 计数按 `(条件, 题目)` 重建，同一题多行以最后一行为准，完成与失败互斥
//! - 逐题产物只追加不重写，坏行带行号报错，便于中断后就地修复
//! - 落盘先写临时文件再改名，中断不会留下半份 JSON

use std::collections::HashMap;
use std::path::Path;

use chrono::{SecondsFormat, Utc};
use ramaria_core::error::{RamariaError, RamariaResult};
use serde::{Deserialize, Serialize};

use super::matrix::RunMatrix;
use super::workspace::RUN_FILE;

#[cfg(test)]
mod tests;

/// 状态产物类型标识。
pub const RUN_ARTIFACT_KIND: &str = "ramaria_eval_run";

/// 状态文件自身的 schema 版本。
pub const RUN_SCHEMA_VERSION: u32 = 1;

// =========================================================
// 状态结构
// =========================================================

/// 运行状态机取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// 运行中（含中断后重入）
    Running,
    /// 全部条件跑完
    Completed,
    /// 主动中止（保留已写产物）
    Aborted,
}

/// 状态文件引用的一个基线档位。
///
/// 字段约定:
/// - `id`: 档位标识；
/// - `dir`: 档位产物目录（字符串，避免绝对路径换机不可读时阻断解析）；
/// - `manifest_sha256`: 清单文件内容摘要，用于判断档位是否被重建；
/// - `rows`: 该档位题目数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTierRef {
    pub id: String,
    pub dir: String,
    pub manifest_sha256: String,
    pub rows: usize,
}

/// 单个条件的计数。
///
/// 字段约定:
/// - `id`: 条件标识；
/// - `total`: 该条件计划题目数；
/// - `completed` / `failed`: 互斥计数，按最后一行为准。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionState {
    pub id: String,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
}

/// 一次运行的持久化状态（`run.json`）。
///
/// 字段约定:
/// - `artifact_kind` / `schema_version`: 产物识别与版本；
/// - `run_id`: 运行根目录名；
/// - `created_at` / `updated_at`: UTC RFC3339（秒级，Z 结尾）；
/// - `matrix`: 冻结的矩阵定义，重入时逐值比对；
/// - `language`: 语料语言标签；
/// - `tiers` / `conditions`: 档位引用与条件计数；
/// - `elapsed_ms`: 累计耗时；
/// - `warnings`: 不阻断运行的降级与口径提示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub artifact_kind: String,
    pub schema_version: u32,
    pub run_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: RunStatus,
    pub matrix: RunMatrix,
    pub language: String,
    pub tiers: Vec<RunTierRef>,
    pub conditions: Vec<ConditionState>,
    pub elapsed_ms: u64,
    pub warnings: Vec<String>,
}

/// 逐题产物中的一行标识（`items.jsonl` 的读取视图）。
///
/// 字段约定:
/// - `condition_id` / `question_id`: 复合键；
/// - `success`: 该行的结果类别。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemKey {
    pub condition_id: String,
    pub question_id: String,
    pub success: bool,
}

/// 扫描结果类别用的最小列集（忽略逐题产物其余字段）。
///
/// 说明: 结果类别优先取显式 `success`，缺省时按 `error` 是否为空推导。
#[derive(Debug, Deserialize)]
struct ScanRow {
    condition_id: String,
    question_id: String,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    error: Option<String>,
}

/// 读取尝试次数用的最小列集（忽略逐题产物其余字段）。
#[derive(Debug, Deserialize)]
struct AttemptRow {
    condition_id: String,
    question_id: String,
    attempts: u32,
}

// =========================================================
// 读取与写入
// =========================================================

impl RunState {
    /// 读取已有状态，或在空目录新建。
    ///
    /// 参数:
    /// - `root`: 运行根目录（`run.json` 所在目录）；
    /// - `matrix`: 本次矩阵；
    /// - `tiers`: 本次档位引用；
    /// - `condition_totals`: 每个条件的计划题目数。
    ///
    /// 返回:
    /// - 新建：状态为运行中、计数清零，并立即落盘；
    /// - 已有：保留计数供续跑，状态回到运行中；
    /// - 已有状态的矩阵与入参不一致返回 `Validation`（提示换 `--output` 或删旧目录）。
    pub fn load_or_init(
        root: &Path,
        matrix: &RunMatrix,
        tiers: &[RunTierRef],
        condition_totals: &[(String, usize)],
    ) -> RamariaResult<Self> {
        let path = root.join(RUN_FILE);
        if path.is_file() {
            let text = std::fs::read_to_string(&path).map_err(|error| {
                RamariaError::io(format!("读取运行状态失败: {}", path.display()), Some(error))
            })?;
            let mut state: Self = serde_json::from_str(&text).map_err(|error| {
                RamariaError::validation(format!(
                    "解析运行状态失败: {}: {error}（请更换 --output 或删除该目录后重跑）",
                    path.display()
                ))
            })?;
            let expected = matrix_value(matrix, "本次矩阵")?;
            let actual = matrix_value(&state.matrix, "运行目录矩阵")?;
            if expected != actual {
                return Err(RamariaError::validation(format!(
                    "运行目录的矩阵与本次矩阵不一致: {}（请更换 --output 或删除该目录后重跑）",
                    root.display()
                )));
            }
            state.status = RunStatus::Running;
            return Ok(state);
        }

        let run_id = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                RamariaError::validation(format!("无法确定运行目录名: {}", root.display()))
            })?
            .to_string();
        let now = now_rfc3339();
        let state = Self {
            artifact_kind: RUN_ARTIFACT_KIND.to_string(),
            schema_version: RUN_SCHEMA_VERSION,
            run_id,
            created_at: now.clone(),
            updated_at: now,
            status: RunStatus::Running,
            matrix: matrix.clone(),
            language: matrix.run.language.clone(),
            tiers: tiers.to_vec(),
            conditions: condition_totals
                .iter()
                .map(|(id, total)| ConditionState {
                    id: id.clone(),
                    total: *total,
                    completed: 0,
                    failed: 0,
                })
                .collect(),
            elapsed_ms: 0,
            warnings: Vec::new(),
        };
        state.save(root)?;
        Ok(state)
    }

    /// 写出状态（`updated_at` 取落盘时刻）。
    ///
    /// 参数:
    /// - `root`: 运行根目录。
    ///
    /// 返回:
    /// - 写入并改名成功为 `Ok(())`；序列化或 I/O 失败返回对应错误。
    pub fn save(&self, root: &Path) -> RamariaResult<()> {
        let mut snapshot = self.clone();
        snapshot.updated_at = now_rfc3339();
        let text = serde_json::to_string_pretty(&snapshot).map_err(|error| {
            RamariaError::Serialization {
                context: format!("序列化运行状态失败: {}", self.run_id),
                source: Some(Box::new(error)),
                trace_id: None,
            }
        })?;

        let path = root.join(RUN_FILE);
        let temp = root.join(format!("{RUN_FILE}.tmp"));
        std::fs::write(&temp, text).map_err(|error| {
            RamariaError::io(
                format!("写入运行状态临时文件失败: {}", temp.display()),
                Some(error),
            )
        })?;
        std::fs::rename(&temp, &path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            RamariaError::io(format!("替换运行状态失败: {}", path.display()), Some(error))
        })
    }

    /// 按逐题结果重建各条件计数。
    ///
    /// 参数:
    /// - `records`: `(条件标识, 题目标识, 是否成功)`，同一复合键以最后一次为准。
    ///
    /// 说明:
    /// - 只改动状态里已登记的条件；未出现的条件计数归零；
    /// - `completed` 与 `failed` 互斥，两者之和等于该条件已定案的题目数。
    pub fn recount(&mut self, records: &[(String, String, bool)]) {
        let mut last: HashMap<(&str, &str), bool> = HashMap::new();
        for (condition_id, question_id, success) in records {
            last.insert((condition_id.as_str(), question_id.as_str()), *success);
        }

        let mut counters: HashMap<&str, (usize, usize)> = HashMap::new();
        for ((condition_id, _question_id), success) in &last {
            let entry = counters.entry(condition_id).or_insert((0, 0));
            if *success {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }

        for condition in &mut self.conditions {
            let (completed, failed) = counters
                .get(condition.id.as_str())
                .copied()
                .unwrap_or((0, 0));
            condition.completed = completed;
            condition.failed = failed;
        }
    }
}

// =========================================================
// 逐题产物扫描
// =========================================================

/// 逐行读取 `items.jsonl`，返回每行的复合键与结果类别。
///
/// 参数:
/// - `path`: 逐题产物路径（不存在视为空）。
///
/// 返回:
/// - 按文件顺序返回全部行；
/// - 空文件或文件缺失返回空列表；
/// - 空行或不可解析的行返回 `Validation`，文案带行号。
pub fn scan_items(path: &Path) -> RamariaResult<Vec<ItemKey>> {
    let mut keys = Vec::new();
    for (index, line) in read_lines(path)?.into_iter().enumerate() {
        let row: ScanRow = serde_json::from_str(&line).map_err(|error| {
            RamariaError::validation(format!(
                "解析逐题产物失败: {} 第 {} 行: {error}",
                path.display(),
                index + 1
            ))
        })?;
        let success = row.success.unwrap_or(row.error.is_none());
        keys.push(ItemKey {
            condition_id: row.condition_id,
            question_id: row.question_id,
            success,
        });
    }
    Ok(keys)
}

/// 逐行读取 `items.jsonl`，返回每题最后一次尝试次数。
///
/// 参数:
/// - `path`: 逐题产物路径（不存在视为空）。
///
/// 返回:
/// - 复合键 → 最后一次记录的尝试次数（同一键后行覆盖前行）；
/// - 坏行返回 `Validation`，文案带行号。
pub fn scan_attempts(path: &Path) -> RamariaResult<HashMap<(String, String), u32>> {
    let mut attempts = HashMap::new();
    for (index, line) in read_lines(path)?.into_iter().enumerate() {
        let row: AttemptRow = serde_json::from_str(&line).map_err(|error| {
            RamariaError::validation(format!(
                "解析逐题尝试次数失败: {} 第 {} 行: {error}",
                path.display(),
                index + 1
            ))
        })?;
        attempts.insert((row.condition_id, row.question_id), row.attempts);
    }
    Ok(attempts)
}

/// 读取文件全部行；文件不存在时返回空列表。
fn read_lines(path: &Path) -> RamariaResult<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(|error| {
        RamariaError::io(format!("读取逐题产物失败: {}", path.display()), Some(error))
    })?;
    Ok(text.lines().map(str::to_string).collect())
}

// =========================================================
// 辅助
// =========================================================

/// 矩阵的序列化值（逐值比对用）。
fn matrix_value(matrix: &RunMatrix, label: &str) -> RamariaResult<serde_json::Value> {
    serde_json::to_value(matrix).map_err(|error| RamariaError::Serialization {
        context: format!("序列化{label}失败"),
        source: Some(Box::new(error)),
        trace_id: None,
    })
}

/// 当前 UTC 时间（RFC3339，秒级，Z 结尾）。
fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
