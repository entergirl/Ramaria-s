//! crates/ramaria-memory/src/rebuild.rs - Ramaria 索引重建编排器
//!
//! 设计特点:
//! - 编排 BM25 + 向量 + 图谱三通道索引的全量重建
//! - 通过 Retriever 持有各通道索引，调用方注入预加载的文档数据
//! - 支持增量重建（仅重建指定通道）和全量重建
//! - 不直接访问数据库——所有数据由调用方通过参数注入
//! - 记录文档数量、耗时等观测指标
//!
//! 全量重建管线（README 核心特性）；v1.6 核查 desktop index 命令接线
//!
//! 重建流程:
//! 1. 清空 Retriever 全部索引（clear）
//! 2. 逐条加载 L1/L2 文档到 Retriever（触发 BM25 + 向量索引）
//! 3. 加载图谱节点和边到 GraphRetriever
//! 4. 返回重建统计（文档数、节点数、边数、耗时）

use crate::graph_retriever::GraphRetriever;
use crate::retriever::{L1DocView, L2DocView, Retriever};
use tracing::{debug, info, warn};

// =========================================================
// 索引重建配置
// =========================================================

/// 索引重建配置。
///
/// 字段约定:
/// - `rebuild_bm25`: 是否重建 BM25 索引，默认 true。
/// - `rebuild_graph`: 是否重建图谱索引，默认 true。
/// - `batch_log_interval`: 每处理多少条文档记录一次进度日志，默认 100。
#[derive(Debug, Clone)]
pub struct RebuildConfig {
    /// 是否重建 BM25 索引
    pub rebuild_bm25: bool,
    /// 是否重建图谱索引
    pub rebuild_graph: bool,
    /// 进度日志间隔（条数）
    pub batch_log_interval: usize,
}

impl Default for RebuildConfig {
    fn default() -> Self {
        Self {
            rebuild_bm25: true,
            rebuild_graph: true,
            batch_log_interval: 100,
        }
    }
}

// =========================================================
// 重建统计
// =========================================================

/// 索引重建完成后的统计信息。
#[derive(Debug, Clone)]
pub struct RebuildStats {
    /// 重建的 L1 文档数
    pub l1_count: usize,
    /// 重建的 L2 事件数
    pub l2_count: usize,
    /// 加载的图谱节点数
    pub graph_nodes: usize,
    /// 加载的图谱边数
    pub graph_edges: usize,
    /// 重建耗时（毫秒）
    pub elapsed_ms: u64,
}

impl std::fmt::Display for RebuildStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "索引重建完成: L1={}, L2={}, 图谱节点={}, 边={}, 耗时={}ms",
            self.l1_count, self.l2_count, self.graph_nodes, self.graph_edges, self.elapsed_ms
        )
    }
}

// =========================================================
// 索引重建器
// =========================================================

/// 索引重建编排器。
///
/// 职责:
/// - 管理 Retriever 的索引生命周期（清空→重载→统计）
/// - 支持仅重建指定通道（BM25 / 图谱）
///
/// 用法:
/// ```
/// use ramaria_memory::{IndexRebuilder, RebuildConfig, Retriever};
/// let mut rebuilder = IndexRebuilder::new(RebuildConfig::default());
/// let mut retriever = Retriever::new();
/// let stats = rebuilder.rebuild_all(&mut retriever, &[], &[], &[], &[]);
/// assert_eq!(stats.l1_count, 0);
/// ```
pub struct IndexRebuilder {
    config: RebuildConfig,
}

impl IndexRebuilder {
    /// 使用给定配置创建索引重建器。
    pub fn new(config: RebuildConfig) -> Self {
        Self { config }
    }

    /// 使用默认配置创建索引重建器。
    pub fn with_defaults() -> Self {
        Self {
            config: RebuildConfig::default(),
        }
    }

    /// 获取当前配置的引用。
    pub fn config(&self) -> &RebuildConfig {
        &self.config
    }

