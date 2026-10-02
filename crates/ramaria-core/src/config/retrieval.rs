//! crates/ramaria-core/src/config/retrieval.rs - Ramaria 检索与衰减配置模块
//!
//! 设计特点:
//! - 定义混合检索配置（通道权重、top_k、叙事取舍等）
//! - 定义记忆衰减与半衰期配置
//! - 提供叙事加权与 top_k 的 serde 缺省回退
//! - 各配置组提供稳定默认值
//! - 支持 serde，不访问外部资源

use serde::{Deserialize, Serialize};

// =========================================================
// 检索配置
// =========================================================

/// 记忆检索参数。
///
/// 职责:
/// - 控制 L0/L1/L2 检索数量、RRF 融合参数和各通道权重。
/// - 为混合 RAG 提供可调默认值。
///
/// 说明:
/// - 具体检索算法在 `ramaria-storage` / `ramaria-memory` 中实现。
/// - 此结构只定义参数，不执行检索。
///
/// 三路检索独立参数说明:
/// - 本组为 **memory_rag 摘要路**（L1/L2 记忆摘要检索 + RRF 融合 + 脉络加权注入）
///   的检索参数；L0/L1/L2 各层 top_k、相似度阈值、融合权重均只服务摘要路。
/// - 本组同时承载摘要路 Persona-Aware RAG 的上下文格式化参数（`rag_*`），
///   运行时组装为 `ramaria-memory::rag::RagConfig`（默认与既有默认行为等价）。
/// - 原文样例路（utt）与知识路（fact）的检索参数分别在各自配置组独立定义，
///   相互不共享本组数值。
///
/// 兼容性说明:
/// - struct 级 `#[serde(default)]`：config.toml 中 `[retrieval]` 表只写部分键时
///   （含旧版本布局未含新增键），缺失字段回退 `Default` 实现，避免解析失败。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetrievalConfig {
    /// L0 滑动窗口大小
    pub l0_window_size: u32,
    /// L0 检索返回条数
    pub l0_retrieve_top_k: u32,
    /// L1 检索返回条数
    pub l1_retrieve_top_k: u32,
    /// L2 检索返回条数
    pub l2_retrieve_top_k: u32,
    /// 语义相似度过滤阈值（余弦距离，超过此值视为不相关）
    pub similarity_threshold: f64,
    /// RRF 融合平滑系数
    pub rrf_k: u32,
    /// BM25 通道权重
    pub bm25_weight: f64,
    /// 图谱通道权重
    pub graph_weight: f64,
    /// 向量通道开关（默认 true）。
    ///
    /// `false` 时关闭摘要路检索的向量通道（仅 BM25 + 图谱参与 RRF 融合），
    /// 对应内存检索器 `RetrieverConfig.enable_vector`。
    pub enable_vector: bool,
    /// 关键词镜像通道开关（默认 true）。
    ///
    /// `false` 时摘要路检索不含关键词镜像（KeywordService CompositeIndex）
    /// 这一第四通道，回退到仅 BM25 + 向量 + 图谱三通道的既有行为；
    /// 对应内存检索器 `RetrieverConfig.enable_keyword_channel`。
    pub enable_keyword_channel: bool,
    /// 关键词镜像通道权重（默认 1.0）。
    ///
    /// 关键词镜像作为第四检索通道参与 RRF 融合时的权重，
    /// 对应内存检索器 `RrfConfig.keyword_weight`（1.0 = 与向量通道同权重）。
    pub keyword_weight: f64,
    /// L2 结果排序权重（<1.0 表示 L2 优先展示）
    pub retrieval_weight_l2: f64,
    /// L1 结果排序权重
    pub retrieval_weight_l1: f64,
    /// 脉络加权注入开关（v1.7 B4）：跨会话近期摘要按"时间（衰减 × 访问加成）× 话题相关性"
    /// 融合排序注入；`false` 回退 v1.6 的"无条件取最近 N 条"。
    #[serde(default = "default_narrative_weighted")]
    pub narrative_weighted: bool,
    /// 脉络注入的最大条数（v1.7 B4），默认 3。
    #[serde(default = "default_narrative_top_k")]
    pub narrative_top_k: u32,
    /// 摘要路 RAG 上下文格式化：最大记忆条目数（默认 5）。
    pub rag_max_memories: u32,
    /// 摘要路 RAG 上下文格式化：单条记忆摘要最大字符数（默认 120）。
    pub rag_max_summary_chars: u32,
    /// 摘要路 Persona-Aware 过滤：user 类型最低 share 阈值（默认 0.3）。
    pub rag_share_threshold_user: f64,
    /// 摘要路 Persona-Aware 过滤：char/anim/oc/hist 类型最低 share 阈值（默认 0.5）。
    pub rag_share_threshold_char: f64,
    /// 摘要路 Persona-Aware 过滤：rama 类型最低 share 阈值（默认 0.0，即全量）。
    pub rag_share_threshold_rama: f64,
    /// 摘要路 RAG 上下文格式化：是否包含图谱实体（默认 true）。
    pub rag_include_graph_entities: bool,
}

