//! crates/ramaria-eval/src/run/sample.rs - Ramaria 评测分层抽样模块
//!
//! 设计特点:
//! - 按「人格 × 类别签名」分层：签名取排序后的类别值，空类别记 none
//! - 预算先保底每层 1 题，余额按层大小比例用最大余数法分配，单层不超过其题数
//! - 层内先按题号字典序排序再等距取题，抽样覆盖时间分布且逐字节确定
//! - 抽样只做过滤，题目仍按 qa.jsonl 原顺序进入运行器
//! - 抽样结果落 sample.json 供续跑复用；规格或题数变化一律报错，不静默重抽

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use ramaria_core::error::{RamariaError, RamariaResult};
use serde::{Deserialize, Serialize};

use crate::baseline::QrelsRow;

use super::matrix::RunMatrix;
use super::plan::BaselineTier;

#[cfg(test)]
mod tests;

/// 抽样产物文件名。
pub const SAMPLE_FILE: &str = "sample.json";

/// 抽样产物类型标识。
pub const SAMPLE_ARTIFACT_KIND: &str = "ramaria_eval_sample";

/// 抽样产物自身的 schema 版本。
pub const SAMPLE_SCHEMA_VERSION: u32 = 1;

// =========================================================
// 结构
// =========================================================

/// 抽样规格。
///
/// 字段约定:
/// - `per_tier`: 每个档位的抽样预算（题数），必须大于 0。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleSpec {
    pub per_tier: usize,
}

/// 一个分层的抽样统计。
///
/// 字段约定:
/// - `sample_key` / `category`: 层键，类别为排序后以 `-` 连接的签名；
/// - `size`: 该层题数；
/// - `picked`: 该层实际抽中的题数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StratumSample {
    pub sample_key: String,
    pub category: String,
    pub size: usize,
    pub picked: usize,
}

/// 一个档位的抽样结果。
///
/// 字段约定:
/// - `tier`: 档位标识；
/// - `total` / `selected`: 档位题数与抽中题数；
/// - `strata`: 分层统计，按层键升序；
/// - `question_ids`: 抽中题号，保持 qa.jsonl 原顺序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierSample {
    pub tier: String,
    pub total: usize,
    pub selected: usize,
    pub strata: Vec<StratumSample>,
    pub question_ids: Vec<String>,
}

/// 整个运行的抽样记录（`sample.json`）。
///
/// 字段约定:
/// - `artifact_kind` / `schema_version`: 产物识别与版本；
/// - `spec`: 抽样规格，续跑时与当前矩阵比对；
/// - `tiers`: 按矩阵档位顺序排列的档位结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleReport {
    pub artifact_kind: String,
    pub schema_version: u32,
    pub spec: SampleSpec,
    pub tiers: Vec<TierSample>,
}

/// 抽样接入结果。
///
/// 字段约定:
/// - `report`: 本次新算出的记录；沿用既有 `sample.json` 时为 `None`（无需重写）；
/// - `lines`: 每档一行的人读提示。
#[derive(Debug)]
pub(crate) struct AppliedSample {
    pub report: Option<SampleReport>,
    pub lines: Vec<String>,
}

// =========================================================
// 抽样
// =========================================================

/// 对一个档位的题目做分层抽样。
///
/// 参数:
/// - `tier`: 档位标识（写入报告）；
/// - `rows`: 档位全部题目，顺序即 qa.jsonl 顺序；
/// - `budget`: 抽样预算，大于等于题数时全选。
///
/// 返回:
/// - 保留的行下标（升序，指向 `rows`）与档位报告；
/// - 层内按题号字典序等距取题，报告中的题号保持原文件顺序。
pub fn sample_tier(tier: &str, rows: &[QrelsRow], budget: usize) -> (Vec<usize>, TierSample) {
    let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        groups
            .entry((row.sample_key.clone(), category_signature(&row.category)))
            .or_default()
            .push(index);
    }

    let mut strata: Vec<StratumSample> = groups
        .iter()
        .map(|((sample_key, category), indices)| StratumSample {
            sample_key: sample_key.clone(),
            category: category.clone(),
            size: indices.len(),
            picked: 0,
        })
        .collect();
    allocate(&mut strata, budget);

    let mut selected = Vec::new();
    for (stratum, indices) in strata.iter().zip(groups.values()) {
        if stratum.picked == 0 {
            continue;
        }
        if stratum.picked >= indices.len() {
            selected.extend(indices.iter().copied());
            continue;
        }
        let mut ordered = indices.clone();
        ordered.sort_by(|left, right| rows[*left].question_id.cmp(&rows[*right].question_id));
        let size = ordered.len();
        for position in 0..stratum.picked {
            selected.push(ordered[position * size / stratum.picked]);
        }
    }
    selected.sort_unstable();

    let question_ids = selected
        .iter()
        .map(|index| rows[*index].question_id.clone())
        .collect();
    let sample = TierSample {
        tier: tier.to_string(),
        total: rows.len(),
        selected: selected.len(),
        strata,
        question_ids,
    };
    (selected, sample)
}

