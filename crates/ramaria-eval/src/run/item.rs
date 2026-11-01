//! crates/ramaria-eval/src/run/item.rs - Ramaria 评测逐题产物
//!
//! 设计特点:
//! - 一行一题：题目口径、条件口径、结果与错误一次写全，判分无需再读基线
//! - 条件行每题重复写入，报告与判分可只读 `items.jsonl` 即可复原实验条件
//! - 成功与失败共用同一结构：失败行 `reply` / `recall` 为 `null`，`error` 带原因
//! - 追加写：只 `append` 不重写，中断后同一键多行以最后一行为准
//! - 设备字段取编译期常量与运行时核数，不探测硬件型号

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{EvalChatOutcome, EvalRecallObservation};
use serde::{Deserialize, Serialize};

use crate::baseline::BaselineManifest;

use super::matrix::Condition;
use super::plan::RunItem;

#[cfg(test)]
mod tests;

// =========================================================
// 逐题产物
// =========================================================

/// 一道题在某个条件下的运行结果。
///
/// 字段约定:
/// - `tier` / `pipeline` / `budget_chars` / `answer_model`: 条件轴；
/// - `sample_key` / `persona_uid` / `ask_time` / `as_of_ms`: 题目与查询时钟；
/// - `question` / `gold_answer` / `gold_message_ids` / `category`: 判分原料；
/// - `reply` / `reply_chars` / `recall`: 结果（失败行为 `None`）；
/// - `attempts`: 本题累计尝试次数（含首次）；
/// - `conditions`: 语料 / 模型 / 设备 / 索引等条件行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunItemRecord {
    pub question_id: String,
    pub condition_id: String,
    pub tier: String,
    pub pipeline: String,
    pub budget_chars: u32,
    pub answer_model: String,
    /// 实际生效的答题模型（本地留空时由服务端自动选择，此处记生效标识）。
    pub answer_model_effective: String,
    pub sample_key: String,
    pub persona_uid: String,
    pub ask_time: String,
    pub as_of_ms: i64,
    pub question: String,
    pub gold_answer: String,
    pub gold_message_ids: Vec<String>,
    pub category: Vec<u8>,
    pub reply: Option<String>,
    pub reply_chars: usize,
    pub recall: Option<EvalRecallObservation>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    pub attempts: u32,
    pub conditions: ConditionLine,
}

/// 每题重复写入的条件行。
///
/// 字段约定:
/// - 语料与模型口径取自基线清单，索引状态取条件装配后的实测值；
/// - 设备字段为编译目标与运行时核数，用于解释跨机差异。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionLine {
    pub corpus_name: String,
    pub corpus_version: String,
    pub language: String,
    pub corpus_sha256: String,
    pub memory_model: String,
    pub memory_model_kind: String,
    pub memory_temperature: f64,
    pub memory_max_tokens: u32,
    pub embedding_fingerprint: String,
    pub embedding_dimension: Option<usize>,
    pub prompt_template_version: String,
    pub schema_version: i32,
    pub index_status: String,
    pub device_os: String,
    pub device_arch: String,
    pub device_cores: usize,
}

impl ConditionLine {
    /// 由基线清单构造条件行。
    ///
    /// 参数:
    /// - `manifest`: 该条件档位的基线清单；
    /// - `language`: 本次运行的语言标签；
    /// - `index_status`: 条件装配后实测的索引状态。
    pub fn from_manifest(manifest: &BaselineManifest, language: &str, index_status: &str) -> Self {
        Self {
            corpus_name: manifest.corpus.name.clone(),
            corpus_version: manifest.corpus.version.clone(),
            language: language.to_string(),
            corpus_sha256: manifest.corpus.sha256.clone(),
            memory_model: manifest.memory_model.model.clone(),
            memory_model_kind: manifest.memory_model.kind.clone(),
            memory_temperature: manifest.memory_model.temperature,
            memory_max_tokens: manifest.memory_model.max_tokens,
            embedding_fingerprint: manifest.embedding.fingerprint.clone(),
            embedding_dimension: manifest.embedding.dimension,
            prompt_template_version: manifest.pipeline.prompt_template_version.clone(),
            schema_version: manifest.pipeline.schema_version,
            index_status: index_status.to_string(),
            device_os: std::env::consts::OS.to_string(),
            device_arch: std::env::consts::ARCH.to_string(),
            device_cores: std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(0),
        }
    }
}

