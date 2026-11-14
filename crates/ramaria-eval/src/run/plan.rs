//! crates/ramaria-eval/src/run/plan.rs - Ramaria 评测题目枚举模块
//!
//! 设计特点:
//! - 档位加载即校验：产物类型、清单版本、目录名与行格式任一不符就报错
//! - 题目保持 qa.jsonl 原顺序，不可回答题保留供可信层统计
//! - persona 标识由 sample_key 直接推导，不依赖语料文件
//! - 全局上限跨档位累计，截断只发生在题目边界
//! - 计划规模与题集同源：抽样与上限复用运行器口径，预览不改动档位
//! - 只读档位产物，不碰引擎与数据库

use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};

use crate::baseline::{
    ARTIFACT_KIND, ARTIFACT_SCHEMA_VERSION, BaselineManifest, MANIFEST_FILE, QRELS_FILE, QrelsRow,
};

use super::matrix::RunMatrix;
use super::workspace::BaselineProduct;

#[cfg(test)]
mod tests;

/// 人格标识前缀：与语料样本的 persona 口径一致。
const PERSONA_UID_PREFIX: &str = "lifebench-";

// =========================================================
// 档位
// =========================================================

/// 一个已构建的基线档位。
///
/// 字段约定:
/// - `dir`: 档位产物目录；
/// - `manifest`: 已校验的清单；
/// - `rows`: qa.jsonl 全量题目，保持文件顺序。
#[derive(Debug, Clone, PartialEq)]
pub struct BaselineTier {
    pub dir: PathBuf,
    pub manifest: BaselineManifest,
    pub rows: Vec<QrelsRow>,
}

impl BaselineTier {
    /// 读取并校验一个档位目录。
    ///
    /// 参数:
    /// - `dir`: 档位产物目录（目录名即档位标识）。
    ///
    /// 返回:
    /// - 成功时返回清单与全部题目；
    /// - 清单缺失 / qrels 不可读返回 `Io`；清单类型、版本、目录名或行格式不符
    ///   返回 `Validation` / `Serialization`，文案带文件路径。
    pub fn load(dir: &Path) -> RamariaResult<Self> {
        let manifest_path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
            RamariaError::io(
                format!("读取基线清单失败: {}", manifest_path.display()),
                Some(error),
            )
        })?;
        let manifest: BaselineManifest = serde_json::from_str(&text).map_err(|error| {
            RamariaError::serialization(format!(
                "解析基线清单失败: {}: {error}",
                manifest_path.display()
            ))
        })?;

        if manifest.artifact_kind != ARTIFACT_KIND {
            return Err(RamariaError::validation(format!(
                "基线产物类型不符: 期望 {ARTIFACT_KIND}，实际 {}（{}）",
                manifest.artifact_kind,
                manifest_path.display()
            )));
        }
        if manifest.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(RamariaError::validation(format!(
                "基线清单版本不符: 期望 {ARTIFACT_SCHEMA_VERSION}，实际 {}（{}）",
                manifest.schema_version,
                manifest_path.display()
            )));
        }
        let dir_name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                RamariaError::validation(format!("无法确定档位目录名: {}", dir.display()))
            })?;
        if dir_name != manifest.tier.id {
            return Err(RamariaError::validation(format!(
                "档位目录名与清单不一致: 目录 {dir_name}，清单 {}（{}）",
                manifest.tier.id,
                manifest_path.display()
            )));
        }

        let rows = read_rows(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
            rows,
        })
    }

    /// 由档位目录构造基线产物路径。
    pub fn product(&self) -> BaselineProduct {
        BaselineProduct::from_dir(&self.dir)
    }
}

/// 逐行读取 qrels；空行视为格式错误，不静默跳过。
fn read_rows(dir: &Path) -> RamariaResult<Vec<QrelsRow>> {
    let path = dir.join(QRELS_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(format!("读取 qrels 失败: {}", path.display()), Some(error))
    })?;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            return Err(RamariaError::serialization(format!(
                "解析 qrels 失败: {} 第 {} 行为空行",
                path.display(),
                index + 1
            )));
        }
        let row = serde_json::from_str(line).map_err(|error| {
            RamariaError::serialization(format!(
                "解析 qrels 失败: {} 第 {} 行: {error}",
                path.display(),
                index + 1
            ))
        })?;
        rows.push(row);
    }
    Ok(rows)
}

