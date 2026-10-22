//! crates/ramaria-cli/src/commands/probe/dataset/build/from_file.rs - 探针数据集文件源构建
//!
//! 设计特点:
//! - 解析数据源 JSON（messages / events）并做确定性抽样。
//! - 与数据库路径共用夹具兜底与三维题项装配。

use super::super::fixture::{
    fixture_emotion_pairs, fixture_fact_events, fixture_tone_pairs, has_emotion_cue,
    sample_with_fallback,
};
use super::default_variants;
use crate::commands::probe::now_iso8601;
use crate::commands::probe::types::{
    ContextTurn, DATASET_SCHEMA_VERSION, DatasetItem, ItemRegister, ProbeDataset,
};
use ramaria_core::error::RamariaError;
use std::path::Path;

/// 从显式数据源文件构建测试集。
///
/// 输入文件格式（JSON）:
/// ```json
/// {
///   "persona_uid": "char-0001",
///   "messages": [{
///     "question": "...",
///     "reply": "...",
///     "source_ref": "...",
///     "context": [{"role": "user", "content": "..."}]
///   }],
///   "events":   [{"title": "...", "summary": "..."}]
/// }
/// ```
///
/// 说明: `messages[].context` 为该 question 之前紧邻的上文（时间正序），
/// 可选；缺省为空即不加下文。
pub async fn build_from_file(
    path: &Path,
    persona_uid: &str,
    qpd: usize,
    seed: u64,
) -> anyhow::Result<ProbeDataset> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取数据源文件失败: {}: {e}",
            path.display()
        )))
    })?;
    let raw: ProbeSourceFile = serde_json::from_str(&text).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "解析数据源文件失败: {}: {e}",
            path.display()
        )))
    })?;

    let persona = raw.persona_uid.unwrap_or_else(|| persona_uid.to_string());

    // tone（全部 messages）与 emotion（仅含情感线索的 messages）同源筛选；
    // messages 需要非空 question 才能配对。
    let tone_pairs: Vec<(String, String, Option<String>, Vec<ContextTurn>)> = raw
        .messages
        .iter()
        .filter(|m| !m.question.trim().is_empty())
        .map(|m| {
            (
                m.question.clone(),
                m.reply.clone(),
                m.source_ref.clone(),
                m.context.clone(),
            )
        })
        .collect();
    let emotion_pairs: Vec<(String, String, Option<String>, Vec<ContextTurn>)> = tone_pairs
        .iter()
        .filter(|(q, _, _, _)| has_emotion_cue(q))
        .cloned()
        .collect();
    let fact_cands: Vec<(String, String, String)> = raw
        .events
        .iter()
        .filter(|e| !e.title.trim().is_empty())
        .map(|e| {
            (
                format!("还记得「{}」这件事吗？", e.title),
                e.summary.clone(),
                e.title.clone(),
            )
        })
        .collect();

    let (tone_items, tone_real) = sample_with_fallback(
        &tone_pairs,
        &fixture_tone_pairs()
            .into_iter()
            .map(|(q, r)| (q, r, None, Vec::new()))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );
    let (fact_cands, fact_real) =
        sample_with_fallback(&fact_cands, &fixture_fact_events(), qpd, seed);
    let (emotion_cands, emotion_real) = sample_with_fallback(
        &emotion_pairs,
        &fixture_emotion_pairs()
            .into_iter()
            .map(|(q, r)| (q, r, None, Vec::new()))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );

    let mut items = Vec::with_capacity(qpd * 3);
    for (idx, (question, reference, src_ref, context)) in tone_items.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: if idx < tone_real { "file" } else { "fixture" }.to_string(),
            source_ref: src_ref,
            context,
            register: ItemRegister::Chat,
        });
    }
    for (idx, (question, reference, title)) in fact_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("fact-{:04}", idx + 1),
            dimension: "fact".to_string(),
            question,
            reference: Some(reference),
            source: if idx < fact_real { "file" } else { "fixture" }.to_string(),
            source_ref: Some(title),
            // 事实维为模板化问句，不依赖即时上文
            context: Vec::new(),
            register: ItemRegister::Chat,
        });
    }
    for (idx, (question, reference, src_ref, context)) in emotion_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: if idx < emotion_real {
                "file"
            } else {
                "fixture"
            }
            .to_string(),
            source_ref: src_ref,
            context,
            register: ItemRegister::Chat,
        });
    }

    Ok(ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona,
        dimensions: vec![
            "tone".to_string(),
            "fact".to_string(),
            "emotion".to_string(),
        ],
        questions_per_dimension: qpd,
        source: "file".to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    })
}

/// 数据源文件输入格式（probe build --source）。
#[derive(Debug, serde::Deserialize)]
pub(crate) struct ProbeSourceFile {
    persona_uid: Option<String>,
    #[serde(default)]
    messages: Vec<SourceMessage>,
    #[serde(default)]
    events: Vec<SourceEvent>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct SourceMessage {
    question: String,
    #[serde(default)]
    reply: String,
    #[serde(default)]
    source_ref: Option<String>,
    /// 该 question 之前紧邻的上文（时间正序，可选）。
    #[serde(default)]
    context: Vec<ContextTurn>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct SourceEvent {
    title: String,
    #[serde(default)]
    summary: String,
}