// =========================================================
// 构造
// =========================================================

impl RunItemRecord {
    /// 成功行。
    ///
    /// 参数:
    /// - `item`: 题目；
    /// - `condition`: 条件；
    /// - `outcome`: 生成结果（回复 + 召回观测）；
    /// - `elapsed_ms`: 本题耗时（含重试）；
    /// - `attempts`: 累计尝试次数；
    /// - `line`: 条件行。
    pub fn success(
        item: &RunItem,
        condition: &Condition,
        outcome: EvalChatOutcome,
        elapsed_ms: u64,
        attempts: u32,
        effective: &str,
        line: ConditionLine,
    ) -> Self {
        Self {
            question_id: item.row.question_id.clone(),
            condition_id: condition.id.clone(),
            tier: item.tier.clone(),
            pipeline: condition.pipeline.as_str().to_string(),
            budget_chars: condition.budget_chars,
            answer_model: condition.answer_model.model.clone(),
            answer_model_effective: effective.to_string(),
            sample_key: item.row.sample_key.clone(),
            persona_uid: item.persona_uid.clone(),
            ask_time: item.row.ask_time.clone(),
            as_of_ms: item.row.ask_time_ms,
            question: item.row.question.clone(),
            gold_answer: item.row.answer.clone(),
            gold_message_ids: item.row.gold_message_ids.clone(),
            category: item.row.category.clone(),
            reply_chars: outcome.chars,
            reply: Some(outcome.reply),
            recall: Some(outcome.recall),
            elapsed_ms,
            error: None,
            attempts,
            conditions: line,
        }
    }

    /// 失败行。
    ///
    /// 参数:
    /// - `item` / `condition` / `line`: 同成功行；
    /// - `error`: 失败原因（面向判分与排查，不含回复正文）；
    /// - `elapsed_ms`: 本题耗时（含重试）；
    /// - `attempts`: 累计尝试次数（条件级失败为 0）。
    pub fn failure(
        item: &RunItem,
        condition: &Condition,
        error: &str,
        elapsed_ms: u64,
        attempts: u32,
        effective: &str,
        line: ConditionLine,
    ) -> Self {
        Self {
            question_id: item.row.question_id.clone(),
            condition_id: condition.id.clone(),
            tier: item.tier.clone(),
            pipeline: condition.pipeline.as_str().to_string(),
            budget_chars: condition.budget_chars,
            answer_model: condition.answer_model.model.clone(),
            answer_model_effective: effective.to_string(),
            sample_key: item.row.sample_key.clone(),
            persona_uid: item.persona_uid.clone(),
            ask_time: item.row.ask_time.clone(),
            as_of_ms: item.row.ask_time_ms,
            question: item.row.question.clone(),
            gold_answer: item.row.answer.clone(),
            gold_message_ids: item.row.gold_message_ids.clone(),
            category: item.row.category.clone(),
            reply: None,
            reply_chars: 0,
            recall: None,
            elapsed_ms,
            error: Some(error.to_string()),
            attempts,
            conditions: line,
        }
    }
}

// =========================================================
// 写出
// =========================================================

/// 追加一行 JSON 到逐题产物（自动补换行并 flush）。
///
/// 参数:
/// - `path`: `items.jsonl` 路径（父目录须已存在）；
/// - `record`: 逐题结果。
///
/// 返回:
/// - 写入成功为 `Ok(())`；序列化或打开 / 写入失败返回对应错误。
pub fn append(path: &Path, record: &RunItemRecord) -> RamariaResult<()> {
    let line = serde_json::to_string(record).map_err(|error| RamariaError::Serialization {
        context: format!("序列化逐题产物失败: {}", record.question_id),
        source: Some(Box::new(error)),
        trace_id: None,
    })?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            RamariaError::io(format!("打开逐题产物失败: {}", path.display()), Some(error))
        })?;
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .map_err(|error| {
            RamariaError::io(format!("写入逐题产物失败: {}", path.display()), Some(error))
        })
}