    /// 全量重建所有启用的索引通道。
    ///
    /// 参数:
    /// - `retriever`: 待重建的检索器（将被清空后重新填充）。
    /// - `l1_docs`: 所有 L1 摘要文档视图。
    /// - `l2_docs`: 所有 L2 事件文档视图。
    /// - `graph_nodes`: 所有图谱节点 `(id, entity_name, entity_type)`。
    /// - `graph_edges`: 所有图谱边 `(id, source_id, target_id, relation_type)`。
    ///
    /// 返回:
    /// - `RebuildStats`: 重建统计信息。
    pub fn rebuild_all(
        &mut self,
        retriever: &mut Retriever,
        l1_docs: &[L1DocView],
        l2_docs: &[L2DocView],
        graph_nodes: &[(i64, String, String)],
        graph_edges: &[(i64, i64, i64, String)],
    ) -> RebuildStats {
        let start = std::time::Instant::now();

        info!(
            l1_count = l1_docs.len(),
            l2_count = l2_docs.len(),
            graph_nodes = graph_nodes.len(),
            graph_edges = graph_edges.len(),
            "开始全量索引重建"
        );

        // 清空全部索引
        retriever.clear();
        debug!("已清空全部索引");

        // 加载文档（BM25 是否构建由 RetrieverConfig.enable_bm25 控制）
        // index_l1/index_l2 接受引用，无需 clone；内部仅在存入 HashMap 时复制一次
        // rebuild_bm25=false 时临时禁用 BM25 通道（仅加载文档映射），加载结束后恢复
        let restore_bm25 = if !self.config.rebuild_bm25 {
            warn!("BM25 重建已关闭，仅加载文档映射");
            Some(retriever.set_bm25_enabled(false))
        } else {
            None
        };

        let mut l1_count = 0usize;
        let mut l2_count = 0usize;

        for (i, doc) in l1_docs.iter().enumerate() {
            retriever.index_l1(doc);
            l1_count += 1;
            if self.config.rebuild_bm25 && (i + 1) % self.config.batch_log_interval == 0 {
                debug!(progress = i + 1, total = l1_docs.len(), "L1 索引构建中...");
            }
        }

        for (i, doc) in l2_docs.iter().enumerate() {
            retriever.index_l2(doc);
            l2_count += 1;
            if self.config.rebuild_bm25 && (i + 1) % self.config.batch_log_interval == 0 {
                debug!(progress = i + 1, total = l2_docs.len(), "L2 索引构建中...");
            }
        }

        // 恢复 BM25 通道配置（仅当 rebuild_bm25=false 时非 None）
        if let Some(prev) = restore_bm25 {
            retriever.set_bm25_enabled(prev);
        }

        // 重建图谱索引
        let (gn, ge) = if self.config.rebuild_graph {
            self.rebuild_graph(retriever.graph_mut(), graph_nodes, graph_edges)
        } else {
            warn!("图谱索引重建已关闭");
            (0, 0)
        };

        let elapsed_ms = start.elapsed().as_millis() as u64;

        let stats = RebuildStats {
            l1_count,
            l2_count,
            graph_nodes: gn,
            graph_edges: ge,
            elapsed_ms,
        };

        info!("{}", stats);
        stats
    }

    /// 仅重建图谱索引（不清空 BM25）。
    ///
    /// 参数:
    /// - `graph`: 图谱检索器。
    /// - `nodes`: 节点列表 `(id, entity_name, entity_type)`。
    /// - `edges`: 边列表 `(id, source_id, target_id, relation_type)`。
    ///
    /// 返回:
    /// - `(节点数, 边数)`
    pub fn rebuild_graph(
        &self,
        graph: &mut GraphRetriever,
        nodes: &[(i64, String, String)],
        edges: &[(i64, i64, i64, String)],
    ) -> (usize, usize) {
        info!(nodes = nodes.len(), edges = edges.len(), "开始重建图谱索引");
        graph.load(nodes, edges);
        let nc = graph.node_count();
        let ec = graph.edge_count();
        info!(nodes = nc, edges = ec, "图谱索引重建完成");
        (nc, ec)
    }
}

