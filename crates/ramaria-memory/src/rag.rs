//! rust/crates/ramaria-memory/src/rag.rs — Persona-Aware RAG（检索增强生成）
//!
//! 设计特点:
//! - 在 retriever 的结果上叠加 Persona-Aware 过滤逻辑
//! - rama 类型 persona 拥有全量检索权（不按 share 过滤）
//! - user/char/anim 等类型按 share 阈值过滤敏感内容
//! - 输出格式化上下文文本，直接注入 System Prompt Block C
//! - 纯计算模块，零 I/O，不依赖数据库或异步运行时
//!
//! Persona-Aware 过滤规则:
//! - rama（助手自身）: share 不设阈值，全量检索（用户数据不暴露给第三方时）
//! - user（用户画像查询）: share >= 0.3（过滤高私密事件）
//! - char/anim/oc/hist: share >= 0.5（角色只能看到较公开的信息）
//!
//! 上下文格式化（对接 5-Block System Prompt）:
//! - Block C (记忆上下文): 2-3 段 L1+L2 格式化文字
//! - Block E (参考信息): 图谱实体关系描述

use crate::retriever::SearchResult;
use ramaria_core::types::PersonaKind;

// =========================================================
// 配置
// =========================================================

/// RAG 配置。
#[derive(Debug, Clone)]
pub struct RagConfig {
    /// 是否启用 Persona-Aware 过滤
    pub persona_aware: bool,
    /// user 类型的最低 share 阈值（默认 0.3）
    pub share_threshold_user: f64,
    /// char/anim/oc/hist 类型的最低 share 阈值（默认 0.5）
    pub share_threshold_char: f64,
    /// rama 类型的最低 share 阈值（默认 0.0，即全量）
    pub share_threshold_rama: f64,
    /// 输出的最大记忆条目数
    pub max_memories: usize,
    /// 单条记忆摘要的最大字符数
    pub max_summary_chars: usize,
    /// 是否包含图谱实体
    pub include_graph_entities: bool,
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            persona_aware: true,
            share_threshold_user: 0.3,
            share_threshold_char: 0.5,
            share_threshold_rama: 0.0,
            max_memories: 5,
            max_summary_chars: 120,
            include_graph_entities: true,
        }
    }
}

// =========================================================
// 过滤与上下文构建
// =========================================================

/// 对检索结果执行 Persona-Aware 过滤。
///
/// 过滤规则:
/// - 若 result.share 存在且 < min_share，则过滤掉
/// - 若 result.share 为 None（L1 文档），不按 share 过滤
/// - rama 类型 min_share=0.0，即全量通过
///
/// 返回过滤后的结果列表。
pub fn filter_by_persona<'a>(
    results: &'a [SearchResult],
    persona_kind: PersonaKind,
    config: &RagConfig,
) -> Vec<&'a SearchResult> {
    if !config.persona_aware {
        return results.iter().collect();
    }

    let min_share = persona_kind.min_share(
        config.share_threshold_rama,
        config.share_threshold_user,
        config.share_threshold_char,
    );

    results
        .iter()
        .filter(|r| {
            match r.share {
                Some(share) => share >= min_share,
                None => true, // L1 文档无 share 字段，全部通过
            }
        })
        .collect()
}

/// 将过滤后的检索结果格式化为上下文文本（供 System Prompt Block C 使用）。
///
/// 格式:
/// ```text
/// [相关记忆]
/// 1. (L1) 用户今天学习了Rust编程...
/// 2. (L2) 完成Rust项目 - 用户完成了第一个Rust项目...
/// 3. [图谱] Python - 关联: 机器学习, 数据清洗
/// ```
pub fn format_context_text(results: &[&SearchResult], config: &RagConfig) -> String {
    if results.is_empty() {
        return "[相关记忆]\n（无）".to_string();
    }

    let mut lines = Vec::with_capacity(results.len() + 2);
    lines.push("[相关记忆]".to_string());

    for (i, result) in results.iter().enumerate() {
        if i >= config.max_memories {
            break;
        }

        let prefix = match result.layer.as_str() {
            "l1" => "(L1)",
            "l2" => "(L2)",
            _ => "(?)",
        };

        let mut summary = result.doc_summary.replace('\n', " ").replace('\r', "");
        if summary.chars().count() > config.max_summary_chars {
            summary = summary
                .chars()
                .take(config.max_summary_chars)
                .collect::<String>();
            summary.push('…');
        }

        let score_info = format!(" [score={:.2}]", result.rrf_score);

        lines.push(format!("{}. {} {}{}", i + 1, prefix, summary, score_info));
    }

    lines.join("\n")
}