/// serde 默认值：脉络加权注入默认启用（自动为主可配置）。
fn default_narrative_weighted() -> bool {
    true
}

/// serde 默认值：脉络注入条数默认 3。
fn default_narrative_top_k() -> u32 {
    3
}

impl Default for RetrievalConfig {
    /// 创建默认检索参数。
    ///
    /// 返回:
    /// - 适合轻度聊天场景的 L0/L1/L2 检索规模。
    /// - RRF k=60，BM25 权重 1.0，图谱权重 0.8；向量通道默认开启。
    /// - 摘要路 RAG 格式化参数与 memory `RagConfig::default()` 一致（行为等价）。
    fn default() -> Self {
        Self {
            l0_window_size: 3,
            l0_retrieve_top_k: 3,
            l1_retrieve_top_k: 4,
            l2_retrieve_top_k: 2,
            similarity_threshold: 0.6,
            rrf_k: 60,
            bm25_weight: 1.0,
            graph_weight: 0.8,
            enable_vector: true,
            enable_keyword_channel: true,
            keyword_weight: 1.0,
            retrieval_weight_l2: 0.8,
            retrieval_weight_l1: 1.0,
            narrative_weighted: true,
            narrative_top_k: 3,
            rag_max_memories: 5,
            rag_max_summary_chars: 120,
            rag_share_threshold_user: 0.3,
            rag_share_threshold_char: 0.5,
            rag_share_threshold_rama: 0.0,
            rag_include_graph_entities: true,
        }
    }
}

// =========================================================
// 记忆衰减配置（Ebbinghaus）
// =========================================================

/// Ebbinghaus 遗忘曲线衰减参数。
///
/// 职责:
/// - 描述不同记忆层的基础稳定性。
/// - 描述 salience 和近期访问对保留率的修正。
///
/// 衰减公式：R = e^(-t / S)
/// - R：保留率 0..1
/// - t：距生成的天数
/// - S：稳定性系数，越大衰减越慢
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DecayConfig {
    /// L0 稳定性系数（细节信息衰减最快）
    pub s_l0: u32,
    /// L1 稳定性系数
    pub s_l1: u32,
    /// L2 稳定性系数（聚合摘要衰减最慢）
    pub s_l2: u32,
    /// 是否启用访问加成
    pub enable_access_boost: bool,
    /// 近期访问加成天数
    pub recent_boost_days: u32,
    /// 近期访问保留率下限
    pub recent_boost_floor: f64,
    /// salience 对稳定性的加成系数
    /// S_adjusted = S × (1 + salience × multiplier)
    pub salience_multiplier: f64,
}

impl Default for DecayConfig {
    /// 创建默认衰减参数。
    ///
    /// 返回:
    /// - L0/L1/L2 稳定性分别为 10/30/60。
    /// - 启用最近访问加成和 salience 修正。
    fn default() -> Self {
        Self {
            s_l0: 10,
            s_l1: 30,
            s_l2: 60,
            enable_access_boost: true,
            recent_boost_days: 7,
            recent_boost_floor: 0.5,
            salience_multiplier: 0.5,
        }
    }
}
