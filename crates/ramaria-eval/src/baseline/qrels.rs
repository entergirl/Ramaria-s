//! crates/ramaria-eval/src/baseline/qrels.rs - Ramaria 评测基线 qrels 生成
//!
//! 设计特点:
//! - 每行一条题：问题、参考答案、类别、证据 artifact 与消息级 gold 一次写全
//! - gold 取 evidence 指向 artifact 的消息 id 并集，按 evidence 顺序与消息顺序去重
//! - 提问时钟取当日 23:59:59.999（UTC 毫秒），与日后检索口径一致
//! - 只保留档位查询窗口内的题；不可回答题天然没有 evidence 与 gold
//! - 未命中 artifact（建库窗口外）计入缺失计数，由清单记一条告警

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::NaiveDate;
use ramaria_core::error::{RamariaError, RamariaResult};
use serde::{Deserialize, Serialize};

use crate::corpus::{Qa, Sample};

use super::tier::TierSpec;

/// 一天毫秒数（当日结束时刻 = 当日零点 + 该值 - 1）。
const DAY_MS: i64 = 86_400_000;

/// qrels 文件名。
pub const QRELS_FILE: &str = "qa.jsonl";

/// 一条 qrels 记录。
///
/// 字段约定:
/// - `sample_key`: 语料用户标识（question_id 中的 user 段）；
/// - `ask_time`: 提问日（YYYY-MM-DD）；
/// - `ask_time_ms`: 提问日结束时刻（UTC 毫秒）；
/// - `evidence`: 原始 evidence artifact 标识，保持语料顺序；
/// - `gold_message_ids`: evidence 指向 artifact 下全部消息 id（去重保序）；
/// - `score_points`: 分项判分要点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QrelsRow {
    pub question_id: String,
    pub sample_key: String,
    pub ask_time: String,
    pub ask_time_ms: i64,
    pub question: String,
    pub answer: String,
    pub category: Vec<u8>,
    pub question_type: Vec<String>,
    pub evidence: Vec<String>,
    pub gold_message_ids: Vec<String>,
    pub score_points: Vec<QrelsScorePoint>,
}

/// 分项判分要点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QrelsScorePoint {
    pub description: String,
    pub score: f64,
}

/// 单个样本的 qrels 生成结果。
///
/// 字段约定:
/// - `rows`: 查询窗口内的题，保持语料原顺序；
/// - `missing_evidence`: 未在建库窗口内命中的 evidence 引用数。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QrelsRows {
    pub rows: Vec<QrelsRow>,
    pub missing_evidence: usize,
}

/// 生成单个样本的 qrels。
///
/// 参数:
/// - `sample`: 语料样本（提供 QA 与 user_key）；
/// - `spec`: 档位（提供查询窗口）；
/// - `artifacts`: artifact 标识 → 消息 id 列表（建库时按会话顺序建立）。
///
/// 返回:
/// - 窗口内 qrels 行与未命中 evidence 计数。
pub fn rows(
    sample: &Sample,
    spec: &TierSpec,
    artifacts: &HashMap<String, Vec<String>>,
) -> QrelsRows {
    let mut rows = Vec::new();
    let mut missing_evidence = 0usize;

    for qa in &sample.qa {
        if !spec.includes_question(qa.ask_time) {
            continue;
        }
        let (gold, missing) = gold_message_ids(qa, artifacts);
        missing_evidence += missing;
        rows.push(QrelsRow {
            question_id: qa.question_id.clone(),
            sample_key: sample.user_key.clone(),
            ask_time: qa.ask_time.format("%Y-%m-%d").to_string(),
            ask_time_ms: ask_time_ms(qa.ask_time),
            question: qa.question.clone(),
            answer: qa.answer.clone(),
            category: qa.category.clone(),
            question_type: qa.question_type.clone(),
            evidence: qa.evidence.clone(),
            gold_message_ids: gold,
            score_points: qa
                .score_points
                .iter()
                .map(|point| QrelsScorePoint {
                    description: point.description.clone(),
                    score: point.score,
                })
                .collect(),
        });
    }

    QrelsRows {
        rows,
        missing_evidence,
    }
}

/// 证据 artifact 下的消息 id 并集。
///
/// 说明:
/// - 顺序为 evidence 出现顺序 + artifact 内消息顺序，重复 id 只保留第一次；
/// - evidence 未命中（建库窗口外）计入第二个返回值。
fn gold_message_ids(qa: &Qa, artifacts: &HashMap<String, Vec<String>>) -> (Vec<String>, usize) {
    let mut ids = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut missing = 0usize;

    for artifact in &qa.evidence {
        match artifacts.get(artifact) {
            Some(message_ids) => {
                for id in message_ids {
                    if seen.insert(id.as_str()) {
                        ids.push(id.clone());
                    }
                }
            }
            None => missing += 1,
        }
    }
    (ids, missing)
}

/// 提问日结束时刻（UTC 毫秒）。
pub fn ask_time_ms(date: NaiveDate) -> i64 {
    let midnight = date
        .and_time(chrono::NaiveTime::MIN)
        .and_utc()
        .timestamp_millis();
    midnight + DAY_MS - 1
}

/// 写出 qrels 文件（每行一条 JSON）。
///
/// 参数:
/// - `dir`: 档位产物目录；
/// - `rows`: qrels 行（空列表也会写出空文件）。
pub fn write(dir: &Path, rows: &[QrelsRow]) -> RamariaResult<()> {
    let mut text = String::new();
    for row in rows {
        let line = serde_json::to_string(row).map_err(|error| RamariaError::Serialization {
            context: format!("序列化 qrels 失败: {}", row.question_id),
            source: Some(Box::new(error)),
            trace_id: None,
        })?;
        text.push_str(&line);
        text.push('\n');
    }

    let path = dir.join(QRELS_FILE);
    std::fs::write(&path, text).map_err(|error| {
        RamariaError::io(format!("写入 qrels 失败: {}", path.display()), Some(error))
    })
}

#[cfg(test)]
mod tests;
