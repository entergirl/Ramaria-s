//! crates/ramaria-memory/src/event/context_retriever.rs - CompositeIndex 补充上下文检索器
//!
//! 设计特点:
//! - 三级降级编排: 精确匹配 → 子串匹配 → (语义模糊匹配，需 embedding 可用时)
//! - 封装 Retriever 的 `search_exact` / `search_substring` / `search` 方法为统一入口
//! - 不引入数据库依赖——纯内存操作，基于 Retriever 已有的 BM25 索引和文档 HashMap
//! - 为 EventExtractor 的事件提取 Prompt 提供"补充背景"段落
//! - 去重约束: 若历史事件与当前候选高度重叠，调高置信度而非新建事件（由 Prompt 指令约束）
//! - 每级结果不足 top_k 时自动降级到下一级

use ramaria_core::keyword::KeywordToken;

use super::batcher::TopicCluster;
use crate::retriever::{Retriever, SearchResult};

// =========================================================
// ContextDocument — 上下文文档视图
// =========================================================

/// 从检索器返回的补充上下文文档。
///
/// 职责:
/// - 作为事件提取 Prompt 中"补充背景"段落的数据载体。
/// - 携带文档摘要和层级信息，供 Prompt 格式化使用。
#[derive(Debug, Clone)]
pub struct ContextDocument {
    /// 文档摘要文本
    pub summary: String,
    /// 文档层级: "l1" 或 "l2"
    pub layer: String,
    /// 检索通道路径: "exact" / "substring" / "semantic"
    pub source_channel: String,
    /// 检索相关性分数
    pub score: f64,
}

impl From<&SearchResult> for ContextDocument {
    fn from(sr: &SearchResult) -> Self {
        Self {
            summary: sr.doc_summary.clone(),
            layer: sr.layer.clone(),
            source_channel: "unknown".to_string(),
            score: sr.rrf_score,
        }
    }
}

// =========================================================
// ContextRetrieverConfig
// =========================================================

/// ContextRetriever 配置。
#[derive(Debug, Clone)]
pub struct ContextRetrieverConfig {
    /// 三级检索总返回上限
    pub top_k: usize,
    /// Level 1 精确匹配结果数（不足则触发 Level 2）
    pub exact_limit: usize,
    /// Level 2 子串匹配补充量（exact + substring 不足则触发 Level 3）
    pub substring_supplement: usize,
}

impl Default for ContextRetrieverConfig {
    fn default() -> Self {
        Self {
            top_k: 5,
            exact_limit: 5,
            substring_supplement: 5,
        }
    }
}

// =========================================================
// ContextRetriever
// =========================================================

/// CompositeIndex 补充上下文检索器。
///
/// 职责:
/// - 封装 Retriever 的三级检索能力，为事件提取提供历史上下文。
/// - 对每个 TopicCluster 执行三级递进检索，补充与该主题相关的历史 L1/L2 文档。
///
/// 三级编排:
/// 1. **精确匹配** (`search_exact`): 用簇关键词做精确命中，查 L1/L2 的 keywords 字段。
/// 2. **子串匹配** (`search_substring`): 结果不足 top_k 时，用簇关键词拼接文本做 BM25 子串检索。
/// 3. **语义模糊** (暂跳过): embedding 不可用时跳过。由上层在 embedding 可用时通过 `search_semantic` 补充。
///
/// 用法:
/// ```no_run
/// # use ramaria_memory::retriever::Retriever;
/// # use ramaria_memory::event::context_retriever::{ContextRetriever, ContextRetrieverConfig};
/// # use ramaria_memory::event::batcher::TopicCluster;
/// # let retriever = Retriever::new();
/// # let config = ContextRetrieverConfig::default();
/// # let cluster = TopicCluster::new(Vec::new());
/// let ctx_retriever = ContextRetriever::new(&retriever, config);
/// let docs = ctx_retriever.retrieve_context(&cluster, "user-0001");
/// // 将 docs 格式化为 Prompt 中的补充背景段落
/// ```
pub struct ContextRetriever<'a> {
    retriever: &'a Retriever,
    config: ContextRetrieverConfig,
}

impl<'a> ContextRetriever<'a> {
    /// 创建新的 ContextRetriever。
    pub fn new(retriever: &'a Retriever, config: ContextRetrieverConfig) -> Self {
        Self { retriever, config }
    }

