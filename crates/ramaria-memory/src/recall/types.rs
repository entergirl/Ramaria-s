//! crates/ramaria-memory/src/recall/types.rs - 召回视图抽象与输入输出类型
//!
//! 设计特点:
//! - 只读视图 trait：宿主侧句柄以回调方式注入，锁只覆盖同步内存读取
//! - 纯数据类型：召回闸门 / 子层选择 / 通道计数 / 结构化条目 / 装配输出
//! - 不依赖任何入口层概念，供在线管线与服务层共用

use std::collections::BTreeMap;
use std::sync::RwLock;

use ramaria_core::config::{DecayConfig as CoreDecayConfig, RetrievalConfig, UttConfig};
use ramaria_core::lock::read_recover;
use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_core::types::PersonaKind;

use crate::decay::DecayConfig;
use crate::keyword::KeywordService;
use crate::retriever::{Retriever, SearchResult};

// =========================================================
// 只读视图抽象（宿主侧句柄注入）
// =========================================================

/// 检索器只读视图。
///
/// 职责:
/// - 把「如何持有检索器」与「如何召回」解耦：在线管线持有 `RwLock<Retriever>`，
///   服务层持有懒加载槽 `RwLock<Option<Retriever>>`，两者共用同一召回实现。
///
/// 实现要求:
/// - 实现方必须同步返回（仅内存读取），不得在回调内 await，避免 std 锁跨 await。
/// - 检索器未加载时传 `None`（召回按空结果处理，不视为故障）。
pub trait RetrieverSource {
    /// 在检索器读锁内执行同步回调。
    ///
    /// 参数:
    /// - `f`: 回调；参数为 `Some(&Retriever)`（已加载）或 `None`（未加载）。
    fn with_retriever<R>(&self, f: impl FnOnce(Option<&Retriever>) -> R) -> R;

    /// 在检索器读锁内执行脉络加权检索（未加载 → 空结果）。
    ///
    /// 用途:
    /// - 跨 session 脉络注入的加权素材来源：以当前用户消息为话题依据，按
    ///   "时间（衰减 × 访问加成）× 话题相关性"融合排序；调用方按空结果
    ///   回退"最近 N 条"路径。
    ///
    /// 参数:
    /// - `query`: 当前用户消息（话题相关性依据）。
    /// - `persona_uid`: 目标人格 uid（仅返回该 persona 的 L1 摘要）。
    /// - `top_k`: 最多返回条数。
    /// - `now_ms`: 当前时间（Unix 毫秒，衰减基准）。
    /// - `decay_config`: 记忆层衰减配置（含访问加成参数）。
    ///
    /// 返回:
    /// - 按融合分降序的 L1 `SearchResult` 列表（最多 `top_k` 条）。
    ///
    /// 默认实现:
    /// - 返回空列表（未覆盖实现的降级，调用方按空结果处理）。
    fn search_narrative(
        &self,
        _query: &str,
        _persona_uid: &str,
        _top_k: usize,
        _now_ms: i64,
        _decay_config: &DecayConfig,
    ) -> Vec<SearchResult> {
        Vec::new()
    }
}

impl RetrieverSource for RwLock<Retriever> {
    fn with_retriever<R>(&self, f: impl FnOnce(Option<&Retriever>) -> R) -> R {
        let guard = read_recover(self, "recall.retriever");
        f(Some(&guard))
    }

    fn search_narrative(
        &self,
        query: &str,
        persona_uid: &str,
        top_k: usize,
        now_ms: i64,
        decay_config: &DecayConfig,
    ) -> Vec<SearchResult> {
        let guard = read_recover(self, "recall.retriever");
        guard.search_narrative(query, persona_uid, top_k, now_ms, decay_config)
    }
}

impl RetrieverSource for RwLock<Option<Retriever>> {
    fn with_retriever<R>(&self, f: impl FnOnce(Option<&Retriever>) -> R) -> R {
        let guard = read_recover(self, "recall.retriever_slot");
        f(guard.as_ref())
    }

