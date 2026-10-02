//! crates/ramaria-memory/src/event/batcher/item.rs - L1 聚类专用精简视图
//!
//! 设计特点:
//! - `L1Item` 为 `MemoryL1` 的聚类专用精简视图，通过 `From` trait 转换
//! - 关键词从逗号分隔字符串解析为 `KeywordToken`
//! - embedding 字段由上层注入（从向量索引查询后挂载）
//! - `semantic_text` 组装 S2 语义增强输入（summary + evidence_notes + keywords）
//! - 纯数据结构与纯计算，不依赖 LLM 或数据库

use ramaria_core::keyword::KeywordToken;
use ramaria_core::types::{EvidenceNote, MemoryL1};
use uuid::Uuid;

// =========================================================
// L1Item — 聚类专用精简视图
// =========================================================

/// L1 摘要的聚类专用视图。
///
/// 职责:
/// - 从 `MemoryL1` 精简提取 TopicBatcher 所需的字段，降低聚类过程中的内存占用。
/// - 关键词从逗号分隔字符串解析为 `Vec<KeywordToken>`。
/// - embedding 字段由上层注入（从向量索引查询后挂载）。
///
/// 字段约定:
/// - `keywords`: 标准化后的关键词列表（已通过 `KeywordToken::new()` 过滤）。
/// - `evidence_notes`: 结构化证据线索（v1.4，供 S2 语义增强输入组装）。
/// - `embedding`: L1 摘要文本的向量表示（384 维），None 表示未配置嵌入模型。
#[derive(Debug, Clone)]
pub struct L1Item {
    pub id: Uuid,
    pub summary: String,
    pub keywords: Vec<KeywordToken>,
    /// 结构化证据线索（v1.4 M4 起参与语义增强输入组装）
    pub evidence_notes: Vec<EvidenceNote>,
    pub embedding: Option<Vec<f32>>,
    pub salience: f64,
    pub created_at: i64,
}

impl From<&MemoryL1> for L1Item {
    /// 从 MemoryL1 构造聚类用精简视图。
    ///
    /// 说明:
    /// - 关键词从 `keywords` 字段解析，通过 `KeywordToken::new()` 标准化。
    /// - evidence_notes 直接复制结构化线索（缺失/为空 → 空 Vec）。
    /// - embedding 初始化为 None，由上层在构建图之前注入。
    fn from(l1: &MemoryL1) -> Self {
        let keywords = l1
            .keywords
            .as_deref()
            .map(|kw_str| {
                kw_str
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .filter_map(KeywordToken::new)
                    .collect()
            })
            .unwrap_or_default();

        Self {
            id: l1.id,
            summary: l1.summary.clone(),
            keywords,
            evidence_notes: l1.evidence_notes.clone().unwrap_or_default(),
            embedding: None,
            salience: l1.salience,
            created_at: l1.created_at,
        }
    }
}

impl L1Item {
    /// 组装 S2 语义增强的 embedding 输入文本（v3.1 §5：summary + evidence_notes + keywords）。
    ///
    /// 设计:
    /// - 上层用此文本调用 embedding provider 生成向量后挂载到 `embedding` 字段。
    /// - evidence_notes 为空时自动退化为 `summary + keywords`，保证无线索时输入形态稳定。
    /// - 结构化槽位（time/who/cause）按"槽位名: 值"拼接，供模型感知因果线索语义。
    ///
    /// 返回:
    /// - 拼接后的语义文本（单行，各段以空格分隔）。
    pub fn semantic_text(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(3);
        parts.push(self.summary.trim().to_string());

        // 证据线索段：仅取 text 槽位与可选槽位（time/who/cause），逐条拼接
        let evidence_part: Vec<String> = self
            .evidence_notes
            .iter()
            .map(|note| {
                let mut seg = note.text.trim().to_string();
                if let Some(time) = note.time.as_deref() {
                    seg.push_str(&format!(" time: {time}"));
                }
                if let Some(who) = note.who.as_deref() {
                    seg.push_str(&format!(" who: {who}"));
                }
                if let Some(cause) = note.cause.as_deref() {
                    seg.push_str(&format!(" cause: {cause}"));
                }
                seg
            })
            .collect();
        if !evidence_part.is_empty() {
            parts.push(evidence_part.join(" ; "));
        }

        // 关键词段
        if !self.keywords.is_empty() {
            let kw_joined: Vec<&str> = self.keywords.iter().map(|k| k.as_str()).collect();
            parts.push(kw_joined.join(" "));
        }

        parts.join(" ")
    }
}