// =========================================================
// 题目
// =========================================================

/// 一道待运行题目。
///
/// 字段约定:
/// - `row`: 题目、参考答案与消息级 gold；
/// - `tier`: 题目所属基线档位标识（与条件档位对照，决定该题参与哪些条件）；
/// - `persona_uid`: 检索与注入使用的人格标识。
#[derive(Debug, Clone, PartialEq)]
pub struct RunItem {
    pub row: QrelsRow,
    pub tier: String,
    pub persona_uid: String,
}

/// 按档位顺序展开题目，并应用跨档位全局上限。
///
/// 参数:
/// - `tiers`: 已加载的档位，顺序即题目顺序；
/// - `limit`: 全局题目上限，`None` 表示不截断。
///
/// 返回:
/// - 展开后的题目列表；不可回答题（gold 为空）不过滤。
pub fn plan_items(tiers: &[BaselineTier], limit: Option<usize>) -> Vec<RunItem> {
    let mut items = Vec::new();
    for tier in tiers {
        for row in &tier.rows {
            if limit.is_some_and(|limit| items.len() >= limit) {
                return items;
            }
            items.push(RunItem {
                row: row.clone(),
                tier: tier.manifest.tier.id.clone(),
                persona_uid: persona_uid(&row.sample_key),
            });
        }
    }
    items
}

/// sample_key → 人格标识。
fn persona_uid(sample_key: &str) -> String {
    format!("{PERSONA_UID_PREFIX}{sample_key}")
}

// =========================================================
// 计划规模
// =========================================================

/// 矩阵在给定档位下的计划规模（预览用，不装配引擎）。
///
/// 字段约定:
/// - `per_tier`: 各档位计划题数（抽样与全局上限之后），顺序同矩阵档位；
/// - `conditions`: 条件总数（档位 × 管线 × 预算 × 答题模型）；
/// - `questions`: 计划题目数（各档位题数之和）；
/// - `executions`: 计划条目数（每档题数 × 该档条件数）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlanCounts {
    pub(crate) per_tier: Vec<(String, usize)>,
    pub(crate) conditions: usize,
    pub(crate) questions: usize,
    pub(crate) executions: usize,
}

/// 按运行器口径计算计划规模。
///
/// 参数:
/// - `matrix`: 矩阵定义；
/// - `tiers`: 已加载的档位（未抽样），顺序即题目顺序。
///
/// 返回:
/// - 与 `plan_items` 一致的题数：先按 `run.question_sample` 取每档选中数，
///   再按 `run.question_limit` 跨档位累计截断；
/// - 条件数按 `matrix.conditions()` 计，条数为各档题数乘以该档条件数。
///
/// 说明: 只读取行，不修改档位；抽样结果与运行器同源（同一个 `sample_tier`）。
pub(crate) fn plan_counts(matrix: &RunMatrix, tiers: &[BaselineTier]) -> PlanCounts {
    let per_tier_conditions =
        matrix.run.pipelines.len() * matrix.run.budgets.len() * matrix.answer_models.len();
    let mut per_tier = Vec::with_capacity(tiers.len());
    let mut remaining = matrix.run.question_limit;
    for tier in tiers {
        let available = match matrix.run.question_sample {
            Some(budget) => {
                super::sample::sample_tier(&tier.manifest.tier.id, &tier.rows, budget)
                    .1
                    .selected
            }
            None => tier.rows.len(),
        };
        let take = remaining.map_or(available, |left| available.min(left));
        remaining = remaining.map(|left| left.saturating_sub(take));
        per_tier.push((tier.manifest.tier.id.clone(), take));
    }
    let questions = per_tier.iter().map(|(_, count)| count).sum();
    let executions = per_tier
        .iter()
        .map(|(_, count)| count * per_tier_conditions)
        .sum();
    PlanCounts {
        per_tier,
        conditions: matrix.conditions().len(),
        questions,
        executions,
    }
}