    fn search_narrative(
        &self,
        query: &str,
        persona_uid: &str,
        top_k: usize,
        now_ms: i64,
        decay_config: &DecayConfig,
    ) -> Vec<SearchResult> {
        let guard = read_recover(self, "recall.retriever_slot");
        guard.as_ref().map_or_else(Vec::new, |r| {
            r.search_narrative(query, persona_uid, top_k, now_ms, decay_config)
        })
    }
}

/// 关键词镜像只读视图。
///
/// 职责:
/// - 提供锁内快照提取（倒排镜像 Arc + 词典池快照），快照在锁外用于异步查询，
///   避免 std 读锁跨 await。
pub trait KeywordMirrorSource {
    /// 在关键词镜像读锁内执行同步回调。
    ///
    /// 参数:
    /// - `f`: 回调；参数为镜像引用（未注入时为 `None`）。
    fn with_keyword_mirror<R>(&self, f: impl FnOnce(Option<&KeywordService>) -> R) -> R;
}

impl KeywordMirrorSource for RwLock<KeywordService> {
    fn with_keyword_mirror<R>(&self, f: impl FnOnce(Option<&KeywordService>) -> R) -> R {
        let guard = read_recover(self, "recall.keyword_mirror");
        f(Some(&guard))
    }
}

// =========================================================
// 输入 / 输出
// =========================================================

/// 召回闸门（宿主配置映射后的两个开关）。
///
/// 字段约定:
/// - `memory_rag`: 摘要路记忆检索（对应 `[injection].memory_rag`）；
/// - `utt`: 原文块通道（对应 `[injection].utt && [utt].enabled`，由调用方合成）。
#[derive(Debug, Clone, Copy)]
pub struct RecallGates {
    pub memory_rag: bool,
    pub utt: bool,
}

/// 摘要路记忆子层选择（L1 / L2 参与开关）。
///
/// 职责:
/// - 让调用方按需只召回 L1（会话摘要）或只召回 L2（事件）——服务层 `include` 的
///   分层开关据此落地（只请求某层时，另一层既不进段落也不进结构化条目）。
/// - 图谱实体由事件标题构成、来源为 L2 事件，跟随 `l2` 开关；两层全开时不受影响。
/// - 在线管线两层全开（[`RecallMemoryLayers::default`]），过滤为无操作，行为不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallMemoryLayers {
    pub l1: bool,
    pub l2: bool,
}

impl Default for RecallMemoryLayers {
    /// 默认两层全开（在线管线口径）。
    fn default() -> Self {
        Self { l1: true, l2: true }
    }
}

impl RecallMemoryLayers {
    /// 两层全开。
    pub fn both() -> Self {
        Self::default()
    }

    /// 仅 L1（会话摘要）。
    pub fn l1_only() -> Self {
        Self {
            l1: true,
            l2: false,
        }
    }

    /// 仅 L2（事件）。
    pub fn l2_only() -> Self {
        Self {
            l1: false,
            l2: true,
        }
    }

    /// 是否有任一层参与（供闸门判定使用）。
    pub fn any(&self) -> bool {
        self.l1 || self.l2
    }

    /// 判定某文档分层是否参与。
    ///
    /// 说明:
    /// - `l1` / `l2` 按开关判定；
    /// - `graph` 图谱实体由事件标题构成、来源为 L2 事件，故跟随 `l2` 开关
    ///   （仅请求 L1 时不展示事件侧内容；两层全开时不受影响）；
    /// - 其它未知分层恒参与（兜底）。
    pub fn allows(&self, layer: &str) -> bool {
        match layer {
            "l1" => self.l1,
            "l2" | "graph" => self.l2,
            _ => true,
        }
    }
}