/// 从检索结果中提取图谱实体列表（供 System Prompt Block E 使用）。
///
/// 格式:
/// ```text
/// [知识图谱]
/// - Python (模块): 用于机器学习的数据分析
/// - 机器学习 (项目): 关联 Python, 数据清洗
/// ```
pub fn format_graph_context(results: &[&SearchResult], config: &RagConfig) -> Option<String> {
    if !config.include_graph_entities {
        return None;
    }

    let entities: Vec<&str> = results
        .iter()
        .filter(|r| r.doc_summary.starts_with("[图谱实体]"))
        .map(|r| r.doc_summary.as_str())
        .collect();

    if entities.is_empty() {
        return None;
    }

    let mut lines = Vec::with_capacity(entities.len() + 2);
    lines.push("[知识图谱]".to_string());

    for (i, entity) in entities.iter().enumerate() {
        // 去掉 "[图谱实体] " 前缀
        let clean = entity.strip_prefix("[图谱实体] ").unwrap_or(entity);
        lines.push(format!("{}. {}", i + 1, clean));
    }

    Some(lines.join("\n"))
}

/// 一站式 RAG 上下文组装。
///
/// 流程:
/// 1. 按 persona_kind 过滤结果
/// 2. 格式化为上下文文本
/// 3. 可选地附加图谱实体
///
/// 返回完整上下文字符串，可直接注入 System Prompt。
pub fn assemble_rag_context(
    results: &[SearchResult],
    persona_uid: &str,
    config: &RagConfig,
) -> String {
    let kind = PersonaKind::from_uid(persona_uid);
    let filtered = filter_by_persona(results, kind, config);

    let mut context = format_context_text(&filtered, config);

    if let Some(graph) = format_graph_context(&filtered, config) {
        context.push('\n');
        context.push('\n');
        context.push_str(&graph);
    }

    context
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bm25::DocId;

    fn make_test_result(
        summary: &str,
        layer: &str,
        share: Option<f64>,
        score: f64,
    ) -> SearchResult {
        SearchResult {
            doc_id: if layer == "l1" {
                DocId::L1(uuid::Uuid::new_v4())
            } else {
                DocId::L2(1)
            },
            layer: layer.to_string(),
            rrf_score: score,
            bm25_score: Some(score),
            vector_score: None,
            graph_score: None,
            persona_uid: Some("user-0001".to_string()),
            share,
            created_at: 1000,
            doc_summary: summary.to_string(),
        }
    }

    // ---- PersonaKind ----

    #[test]
    fn persona_kind_from_uid() {
        assert_eq!(PersonaKind::from_uid("rama-0001"), PersonaKind::Rama);
        assert_eq!(PersonaKind::from_uid("user-0001"), PersonaKind::User);
        assert_eq!(PersonaKind::from_uid("char-0003"), PersonaKind::Char);
        assert_eq!(PersonaKind::from_uid("oc-0001"), PersonaKind::Oc);
        assert_eq!(PersonaKind::from_uid("hist-0002"), PersonaKind::Hist);
        // 未知前缀保守回退为 Char
        assert_eq!(PersonaKind::from_uid("unknown-0001"), PersonaKind::Char);
    }

    #[test]
    fn persona_min_share() {
        let config = RagConfig::default();
        // Rama 全量 by default (0.0)
        assert!(
            (PersonaKind::Rama.min_share(
                config.share_threshold_rama,
                config.share_threshold_user,
                config.share_threshold_char
            ) - 0.0)
                .abs()
                < f64::EPSILON
        );
        assert!(
            (PersonaKind::User.min_share(
                config.share_threshold_rama,
                config.share_threshold_user,
                config.share_threshold_char
            ) - 0.3)
                .abs()
                < f64::EPSILON
        );
        // Char/Anim/Oc/Hist all use char_threshold
        for kind in [
            PersonaKind::Char,
            PersonaKind::Anim,
            PersonaKind::Oc,
            PersonaKind::Hist,
        ] {
            assert!(
                (kind.min_share(
                    config.share_threshold_rama,
                    config.share_threshold_user,
                    config.share_threshold_char
                ) - 0.5)
                    .abs()
                    < f64::EPSILON
            );
        }
    }

    // ---- filter_by_persona ----

    #[test]
    fn filter_rama_sees_all() {
        let config = RagConfig::default();
        let results = vec![
            make_test_result("私密内容", "l2", Some(0.1), 0.9),
            make_test_result("公开内容", "l2", Some(0.9), 0.8),
            make_test_result("L1无share", "l1", None, 0.7),
        ];
        let filtered = filter_by_persona(&results, PersonaKind::Rama, &config);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn filter_user_hides_low_share() {
        let config = RagConfig::default();
        let results = vec![
            make_test_result("私密", "l2", Some(0.1), 0.9), // share=0.1 < 0.3 → 过滤
            make_test_result("公开", "l2", Some(0.5), 0.8), // share=0.5 ≥ 0.3 → 保留
            make_test_result("L1", "l1", None, 0.7),        // no share → 保留
        ];
        let filtered = filter_by_persona(&results, PersonaKind::User, &config);
        assert_eq!(filtered.len(), 2);
        assert!(!filtered.iter().any(|r| r.doc_summary == "私密"));
    }

    #[test]
    fn filter_character_strict() {
        let config = RagConfig::default();
        let results = vec![
            make_test_result("私密", "l2", Some(0.3), 0.9), // 0.3 < 0.5 → 过滤
            make_test_result("公开", "l2", Some(0.7), 0.8), // 0.7 ≥ 0.5 → 保留
        ];
        let filtered = filter_by_persona(&results, PersonaKind::Char, &config);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].doc_summary, "公开");
    }

    #[test]
    fn filter_disabled_persona_aware() {
        let mut config = RagConfig::default();
        config.persona_aware = false;

        let results = vec![make_test_result("超私密", "l2", Some(0.01), 0.9)];
        let filtered = filter_by_persona(&results, PersonaKind::Char, &config);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_share_threshold_boundary() {
        let config = RagConfig::default();
        // share = 0.3 刚好在 user 阈值上
        let results = vec![make_test_result("边界", "l2", Some(0.3), 0.9)];
        let filtered = filter_by_persona(&results, PersonaKind::User, &config);
        assert_eq!(filtered.len(), 1);
    }

    // ---- format_context_text ----

    #[test]
    fn format_context_produces_structured_output() {
        let config = RagConfig::default();
        let r1 = make_test_result("今天学习了Rust", "l1", None, 0.9);
        let r2 = make_test_result("完成Rust项目", "l2", Some(0.8), 0.85);
        let results = vec![&r1, &r2];
        let text = format_context_text(&results, &config);
        assert!(text.contains("[相关记忆]"));
        assert!(text.contains("(L1)"));
        assert!(text.contains("(L2)"));
        assert!(text.contains("今天学习了Rust"));
        assert!(text.contains("完成Rust项目"));
    }

    #[test]
    fn format_context_empty() {
        let config = RagConfig::default();
        let empty: Vec<&SearchResult> = vec![];
        let text = format_context_text(&empty, &config);
        assert!(text.contains("（无）"));
    }

    #[test]
    fn format_context_max_memories() {
        let mut config = RagConfig::default();
        config.max_memories = 2;

        let results: Vec<SearchResult> = (0..5)
            .map(|i| make_test_result(&format!("文档{}", i), "l1", None, 1.0 - i as f64 * 0.1))
            .collect();
        let refs: Vec<&SearchResult> = results.iter().collect();
        let text = format_context_text(&refs, &config);

        // 应有 2 条文档
        assert!(text.contains("1. "));
        assert!(text.contains("2. "));
        assert!(!text.contains("3. "));
    }

    #[test]
    fn format_context_long_summary_truncation() {
        let mut config = RagConfig::default();
        config.max_summary_chars = 10;

        let long = "这是一段非常长的描述文本需要被截断";
        let r = make_test_result(long, "l1", None, 0.9);
        let results = vec![&r];
        let text = format_context_text(&results, &config);

        // 截断后后应有省略号
        assert!(text.contains('…'));
    }

    // ---- format_graph_context ----

    #[test]
    fn format_graph_context_with_entities() {
        let config = RagConfig::default();
        let r1 = make_test_result("正常记忆", "l1", None, 0.9);
        let r2 = SearchResult {
            doc_id: DocId::Graph("Python".to_string()),
            layer: "graph".to_string(),
            rrf_score: 0.5,
            bm25_score: None,
            vector_score: None,
            graph_score: Some(0.5),
            persona_uid: None,
            share: None,
            created_at: 0,
            doc_summary: "[图谱实体] Python".to_string(),
        };
        let results = vec![&r1, &r2];
        let graph = format_graph_context(&results, &config);
        assert!(graph.is_some());
        let text = graph.unwrap();
        assert!(text.contains("[知识图谱]"));
        assert!(text.contains("Python"));
    }

    #[test]
    fn format_graph_context_no_entities() {
        let config = RagConfig::default();
        let r = make_test_result("正常记忆", "l1", None, 0.9);
        let results = vec![&r];
        let graph = format_graph_context(&results, &config);
        assert!(graph.is_none());
    }

    #[test]
    fn format_graph_context_disabled() {
        let mut config = RagConfig::default();
        config.include_graph_entities = false;

        let r = SearchResult {
            doc_id: DocId::Graph("Python".to_string()),
            layer: "graph".to_string(),
            rrf_score: 0.5,
            bm25_score: None,
            vector_score: None,
            graph_score: Some(0.5),
            persona_uid: None,
            share: None,
            created_at: 0,
            doc_summary: "[图谱实体] Python".to_string(),
        };
        let results = vec![&r];
        assert!(format_graph_context(&results, &config).is_none());
    }

    // ---- assemble_rag_context ----

    #[test]
    fn assemble_rag_context_rama_sees_all() {
        let config = RagConfig::default();
        let results = vec![
            make_test_result("私密内容", "l2", Some(0.1), 0.9),
            make_test_result("公开内容", "l2", Some(0.9), 0.8),
        ];
        let context = assemble_rag_context(&results, "rama-0001", &config);
        assert!(context.contains("私密内容"));
        assert!(context.contains("公开内容"));
    }

    #[test]
    fn assemble_rag_context_user_filtered() {
        let config = RagConfig::default();
        let results = vec![
            make_test_result("私密内容", "l2", Some(0.1), 0.9),
            make_test_result("公开内容", "l2", Some(0.9), 0.8),
        ];
        let context = assemble_rag_context(&results, "user-0001", &config);
        assert!(!context.contains("私密内容"));
        assert!(context.contains("公开内容"));
    }

    #[test]
    fn assemble_rag_context_includes_graph() {
        let mut config = RagConfig::default();
        config.max_memories = 5;

        let mut results: Vec<SearchResult> = (0..3)
            .map(|i| make_test_result(&format!("文档{}", i), "l1", None, 0.9))
            .collect();
        results.push(SearchResult {
            doc_id: DocId::Graph("Python".to_string()),
            layer: "graph".to_string(),
            rrf_score: 0.5,
            bm25_score: None,
            vector_score: None,
            graph_score: Some(0.5),
            persona_uid: None,
            share: None,
            created_at: 0,
            doc_summary: "[图谱实体] Python".to_string(),
        });

        let context = assemble_rag_context(&results, "rama-0001", &config);
        assert!(context.contains("[相关记忆]"));
        assert!(context.contains("[知识图谱]"));
    }
}