    /// 为给定 TopicCluster 检索补充上下文文档。
    ///
    /// 参数:
    /// - `cluster`: TopicBatcher 产出的主题簇。
    /// - `persona_uid`: 目标人格 UID（空字符串表示不过滤）。
    ///
    /// 返回:
    /// - 三级编排后的去重 ContextDocument 列表（最多 `config.top_k` 条）。
    pub fn retrieve_context(
        &self,
        cluster: &TopicCluster,
        persona_uid: &str,
    ) -> Vec<ContextDocument> {
        let mut results: Vec<ContextDocument> = Vec::with_capacity(self.config.top_k);

        // =========================================================
        // Level 1: 精确匹配（关键词完全命中）
        // =========================================================
        if !cluster.cluster_keywords.is_empty() {
            let exact_hits = self.retriever.search_exact(
                &cluster.cluster_keywords,
                persona_uid,
                self.config.exact_limit,
            );

            for hit in &exact_hits {
                results.push(ContextDocument {
                    summary: hit.doc_summary.clone(),
                    layer: hit.layer.clone(),
                    source_channel: "exact".to_string(),
                    score: hit.rrf_score,
                });
            }

            tracing::debug!(
                persona_uid,
                exact_count = exact_hits.len(),
                cluster_kw_count = cluster.cluster_keywords.len(),
                "ContextRetriever Level 1 精确匹配完成"
            );
        }

        // =========================================================
        // Level 2: 子串匹配（BM25 bigram 检索）
        // =========================================================
        if results.len() < self.config.top_k {
            let remaining = self.config.top_k - results.len();
            let query = build_query_from_keywords(&cluster.cluster_keywords);

            if !query.is_empty() {
                let substring_hits = self.retriever.search_substring(
                    &query,
                    persona_uid,
                    self.config.substring_supplement.min(remaining),
                );

                for hit in &substring_hits {
                    // 去重: 跳过 Level 1 已命中的文档
                    if results.iter().any(|r| r.summary == hit.doc_summary) {
                        continue;
                    }
                    results.push(ContextDocument {
                        summary: hit.doc_summary.clone(),
                        layer: hit.layer.clone(),
                        source_channel: "substring".to_string(),
                        score: hit.rrf_score,
                    });
                    if results.len() >= self.config.top_k {
                        break;
                    }
                }

                tracing::debug!(
                    persona_uid,
                    substring_count = substring_hits.len(),
                    query_len = query.len(),
                    "ContextRetriever Level 2 子串匹配完成"
                );
            }
        }

        // =========================================================
        // Level 3: 语义模糊匹配（需要 embedding，当前跳过）
        // =========================================================
        // 语义检索需要 embedding 模型支持。在 embedding 不可用时跳过此级。
        // 由上层在 embedding provider 就绪时通过 Retriever.search() 补充。
        if results.len() < self.config.top_k {
            tracing::debug!(
                persona_uid,
                current_count = results.len(),
                target_top_k = self.config.top_k,
                "ContextRetriever Level 3 语义匹配跳过（需 embedding 模型支持）"
            );
        }

        // 截断到 top_k
        if results.len() > self.config.top_k {
            results.truncate(self.config.top_k);
        }

        tracing::info!(
            persona_uid,
            total_results = results.len(),
            "ContextRetriever 三级检索完成"
        );

        results
    }
}

// =========================================================
// 辅助函数
// =========================================================

