//! crates/ramaria-memory/src/retriever/helpers.rs — 检索结果解析辅助函数
//!
//! 设计特点:
//! - 提供 BM25 文档预解析、图谱/向量 label 解析与关键词命中计数
//! - 均为纯函数，仅在父模块内（`pub(super)`）供检索编排逻辑调用
//! - 不访问 Retriever 字段，文档映射以参数注入

use crate::bm25::DocId;

// =========================================================
// 辅助函数
// =========================================================

/// BM25 文档预解析结果——在 BM25 搜索阶段一次解析，避免 RRF 融合后重复解析。
///
/// 存储 label→DocId 的映射以及关联的文档元数据，
/// 省去 `parse_doc_label` 中的 UUID 字符串解析开销。
pub(super) struct Bm25Resolved {
    pub(super) doc_id: DocId,
    pub(super) persona_uid: Option<String>,
    pub(super) share: Option<f64>,
    pub(super) created_at: i64,
    pub(super) last_accessed_at: Option<i64>,
    pub(super) summary: String,
    pub(super) layer: String,
}

/// 从 BM25 搜索阶段的 DocId 直接解析文档数据（无需字符串往返）。
///
/// 返回预解析的文档数据，存储在 `Bm25Resolved` 中供 RRF 融合后直接使用。
pub(super) fn resolve_bm25_doc(
    doc_id: &DocId,
    l1_docs: &std::collections::HashMap<uuid::Uuid, super::types::L1DocView>,
    l2_docs: &std::collections::HashMap<i64, super::types::L2DocView>,
) -> Bm25Resolved {
    match doc_id {
        DocId::L1(id) => {
            if let Some(doc) = l1_docs.get(id) {
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: doc.persona_uid.clone(),
                    share: None,
                    created_at: doc.created_at,
                    last_accessed_at: doc.last_accessed_at,
                    summary: doc.summary.clone(),
                    layer: "l1".to_string(),
                }
            } else {
                // 文档已被移除，返回哨兵数据（上层会检查并跳过）
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: None,
                    share: None,
                    created_at: 0,
                    last_accessed_at: None,
                    summary: "[已删除]".to_string(),
                    layer: "l1".to_string(),
                }
            }
        }
        DocId::L2(id) => {
            if let Some(doc) = l2_docs.get(id) {
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: Some(doc.persona_uid.clone()),
                    share: Some(doc.share),
                    created_at: doc.created_at,
                    last_accessed_at: None, // L2 事件暂不追踪访问时间
                    summary: format!("{} — {}", doc.title, doc.summary),
                    layer: "l2".to_string(),
                }
            } else {
                Bm25Resolved {
                    doc_id: doc_id.clone(),
                    persona_uid: None,
                    share: None,
                    created_at: 0,
                    last_accessed_at: None,
                    summary: "[已删除]".to_string(),
                    layer: "l2".to_string(),
                }
            }
        }
        DocId::Graph(_) => {
            // 图谱实体不应出现在 BM25 结果中，但若出现则做防御处理
            Bm25Resolved {
                doc_id: doc_id.clone(),
                persona_uid: None,
                share: None,
                created_at: 0,
                last_accessed_at: None,
                summary: format!("[图谱实体] {}", doc_id),
                layer: "graph".to_string(),
            }
        }
    }
}

/// 从图谱通道的 "graph:{entity}" label 解析出 SearchResult 所需数据。
///
/// 图谱实体没有对应数据库记录，因此返回实体名作为摘要。
///
/// 返回: 元组 (DocId::Graph, None, None, 0, None, "[图谱实体] {name}", "graph")
#[allow(clippy::type_complexity)]
pub(super) fn parse_graph_label(
    label: &str,
) -> Option<(
    DocId,
    Option<String>,
    Option<f64>,
    i64,
    Option<i64>,
    String,
    String,
)> {
    label.strip_prefix("graph:").map(|entity| {
        (
            DocId::Graph(entity.to_string()),
            None, // persona_uid（图谱实体不关联特定 persona）
            None, // share
            0,    // created_at（图谱实体无时间戳）
            None, // last_accessed_at（图谱实体无访问时间）
            format!("[图谱实体] {}", entity),
            "graph".to_string(),
        )
    })
}

/// 从 RRF 融合结果的 label 解析出 doc_id 和文档摘要。
///
/// 仅用于向量通道产生的 L1:/L2: 格式 label（BM25 已通过 `resolve_bm25_doc` 预解析，
/// 图谱已通过 `parse_graph_label` 独立处理）。
///
/// 返回: 元组 (DocId, persona_uid, share, created_at, last_accessed_at, summary)
#[allow(clippy::type_complexity)]
pub(super) fn parse_doc_label(
    label: &str,
    l1_docs: &std::collections::HashMap<uuid::Uuid, super::types::L1DocView>,
    l2_docs: &std::collections::HashMap<i64, super::types::L2DocView>,
) -> Option<(DocId, Option<String>, Option<f64>, i64, Option<i64>, String)> {
    if let Some(uuid_str) = label
        .strip_prefix("L1:")
        .or_else(|| label.strip_prefix("l1:"))
    {
        let id = uuid::Uuid::parse_str(uuid_str).ok()?;
        let doc = l1_docs.get(&id)?;
        Some((
            DocId::L1(id),
            doc.persona_uid.clone(),
            None, // L1 无 share
            doc.created_at,
            doc.last_accessed_at,
            doc.summary.clone(),
        ))
    } else if let Some(id_str) = label
        .strip_prefix("L2:")
        .or_else(|| label.strip_prefix("l2:"))
    {
        let id = id_str.parse::<i64>().ok()?;
        let doc = l2_docs.get(&id)?;
        Some((
            DocId::L2(id),
            Some(doc.persona_uid.clone()),
            Some(doc.share),
            doc.created_at,
            None, // L2 事件暂不追踪访问时间
            format!("{} — {}", doc.title, doc.summary),
        ))
    } else {
        // 未知格式 label（不应出现，但做防御处理）
        None
    }
}

/// 统计文档关键词字段中匹配到的查询关键词数量。
///
/// 参数:
/// - `doc_keywords`: 文档的逗号分隔关键词字符串（如 "工作, 压力, 倦怠"）。
/// - `query_keywords`: 查询关键词列表。
///
/// 返回:
/// - 精确命中的关键词数量。
pub(super) fn count_keyword_matches(
    doc_keywords: Option<&str>,
    query_keywords: &[ramaria_core::keyword::KeywordToken],
) -> usize {
    let doc_str = match doc_keywords {
        Some(s) => s,
        None => return 0,
    };

    // 解析文档关键词为去重集合，做与 KeywordToken::new() 一致的规范化（trim + 英文小写）
    let doc_kw_set: std::collections::HashSet<String> = doc_str
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    if doc_kw_set.is_empty() {
        return 0;
    }

    query_keywords
        .iter()
        .filter(|qk| doc_kw_set.contains(qk.as_str()))
        .count()
}
