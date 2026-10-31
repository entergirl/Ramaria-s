//! crates/ramaria-eval/src/corpus/lifebench/align.rs - Ramaria LifeBench 中英对齐
//!
//! 设计特点:
//! - 对齐键固定为 question_id，sample 顺序与 QA 顺序都必须一致
//! - 只比较 gold 字段：question_id / evidence / ask_time / category / 分值
//! - sample_id、人名与正文是译文，不参与对齐
//! - 首个不一致即返回带 sample 与 QA 定位的 `Validation`

use ramaria_core::error::{RamariaError, RamariaResult};

use super::{Qa, Sample};

/// 中英对齐结果。
///
/// 字段约定:
/// - `samples`: 对齐通过的 sample 数；
/// - `qa_count`: 逐题比较通过的 QA 数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignmentReport {
    pub samples: usize,
    pub qa_count: usize,
}

/// 逐 sample、逐 QA 比较中英两份语料的 gold 字段。
///
/// 参数:
/// - `zh`: 中文版 samples；
/// - `en`: 英文版 samples，顺序需与中文版一致。
///
/// 返回:
/// - 全部一致返回报告；数量、顺序或任一 gold 字段不一致返回 `Validation`。
pub fn align(zh: &[Sample], en: &[Sample]) -> RamariaResult<AlignmentReport> {
    if zh.len() != en.len() {
        return Err(RamariaError::validation(format!(
            "中英语料 sample 数不一致: {} / {}",
            zh.len(),
            en.len()
        )));
    }

    let mut qa_count = 0;
    for (sample_index, (zh_sample, en_sample)) in zh.iter().zip(en.iter()).enumerate() {
        if zh_sample.qa.len() != en_sample.qa.len() {
            return Err(RamariaError::validation(format!(
                "中英语料 sample #{sample_index} 的 QA 数不一致: {} / {}（zh {} / en {}）",
                zh_sample.qa.len(),
                en_sample.qa.len(),
                zh_sample.sample_id,
                en_sample.sample_id
            )));
        }
        for (qa_index, (zh_qa, en_qa)) in zh_sample.qa.iter().zip(en_sample.qa.iter()).enumerate() {
            compare_qa(sample_index, qa_index, zh_qa, en_qa)?;
            qa_count += 1;
        }
    }

    Ok(AlignmentReport {
        samples: zh.len(),
        qa_count,
    })
}

fn compare_qa(sample_index: usize, qa_index: usize, zh: &Qa, en: &Qa) -> RamariaResult<()> {
    let mut mismatches = Vec::new();
    if zh.question_id != en.question_id {
        mismatches.push(format!(
            "question_id（zh {} / en {}）",
            zh.question_id, en.question_id
        ));
    }
    if zh.evidence != en.evidence {
        mismatches.push(format!(
            "evidence（zh {:?} / en {:?}）",
            zh.evidence, en.evidence
        ));
    }
    if zh.ask_time != en.ask_time {
        mismatches.push(format!(
            "ask_time（zh {} / en {}）",
            zh.ask_time, en.ask_time
        ));
    }
    if zh.category != en.category {
        mismatches.push(format!(
            "category（zh {:?} / en {:?}）",
            zh.category, en.category
        ));
    }
    let zh_scores: Vec<f64> = zh.score_points.iter().map(|point| point.score).collect();
    let en_scores: Vec<f64> = en.score_points.iter().map(|point| point.score).collect();
    if zh_scores != en_scores {
        mismatches.push(format!(
            "score_points（zh {zh_scores:?} / en {en_scores:?}）"
        ));
    }

    if mismatches.is_empty() {
        return Ok(());
    }
    Err(RamariaError::validation(format!(
        "中英语料不对齐: sample #{sample_index} 的 QA #{qa_index} 字段不一致: {}",
        mismatches.join("；")
    )))
}