/// 类别签名：排序后以 `-` 连接；空类别记 `none`。
fn category_signature(category: &[u8]) -> String {
    if category.is_empty() {
        return "none".to_string();
    }
    let mut values = category.to_vec();
    values.sort_unstable();
    values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("-")
}

/// 按层大小把预算分到各层。
///
/// 规则:
/// - 预算不小于总题数时全选；
/// - 预算小于层数时按层大小降序取前 budget 层，每层 1 题；
/// - 其余情况先每层保底 1 题，余额按层大小比例分配，余数按最大余数法补齐，
///   达到层容量的层不再参与，直到预算分完。
fn allocate(strata: &mut [StratumSample], budget: usize) {
    let total: usize = strata.iter().map(|stratum| stratum.size).sum();
    if budget == 0 || total == 0 {
        return;
    }
    if budget >= total {
        for stratum in strata.iter_mut() {
            stratum.picked = stratum.size;
        }
        return;
    }
    if budget < strata.len() {
        let mut order: Vec<usize> = (0..strata.len()).collect();
        order.sort_by(|left, right| {
            strata[*right]
                .size
                .cmp(&strata[*left].size)
                .then_with(|| left.cmp(right))
        });
        for index in order.into_iter().take(budget) {
            strata[index].picked = 1;
        }
        return;
    }

    for stratum in strata.iter_mut() {
        stratum.picked = 1;
    }
    let mut remaining = budget - strata.len();
    while remaining > 0 {
        let eligible: Vec<usize> = (0..strata.len())
            .filter(|index| strata[*index].picked < strata[*index].size)
            .collect();
        if eligible.is_empty() {
            return;
        }
        let weight: usize = eligible.iter().map(|index| strata[*index].size).sum();
        let mut added = 0usize;
        let mut remainders: Vec<(usize, usize)> = Vec::with_capacity(eligible.len());
        for index in eligible {
            let quota = remaining * strata[index].size / weight;
            let capacity = strata[index].size - strata[index].picked;
            let add = quota.min(capacity);
            strata[index].picked += add;
            added += add;
            remainders.push((index, (remaining * strata[index].size) % weight));
        }
        remaining -= added;
        if remaining == 0 {
            return;
        }
        if added == 0 {
            // 比例份额都落不到 1 题：按余数降序、层大小降序、层键序各补 1 题。
            remainders.sort_by(|left, right| {
                right
                    .1
                    .cmp(&left.1)
                    .then_with(|| strata[right.0].size.cmp(&strata[left.0].size))
                    .then_with(|| left.0.cmp(&right.0))
            });
            for (index, _) in remainders {
                if remaining == 0 {
                    return;
                }
                if strata[index].picked < strata[index].size {
                    strata[index].picked += 1;
                    remaining -= 1;
                }
            }
            return;
        }
    }
}

// =========================================================
// 运行器接入
// =========================================================

/// 按矩阵的抽样预算过滤各档位题目。
///
/// 参数:
/// - `matrix`: 当前矩阵（提供 `run.question_sample`）；
/// - `root`: 运行根目录（`sample.json` 所在）；
/// - `tiers`: 已加载的档位，就地过滤为抽样题集。
///
/// 返回:
/// - 待写报告（沿用既有记录时为 `None`）与每档提示行；
/// - 既有记录与当前规格 / 题数不一致、题号缺失返回 `Validation`。
pub(crate) fn apply(
    matrix: &RunMatrix,
    root: &Path,
    tiers: &mut [BaselineTier],
) -> RamariaResult<AppliedSample> {
    let Some(budget) = matrix.run.question_sample else {
        return Ok(AppliedSample {
            report: None,
            lines: Vec::new(),
        });
    };
    let spec = SampleSpec { per_tier: budget };
    let existing = load(root)?;
    if let Some(report) = &existing {
        check_spec(report.spec, spec)?;
    }

    let mut fresh = SampleReport {
        artifact_kind: SAMPLE_ARTIFACT_KIND.to_string(),
        schema_version: SAMPLE_SCHEMA_VERSION,
        spec,
        tiers: Vec::new(),
    };
    let mut lines = Vec::with_capacity(tiers.len());
    for tier in tiers.iter_mut() {
        let tier_id = tier.manifest.tier.id.clone();
        let record = match &existing {
            Some(report) => Some(
                report
                    .tiers
                    .iter()
                    .find(|record| record.tier == tier_id)
                    .ok_or_else(|| {
                        RamariaError::validation(format!(
                            "抽样记录缺少档位: {tier_id}（请删除 {} 后重跑）",
                            root.join(SAMPLE_FILE).display()
                        ))
                    })?,
            ),
            None => None,
        };
        let sample = apply_tier(&tier_id, &mut tier.rows, budget, record)?;
        lines.push(format!(
            "档位 {tier_id}: 抽样 {}/{}（分层 {}）",
            sample.selected,
            sample.total,
            sample.strata.len()
        ));
        if existing.is_none() {
            fresh.tiers.push(sample);
        }
    }

    Ok(AppliedSample {
        report: existing.is_none().then_some(fresh),
        lines,
    })
}