/// 召回装配输入（纯数据 + 只读句柄）。
///
/// 泛型约定:
/// - `R` / `K` 为具体句柄类型（如 `RwLock<Retriever>`、`RwLock<Option<Retriever>>`、
///   `RwLock<KeywordService>`）；泛型而非 trait object，避免回调式视图方法的 dyn 兼容限制。
pub struct RecallInput<'a, R: RetrieverSource + ?Sized, K: KeywordMirrorSource + ?Sized> {
    /// 检索器只读视图（在线管线 / 服务层各自注入）。
    pub retriever: &'a R,
    /// 关键词镜像只读视图。
    pub keyword_mirror: &'a K,
    /// 存储后端（仅用于检索命中的 L1 访问时间刷新）。
    pub storage: &'a dyn StorageBackend,
    /// 嵌入 provider（None = 向量通道降级）。
    pub embedding: Option<&'a dyn EmbeddingProvider>,
    /// 查询文本（在线管线为当前用户输入；服务层为 query 或最后一条用户消息）。
    pub query: &'a str,
    /// 目标人格 uid（供检索过滤与 utt 原文隔离；类型判定见 `persona_kind`）。
    pub persona_uid: Option<&'a str>,
    /// 目标人格类型（显式载体：调用方按 persona 行 `kind` 解析后传入）。
    ///
    /// 约定:
    /// - `Some(kind)`: 已解析的人格类型——Persona-Aware 阈值与 utt 原文白名单均以此为准；
    /// - `None`: 类型未知（persona 行缺失 / 读取失败）——保守降级：RAG 按最严阈值、
    ///   utt 白名单不放行；
    /// - 无画像的 rama 自身召回由调用方显式传 `Some(PersonaKind::Rama)`。
    pub persona_kind: Option<PersonaKind>,
    /// 摘要路检索配置（`[retrieval]` 组）。
    pub retrieval: &'a RetrievalConfig,
    /// 时间衰减配置（`[decay]` 组）。
    pub decay: &'a CoreDecayConfig,
    /// utt 原文通道配置（`[utt]` 组）。
    pub utt: &'a UttConfig,
    /// 召回闸门。
    pub gates: RecallGates,
    /// 摘要路记忆子层选择（L1 / L2；在线管线用默认值＝两层全开）。
    pub memory_layers: RecallMemoryLayers,
    /// 当前时间（Unix 毫秒，衰减与访问加成基准）。
    pub now_ms: i64,
}

/// 单个检索通道的命中计数（诊断用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecallChannels {
    pub vector: usize,
    pub bm25: usize,
    pub keyword: usize,
    pub graph: usize,
}

impl RecallChannels {
    /// 转为通道名 → 命中数的有序映射（供 `stats.channels` 直接使用）。
    pub fn as_map(&self) -> BTreeMap<String, usize> {
        BTreeMap::from([
            ("vector".to_string(), self.vector),
            ("bm25".to_string(), self.bm25),
            ("keyword".to_string(), self.keyword),
            ("graph".to_string(), self.graph),
        ])
    }
}

/// 结构化召回条目（供调用方组装 items / 概览）。
///
/// 字段约定:
/// - `layer`: 条目分层（`l1` / `l2` / `graph`）；
/// - `id`: 文档标识字符串（`L1:{uuid}` / `L2:{id}` / `graph:{实体}`）；
/// - `score`: 衰减重排后的融合分；
/// - `created_at`: 文档创建时间（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecallHit {
    pub layer: String,
    pub id: String,
    pub text: String,
    pub score: f64,
    pub created_at: i64,
}

/// 召回装配输出。
///
/// 字段约定:
/// - `memory_context`: 摘要路上下文文本（`None` = 无命中 / 闸门关闭 / 索引未加载）；
/// - `doc_labels`: 实际进入上下文文本的文档 label 集合（`L1:{uuid}` / `L2:{id}`）；
/// - `utt_context`: 原文片段文本（`None` = 未命中 / 白名单外 / 闸门关闭）；
/// - `hits`: Persona-Aware 过滤后的命中（按衰减后分数降序，已受 `rag.max_memories` 截断）；
/// - `channels`: 各检索通道命中计数；
/// - `fused_count` / `filtered_count`: 融合后条数 / 过滤后条数（诊断用）。
#[derive(Debug, Clone, Default)]
pub struct RecallOutput {
    pub memory_context: Option<String>,
    pub doc_labels: Vec<String>,
    pub utt_context: Option<String>,
    pub hits: Vec<RecallHit>,
    pub channels: RecallChannels,
    pub fused_count: usize,
    pub filtered_count: usize,
}