/// 将 KeywordToken 列表拼接为 BM25 查询字符串。
///
/// 格式: 空格分隔的关键词文本。
/// 说明: BM25 的 bigram 分词会将中文关键词再拆为双字 bigram，
/// 从而实现子串级别的模糊匹配。
fn build_query_from_keywords(keywords: &[KeywordToken]) -> String {
    keywords
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<&str>>()
        .join(" ")
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retriever::{L1DocView, L2DocView, Retriever};

    /// 构造含 3 条 L1 + 2 条 L2 的测试 Retriever。
    fn make_test_retriever() -> Retriever {
        let mut r = Retriever::new();

        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户讨论了Rust异步编程的技术细节".to_string(),
            keywords: Some("Rust,编程,异步".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 1000,
            salience: 0.8,
        });
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户和朋友去吃火锅，聊了工作压力".to_string(),
            keywords: Some("社交,火锅,工作压力".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 2000,
            salience: 0.6,
        });
        r.index_l1(&L1DocView {
            id: uuid::Uuid::new_v4(),
            summary: "用户周末去爬山，感觉身心放松".to_string(),
            keywords: Some("运动,爬山,放松".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 3000,
            salience: 0.5,
        });

        r.index_l2(&L2DocView {
            id: 1,
            title: "完成Rust项目".to_string(),
            summary: "用户完成了第一个Rust项目，发布了crate".to_string(),
            keywords: Some("Rust,项目,发布".to_string()),
            attitude: Some("感到很有成就感".to_string()),
            paraphrase: Some("对完成重要工作感到满意".to_string()),
            persona_uid: "user-0001".to_string(),
            share: 0.8,
            confidence: 0.9,
            created_at: 1500,
            salience: 0.9,
        });
        r.index_l2(&L2DocView {
            id: 2,
            title: "工作压力导致失眠".to_string(),
            summary: "用户因连续加班出现失眠症状".to_string(),
            keywords: Some("工作压力,加班,失眠".to_string()),
            attitude: Some("感到身心俱疲".to_string()),
            paraphrase: Some("面对长期工作压力时容易出现身心症状".to_string()),
            persona_uid: "user-0001".to_string(),
            share: 0.3,
            confidence: 0.85,
            created_at: 2500,
            salience: 0.85,
        });

        r
    }

    // ---- 基础功能 ----

    #[test]
    fn retrieve_context_exact_match() {
        let r = make_test_retriever();
        let cr = ContextRetriever::new(&r, ContextRetrieverConfig::default());

        // 构造一个与 "Rust" 相关的簇
        let cluster = TopicCluster::new(vec![super::super::batcher::L1Item {
            id: uuid::Uuid::new_v4(),
            summary: "Rust学习".into(),
            keywords: vec![
                KeywordToken::new("Rust").unwrap(),
                KeywordToken::new("编程").unwrap(),
            ],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 1000,
        }]);

        let docs = cr.retrieve_context(&cluster, "user-0001");
        // 应至少命中 Level 1 精确匹配: L1 "Rust,编程,异步" 和 L2 "Rust,项目,发布"
        assert!(!docs.is_empty(), "应至少命中一条精确匹配结果");
        assert!(
            docs.iter().any(|d| d.source_channel == "exact"),
            "应包含精确匹配通道结果"
        );
    }

    #[test]
    fn retrieve_context_empty_cluster() {
        let r = make_test_retriever();
        let cr = ContextRetriever::new(&r, ContextRetrieverConfig::default());

        let cluster = TopicCluster::new(vec![]);
        let docs = cr.retrieve_context(&cluster, "user-0001");
        // 空簇 → 无关键词 → 无检索结果
        assert!(docs.is_empty());
    }

    #[test]
    fn retrieve_context_filters_persona() {
        let r = make_test_retriever();
        let cr = ContextRetriever::new(&r, ContextRetrieverConfig::default());

        let cluster = TopicCluster::new(vec![super::super::batcher::L1Item {
            id: uuid::Uuid::new_v4(),
            summary: "Rust".into(),
            keywords: vec![KeywordToken::new("Rust").unwrap()],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 1000,
        }]);

        // user-0002 无任何文档
        let docs = cr.retrieve_context(&cluster, "user-0002");
        assert!(docs.is_empty());
    }

    #[test]
    fn retrieve_context_top_k_limit() {
        let r = make_test_retriever();
        let config = ContextRetrieverConfig {
            top_k: 1,
            exact_limit: 1,
            substring_supplement: 1,
        };
        let cr = ContextRetriever::new(&r, config);

        let cluster = TopicCluster::new(vec![super::super::batcher::L1Item {
            id: uuid::Uuid::new_v4(),
            summary: "Rust".into(),
            keywords: vec![
                KeywordToken::new("Rust").unwrap(),
                KeywordToken::new("编程").unwrap(),
            ],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 1000,
        }]);

        let docs = cr.retrieve_context(&cluster, "user-0001");
        assert!(docs.len() <= 1, "结果数不应超过 top_k=1");
    }

    // （原 retrieve_context_substring_fallback 输入"失眠"实际存在于 L2 文档 keywords，
    //  exact 路径必命中，substring 降级不可达，测试名不副实，已删除）

    // ---- 降级路径 ----

    // （原 retrieve_context_level2_triggers_when_level1_insufficient 的
    //  Level 2 补充行为无任何断言，仅验证不报错，已删除）

    #[test]
    fn retrieve_context_skips_semantic_channel_without_embedding() {
        let r = make_test_retriever();
        // 全部文档的 keywords 均不含 "xyznotfound"
        let config = ContextRetrieverConfig {
            top_k: 3,
            exact_limit: 3,
            substring_supplement: 3,
        };
        let cr = ContextRetriever::new(&r, config);

        let cluster = TopicCluster::new(vec![super::super::batcher::L1Item {
            id: uuid::Uuid::new_v4(),
            summary: "xyznotfound".into(),
            keywords: vec![KeywordToken::new("xyznotfound").unwrap()],
            embedding: None,
            evidence_notes: vec![],
            salience: 0.5,
            created_at: 1000,
        }]);

        let docs = cr.retrieve_context(&cluster, "user-0001");
        // 真实验证：embedding 不可用 → Level 3（semantic 通道）被跳过，
        // 任何结果都不应来自 semantic 通道
        assert!(
            docs.iter().all(|d| d.source_channel != "semantic"),
            "embedding 不可用时不应产出语义通道结果"
        );
        // 无精确/子串命中 → 返回空（检索路径不 panic）
        assert!(docs.is_empty(), "无任何匹配时应返回空");
    }

    // ---- build_query_from_keywords ----

    /// build_query_from_keywords 各关键词输入参数化验证。
    #[test]
    fn build_query_cases() {
        // 非空关键词 → 拼接查询
        let kw = vec![
            KeywordToken::new("Rust").unwrap(),
            KeywordToken::new("编程").unwrap(),
            KeywordToken::new("异步").unwrap(),
        ];
        let query = build_query_from_keywords(&kw);
        // KeywordToken::new() 会对英文做小写规范化，故此处用 "rust"
        assert!(query.contains("rust"));
        assert!(query.contains("编程"));
        assert!(query.contains("异步"));
        // 空关键词 → 空查询
        assert!(build_query_from_keywords(&[]).is_empty());
    }
}