/// 单档过滤：有记录时按题号复用，无记录时现场抽样。
///
/// 说明:
/// - 复用前校验记录题数、选中数与当前档位一致，题号缺失报错并带上题号；
/// - 现场抽样按行下标过滤，重复题号也不会多选。
fn apply_tier(
    tier: &str,
    rows: &mut Vec<QrelsRow>,
    budget: usize,
    record: Option<&TierSample>,
) -> RamariaResult<TierSample> {
    let Some(record) = record else {
        let (indices, sample) = sample_tier(tier, rows, budget);
        let mut mask = vec![false; rows.len()];
        for index in indices {
            if let Some(slot) = mask.get_mut(index) {
                *slot = true;
            }
        }
        let mut cursor = mask.into_iter();
        rows.retain(|_| cursor.next().unwrap_or(false));
        return Ok(sample);
    };

    if record.total != rows.len() {
        return Err(RamariaError::validation(format!(
            "抽样记录与当前档位题数不一致: 档位 {tier}（记录 {} / 当前 {}）",
            record.total,
            rows.len()
        )));
    }
    if record.selected != record.question_ids.len() {
        return Err(RamariaError::validation(format!(
            "抽样记录题数与选中数不一致: 档位 {tier}（选中 {} / 题号 {}）",
            record.selected,
            record.question_ids.len()
        )));
    }
    let current: HashSet<&str> = rows.iter().map(|row| row.question_id.as_str()).collect();
    for question_id in &record.question_ids {
        if !current.contains(question_id.as_str()) {
            return Err(RamariaError::validation(format!(
                "抽样记录中的题目不存在: {question_id}（档位 {tier}）"
            )));
        }
    }
    let keep: HashSet<&str> = record.question_ids.iter().map(String::as_str).collect();
    rows.retain(|row| keep.contains(row.question_id.as_str()));
    Ok(record.clone())
}

/// 规格一致性：只比对每档预算，不一致直接报错。
fn check_spec(existing: SampleSpec, current: SampleSpec) -> RamariaResult<()> {
    if existing != current {
        return Err(RamariaError::validation(format!(
            "抽样规格与既有运行不一致（既有 {} / 当前 {}）",
            existing.per_tier, current.per_tier
        )));
    }
    Ok(())
}

// =========================================================
// 读写
// =========================================================

/// 写出抽样记录（先临时文件再改名，末尾带换行）。
///
/// 参数:
/// - `dir`: 运行根目录；
/// - `report`: 抽样记录。
///
/// 返回:
/// - 成功时返回记录路径；序列化或 I/O 失败返回对应错误。
pub fn write(dir: &Path, report: &SampleReport) -> RamariaResult<PathBuf> {
    let mut text =
        serde_json::to_string_pretty(report).map_err(|error| RamariaError::Serialization {
            context: "序列化抽样记录失败".to_string(),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;
    text.push('\n');

    let path = dir.join(SAMPLE_FILE);
    let temp = dir.join(format!("{SAMPLE_FILE}.tmp"));
    std::fs::write(&temp, text).map_err(|error| {
        RamariaError::io(
            format!("写入抽样记录临时文件失败: {}", temp.display()),
            Some(error),
        )
    })?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        RamariaError::io(format!("替换抽样记录失败: {}", path.display()), Some(error))
    })?;
    Ok(path)
}

/// 读取运行根下的抽样记录。
///
/// 参数:
/// - `dir`: 运行根目录。
///
/// 返回:
/// - 文件不存在返回 `Ok(None)`；
/// - 解析失败、产物类型或版本不符、规格为零返回 `Validation`。
pub fn load(dir: &Path) -> RamariaResult<Option<SampleReport>> {
    let path = dir.join(SAMPLE_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| {
        RamariaError::io(format!("读取抽样记录失败: {}", path.display()), Some(error))
    })?;
    let report: SampleReport = serde_json::from_str(&text).map_err(|error| {
        RamariaError::validation(format!(
            "解析抽样记录失败: {}: {error}（请删除该文件后重跑）",
            path.display()
        ))
    })?;
    if report.artifact_kind != SAMPLE_ARTIFACT_KIND {
        return Err(RamariaError::validation(format!(
            "抽样记录类型不符: 期望 {SAMPLE_ARTIFACT_KIND}，实际 {}（{}）",
            report.artifact_kind,
            path.display()
        )));
    }
    if report.schema_version != SAMPLE_SCHEMA_VERSION {
        return Err(RamariaError::validation(format!(
            "抽样记录版本不符: 期望 {SAMPLE_SCHEMA_VERSION}，实际 {}（{}）",
            report.schema_version,
            path.display()
        )));
    }
    if report.spec.per_tier == 0 {
        return Err(RamariaError::validation(format!(
            "抽样记录规格非法: per_tier 必须大于 0（{}）",
            path.display()
        )));
    }
    Ok(Some(report))
}