impl Default for IndexRebuilder {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// =========================================================
// 辅助工具
// =========================================================

/// 从 MemoryL1 列表构建 L1DocView 列表。
///
/// 用于将存储层查询结果转换为检索器可用的文档视图。
///
/// 参数:
/// - `l1_list`: MemoryL1 列表。
///
/// 返回:
/// - L1DocView 列表。
pub fn l1_list_to_views(l1_list: &[ramaria_core::MemoryL1]) -> Vec<L1DocView> {
    l1_list
        .iter()
        .map(|l1| L1DocView {
            id: l1.id,
            summary: l1.summary.clone(),
            keywords: l1.keywords.clone(),
            persona_uid: l1.persona_uid.clone(),
            created_at: l1.created_at,
            salience: l1.salience,
        })
        .collect()
}

/// 从 MemoryEvent 列表构建 L2DocView 列表。
///
/// 用于将存储层查询结果转换为检索器可用的文档视图。
///
/// 参数:
/// - `events`: MemoryEvent 列表。
///
/// 返回:
/// - L2DocView 列表。
pub fn events_to_views(events: &[ramaria_core::MemoryEvent]) -> Vec<L2DocView> {
    events
        .iter()
        .map(|ev| L2DocView {
            id: ev.id,
            title: ev.title.clone(),
            summary: ev.summary.clone(),
            keywords: ev.keywords.clone(),
            attitude: ev.attitude.clone(),
            paraphrase: ev.paraphrase.clone(),
            persona_uid: ev.persona_uid.clone(),
            share: ev.share,
            confidence: ev.confidence,
            created_at: ev.created_at,
            salience: ev.salience,
        })
        .collect()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retriever::Retriever;
    use uuid::Uuid;

    fn make_l1(id: Uuid, summary: &str) -> L1DocView {
        L1DocView {
            id,
            summary: summary.to_string(),
            keywords: Some("测试,索引".to_string()),
            persona_uid: Some("user-0001".to_string()),
            created_at: 1000,
            salience: 0.5,
        }
    }

    fn make_l2(id: i64, title: &str, summary: &str) -> L2DocView {
        L2DocView {
            id,
            title: title.to_string(),
            summary: summary.to_string(),
            keywords: Some("事件,重建".to_string()),
            attitude: None,
            paraphrase: None,
            persona_uid: "user-0001".to_string(),
            share: 0.5,
            confidence: 0.8,
            created_at: 1000,
            salience: 0.6,
        }
    }

    #[test]
    fn test_rebuild_empty() {
        let mut retriever = Retriever::new();
        let mut rebuilder = IndexRebuilder::with_defaults();
        let stats = rebuilder.rebuild_all(&mut retriever, &[], &[], &[], &[]);
        assert_eq!(stats.l1_count, 0);
        assert_eq!(stats.l2_count, 0);
        assert_eq!(stats.graph_nodes, 0);
        assert_eq!(stats.graph_edges, 0);
        assert_eq!(retriever.doc_count(), 0);
    }

    #[test]
    fn test_rebuild_with_docs() {
        let mut retriever = Retriever::new();
        let l1_docs = vec![
            make_l1(Uuid::new_v4(), "测试摘要1"),
            make_l1(Uuid::new_v4(), "测试摘要2"),
        ];
        let l2_docs = vec![make_l2(1, "事件标题", "事件摘要")];

        let mut rebuilder = IndexRebuilder::with_defaults();
        let stats = rebuilder.rebuild_all(&mut retriever, &l1_docs, &l2_docs, &[], &[]);

        assert_eq!(stats.l1_count, 2);
        assert_eq!(stats.l2_count, 1);
        assert_eq!(retriever.doc_count(), 3);
    }

    #[test]
    fn test_rebuild_graph_only() {
        let mut retriever = Retriever::new();
        let nodes = vec![
            (1i64, "实体A".to_string(), "concept".to_string()),
            (2i64, "实体B".to_string(), "person".to_string()),
        ];
        let edges = vec![(1i64, 1i64, 2i64, "USES_DEPENDS".to_string())];

        let mut rebuilder = IndexRebuilder::with_defaults();
        let stats = rebuilder.rebuild_all(&mut retriever, &[], &[], &nodes, &edges);

        assert_eq!(stats.graph_nodes, 2);
        assert_eq!(stats.graph_edges, 1);
        assert_eq!(retriever.graph_mut().node_count(), 2);
        assert_eq!(retriever.graph_mut().edge_count(), 1);
    }

    #[test]
    fn test_rebuild_stats_display() {
        let stats = RebuildStats {
            l1_count: 10,
            l2_count: 5,
            graph_nodes: 3,
            graph_edges: 2,
            elapsed_ms: 42,
        };
        let display = format!("{}", stats);
        assert!(display.contains("L1=10"));
        assert!(display.contains("L2=5"));
        assert!(display.contains("节点=3"));
        assert!(display.contains("边=2"));
        assert!(display.contains("42ms"));
    }

    #[test]
    fn test_l1_list_to_views() {
        use ramaria_core::MemoryL1;
        let mut l1 = MemoryL1::new(
            Uuid::new_v4(),
            "摘要内容".to_string(),
            Some("下午".to_string()),
        );
        l1.keywords = Some("标签1,标签2".to_string());
        l1.persona_uid = Some("user-0001".to_string());
        l1.context_json = Some(r#"{"chat_partners":["user-0001"]}"#.to_string());

        let views = l1_list_to_views(&[l1]);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].summary, "摘要内容");
        assert_eq!(views[0].persona_uid, Some("user-0001".to_string()));
    }

    #[test]
    fn test_events_to_views() {
        use ramaria_core::MemoryEvent;
        let now = ramaria_core::now_ms();
        let ev = MemoryEvent::new(
            "user-0001".to_string(),
            "事件标题".to_string(),
            "事件摘要".to_string(),
            now,
            now,
        );
        let views = events_to_views(&[ev]);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].title, "事件标题");
        assert_eq!(views[0].persona_uid, "user-0001");
    }

    #[test]
    fn test_rebuild_config_default() {
        let cfg = RebuildConfig::default();
        assert!(cfg.rebuild_bm25);
        assert!(cfg.rebuild_graph);
        assert_eq!(cfg.batch_log_interval, 100);
    }
}
