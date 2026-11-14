//! crates/ramaria-eval/src/tui/console/review/source.rs - Ramaria 审查屏原对话片段
//!
//! 设计特点:
//! - 只读本地语料：按运行条件拼路径，不联网、不读基线库消息
//! - 按 `sample_key` 命中样本，按 evidence 的 artifact 标识命中消息
//! - 每处命中取前后各两条消息；同一 session 按证据簇合并重叠 / 相邻区间，不相邻块用 … 分隔
//! - 证据行前缀 `>> ` 并标记高亮，普通行保留说话人
//! - 空 evidence（不可回答题）给说明行，不当作错误

use std::collections::HashSet;
use std::path::PathBuf;

use ramaria_core::error::RamariaResult;

use crate::corpus::{CorpusLanguage, Sample};

#[cfg(test)]
mod tests;

/// 证据行前后保留的消息条数。
const CONTEXT: usize = 2;

// =========================================================
// 语料请求
// =========================================================

/// 一次原对话加载请求。
///
/// 字段约定:
/// - `root`: 评测根（当前工作目录）；
/// - `name` / `version` / `language`: 运行时条件行里的语料口径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CorpusRequest {
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) language: String,
}

impl CorpusRequest {
    /// 缓存键：语料名 / 版本 / 语言。
    pub(crate) fn key(&self) -> String {
        format!("{}/{}/{}", self.name, self.version, self.language)
    }

    /// 期望的语料文件路径。
    pub(crate) fn path(&self) -> RamariaResult<PathBuf> {
        corpus_path(self)
    }
}

/// 拼出语料文件路径：`<root>/.eval/corpus/<name>/<version>/<语言文件>`。
///
/// 参数:
/// - `request`: 语料请求。
///
/// 返回:
/// - 成功时返回期望路径；
/// - 语言标签未知时返回 `Validation`。
pub(crate) fn corpus_path(request: &CorpusRequest) -> RamariaResult<PathBuf> {
    let language = CorpusLanguage::parse(&request.language)?;
    Ok(request
        .root
        .join(".eval")
        .join("corpus")
        .join(&request.name)
        .join(&request.version)
        .join(language.file_name()))
}

// =========================================================
// 片段
// =========================================================

/// 原对话片段中的一行。
///
/// 字段约定:
/// - `text`: 已排版的一行文本；
/// - `highlight`: 是否为证据命中行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceLine {
    pub(crate) text: String,
    pub(crate) highlight: bool,
}

/// 不可回答题的提示片段（无证据时直接展示，不加载语料）。
pub(crate) fn no_evidence_lines() -> Vec<SourceLine> {
    vec![SourceLine::normal("本题无证据（不可回答题）".to_string())]
}

impl SourceLine {
    /// 普通行。
    fn normal(text: String) -> Self {
        Self {
            text,
            highlight: false,
        }
    }

    /// 证据行。
    fn evidence(text: String) -> Self {
        Self {
            text,
            highlight: true,
        }
    }
}

/// 从语料样本中取出一道题的原对话片段。
///
/// 参数:
/// - `samples`: 语料全部样本；
/// - `sample_key`: 题目所属样本（`Sample::user_key`）；
/// - `evidence`: 题目的证据 artifact 标识集合。
///
/// 返回:
/// - 成功时返回片段行（可能只有一条说明行）；
/// - 样本不存在返回错误文本。
///
/// 说明: 每处命中取前后各两条消息，同一 session 按证据簇合并重叠或相邻区间；
/// 证据不在该样本的原对话中时只给提示，不报错。
pub(crate) fn build_snippet(
    samples: &[Sample],
    sample_key: &str,
    evidence: &[String],
) -> Result<Vec<SourceLine>, String> {
    let sample = samples
        .iter()
        .find(|sample| sample.user_key == sample_key)
        .ok_or_else(|| format!("语料中未找到样本: {sample_key}"))?;

    if evidence.is_empty() {
        return Ok(no_evidence_lines());
    }
    let wanted: HashSet<&str> = evidence.iter().map(String::as_str).collect();

    let mut lines = Vec::new();
    for session in &sample.sessions {
        let hits: Vec<usize> = session
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| wanted.contains(message.artifact_id().as_str()))
            .map(|(index, _)| index)
            .collect();
        if hits.is_empty() {
            continue;
        }
        // 同一 session 内只合并 ±CONTEXT 后重叠或相邻的区间
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for hit in hits.iter().copied() {
            let start = hit.saturating_sub(CONTEXT);
            let end = (hit + CONTEXT).min(session.messages.len() - 1);
            match blocks.last_mut() {
                Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
                _ => blocks.push((start, end)),
            }
        }
        let mut emitted = false;
        for (start, end) in blocks {
            if emitted {
                lines.push(SourceLine::normal("…".to_string()));
            }
            emitted = true;
            lines.push(SourceLine::normal(format!(
                "—— session {} · {} ——",
                session.index, session.date
            )));
            for index in start..=end {
                let message = &session.messages[index];
                let text = format!("{}: {}", message.speaker, message.text);
                if hits.contains(&index) {
                    lines.push(SourceLine::evidence(format!(">> {text}")));
                } else {
                    lines.push(SourceLine::normal(format!("   {text}")));
                }
            }
        }
    }

    if lines.is_empty() {
        lines.push(SourceLine::normal(
            "证据不在该样本原对话中（可能超出建库窗口）".to_string(),
        ));
    }
    Ok(lines)
}
