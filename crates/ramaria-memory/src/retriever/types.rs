//! crates/ramaria-memory/src/retriever/types.rs — 检索器对外公开的类型定义
//!
//! 设计特点:
//! - 承载检索配置、请求/响应、L1/L2/utt 文档检索视图等纯数据类型的声明
//! - 类型经根模块 `retriever.rs` 以 `pub use` 再导出，维持 lib.rs 对外 API 不变
//! - 仅声明字段与 derive，不含任何业务逻辑

use crate::bm25::{Bm25Config, DocId};
use crate::graph_retriever::GraphRetrieverConfig;
use crate::rrf::RrfConfig;
use crate::vector::VectorIndexConfig;

// =========================================================
// 检索配置
// =========================================================

/// 组合检索配置。
#[derive(Debug, Clone)]
pub struct RetrieverConfig {
    /// BM25 配置
    pub bm25: Bm25Config,
    /// 向量检索配置
    pub vector: VectorIndexConfig,
    /// 图谱检索配置
    pub graph: GraphRetrieverConfig,
    /// RRF 融合配置
    pub rrf: RrfConfig,
    /// 是否启用 BM25 通道
    pub enable_bm25: bool,
    /// 是否启用向量通道
    pub enable_vector: bool,
    /// 是否启用图谱通道
    pub enable_graph: bool,
}

impl Default for RetrieverConfig {
    fn default() -> Self {
        Self {
            bm25: Bm25Config::default(),
            vector: VectorIndexConfig::default(),
            graph: GraphRetrieverConfig::default(),
            rrf: RrfConfig::default(),
            enable_bm25: true,
            enable_vector: true,
            enable_graph: true,
        }
    }
}

// =========================================================
// 检索结果
// =========================================================

/// 统一检索结果——一条被检索到的文档。
///
/// 可在下游（rag.rs）按 persona_uid/share 进一步过滤。
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 文档标识符
    pub doc_id: DocId,
    /// 文档所在层级: "l1" 或 "l2"
    pub layer: String,
    /// RRF 融合分数
    pub rrf_score: f64,
    /// BM25 原始分数（若不出现在 BM25 结果中则为 None）
    pub bm25_score: Option<f64>,
    /// 向量通道原始相似度（若不出现在向量结果中则为 None）
    pub vector_score: Option<f64>,
    /// 图谱通道原始分数（若不出现在图谱结果中则为 None）
    pub graph_score: Option<f64>,
    /// 关联的 persona_uid（L1 的 persona_uid 或 L2 event 的 persona_uid）
    pub persona_uid: Option<String>,
    /// 分享意愿（L2 事件专用，L1 为 None）
    pub share: Option<f64>,
    /// 文档创建时间（Unix 毫秒），用于时间衰减加权
    pub created_at: i64,
    /// 最近访问时间（Unix 毫秒，L1 经 touch 接线刷新；L2 事件暂不追踪 → None）。
    ///
    /// 用途:
    /// - 参与时间衰减排序（`decay::calc_retention`）：近期被检索命中的 L1
    ///   获得访问加成（保底 `recent_boost_floor`），使"刚聊过的话题"更容易被召回。
    pub last_accessed_at: Option<i64>,
    /// 文档标题/摘要（供 RAG 格式化使用）
    pub doc_summary: String,
}

/// 检索请求参数。
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// 查询文本
    pub query: String,
    /// 目标 persona_uid（用于过滤检索结果）
    pub persona_uid: Option<String>,
    /// 最大返回结果数
    pub top_k: usize,
    /// 供下游 `rag.rs` 过滤低 share 事件使用（本结构体的检索方法不读取该字段）
    pub filter_share: bool,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            persona_uid: None,
            top_k: 5,
            filter_share: true,
        }
    }
}

// =========================================================
// 文档数据源（用于注入存储访问）
// =========================================================

/// L1 记忆的检索视图（BM25 索引+检索需要的字段）。
#[derive(Debug, Clone)]
pub struct L1DocView {
    pub id: uuid::Uuid,
    pub summary: String,
    pub keywords: Option<String>,
    pub persona_uid: Option<String>,
    pub created_at: i64,
    pub salience: f64,
    /// 最近访问时间（Unix 毫秒），检索命中经 touch 刷新后参与访问加成衰减。
    pub last_accessed_at: Option<i64>,
}

/// L2 事件的检索视图。
#[derive(Debug, Clone)]
pub struct L2DocView {
    pub id: i64,
    pub title: String,
    pub summary: String,
    pub keywords: Option<String>,
    pub attitude: Option<String>,
    pub paraphrase: Option<String>,
    pub persona_uid: String,
    pub share: f64,
    pub confidence: f64,
    pub created_at: i64,
    pub salience: f64,
}

/// utt 话语块的检索视图（v1.4 原文通道）。
///
/// 安全约束:
/// - 原文是最高敏感层：检索严格按 `persona_uid` 精确隔离，不做跨 persona 共享。
/// - `block_text` 不写日志。
#[derive(Debug, Clone)]
pub struct UttDocView {
    /// utt_blocks 表主键
    pub id: i64,
    /// 块归属人格（检索隔离键）
    pub persona_uid: String,
    /// 来源会话
    pub session_id: uuid::Uuid,
    /// 块内原文全文（含发言人标记）
    pub block_text: String,
    /// 块内消息条数
    pub msg_count: u32,
    /// 块创建时间（Unix 毫秒）
    pub created_at: i64,
}

/// utt 原文块检索命中。
#[derive(Debug, Clone)]
pub struct UttHit {
    /// 命中的块视图
    pub doc: UttDocView,
    /// 相似度得分：向量通道为调整后相似度，子串降级为命中 token 数
    pub score: f64,
    /// 命中通道: `"vector"`（向量）或 `"substring"`（BM25 子串降级）
    pub channel: &'static str,
}
