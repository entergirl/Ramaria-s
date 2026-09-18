//! crates/ramaria-memory/src/recall.rs - 召回装配共用实现（在线管线与服务层同源）
//!
//! 设计特点:
//! - 单份实现：把「检索 → Persona-Aware 过滤 → 衰减重排 → 预算裁剪 → 段落渲染 → utt 渲染」
//!   抽为共用函数，在线管线（Stage 5）与服务层 recall 用例调用同一份，保证两个入口召回同源
//! - 与宿主解耦：检索器与关键词镜像经只读视图 trait（`RetrieverSource` /
//!   `KeywordMirrorSource`）注入，本模块不依赖 app / cli / desktop / 协议概念
//! - 锁纪律：std 读锁只覆盖同步内存检索；embedding 生成与 touch_l1 在锁外 await
//! - 静默降级：嵌入不可用 → 向量通道缺席；镜像为空 → 关键词通道缺席；
//!   索引未加载 → 空召回（不视为故障，也不阻塞调用方）
//! - 隐私：不记录查询文本与记忆原文，日志只记计数、维度与字符数

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use ramaria_core::config::{DecayConfig as CoreDecayConfig, RetrievalConfig, UttConfig};
use ramaria_core::lock::read_recover;
use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_core::types::{PersonaKind, now_ms};
use uuid::Uuid;

use crate::bm25::DocId;
use crate::decay::{DecayConfig, calc_retention};
use crate::keyword::service::{KeywordPoolSnapshot, query_text_labels};
use crate::keyword::{CompositeIndex, KeywordService};
use crate::rag::{RagConfig, filter_by_persona, format_context_text};
use crate::retriever::{Retriever, SearchRequest, SearchResult, UttHit};

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
}

impl RetrieverSource for RwLock<Retriever> {
    fn with_retriever<R>(&self, f: impl FnOnce(Option<&Retriever>) -> R) -> R {
        let guard = read_recover(self, "recall.retriever");
        f(Some(&guard))
    }
}

impl RetrieverSource for RwLock<Option<Retriever>> {
    fn with_retriever<R>(&self, f: impl FnOnce(Option<&Retriever>) -> R) -> R {
        let guard = read_recover(self, "recall.retriever_slot");
        f(guard.as_ref())
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
    /// - 其它分层（如 `graph` 图谱实体）不属于摘要路子层，恒参与（不受该开关约束）。
    pub fn allows(&self, layer: &str) -> bool {
        match layer {
            "l1" => self.l1,
            "l2" => self.l2,
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
    /// 目标人格 uid（None = 不过滤；Persona-Aware 类型阈值仍生效）。
    pub persona_uid: Option<&'a str>,
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

// =========================================================
// 共用召回装配
// =========================================================

/// 执行一次完整召回装配。
///
/// 流程:
/// 1. 闸门判定：两个通道全关 → 直接返回空输出（不生成向量、不检索）；
/// 2. 查询向量：RAG 或（白名单内）utt 需要时生成，失败降级为 None；
/// 3. 关键词镜像预取：锁内取快照 → 锁外异步查询 → 纯 `(label, score)` 交检索融合；
/// 4. 多通道检索（读锁内同步）：BM25 + 向量 + 图谱 + 关键词镜像，RRF 融合；
/// 5. 记忆子层过滤：按 `memory_layers` 剔除未请求的 L1 / L2 文档（两层全开 = 无操作）；
/// 6. 时间衰减（含访问加成）→ touch_l1 刷新访问时间 → 按衰减后分数重排；
/// 7. Persona-Aware 过滤 → 段落渲染（`[相关记忆]`）→ 覆盖集合记录；
/// 8. utt 原文检索与预算渲染（白名单内且闸门开启）。
///
/// 参数:
/// - `input`: 召回输入（依赖句柄 + 生效配置 + 闸门 + 时间基准）。
///
/// 返回:
/// - 召回输出；任何降级路径都返回结构完整的输出而非错误（调用方按空值处理）。
///
/// 说明:
/// - 本函数是在线管线与服务层 recall 的唯一召回实现，行为等价由两入口共用同一份代码保证。
pub async fn assemble_recall<R: RetrieverSource + ?Sized, K: KeywordMirrorSource + ?Sized>(
    input: RecallInput<'_, R, K>,
) -> RecallOutput {
    let mut output = RecallOutput::default();

    // ---- 1. 闸门判定：两通道全关时直接返回（不生成向量、不检索） ----
    if !input.gates.memory_rag && !input.gates.utt {
        tracing::debug!("召回闸门全关，跳过检索装配");
        return output;
    }

    // utt 是否对本 persona 生效（原文白名单双闸门：开关 + persona 类型）。
    // 查询向量仅在 RAG 或 utt（白名单内）需要时生成，避免无效 embedding 调用。
    let utt_persona_allowed = input.gates.utt
        && input
            .persona_uid
            .map(|puid| {
                input
                    .utt
                    .persona_kind_whitelist
                    .contains(&PersonaKind::from_uid(puid))
            })
            .unwrap_or(false);
    let need_query_vec = input.gates.memory_rag || utt_persona_allowed;

    // ---- 2. 查询向量（锁外 await；失败降级为 None） ----
    let query_vec: Option<Vec<f32>> = if !need_query_vec {
        tracing::debug!("无需查询向量，仅执行无需向量的通道");
        None
    } else {
        match input.embedding {
            Some(provider) if provider.is_available() => match provider.embed(input.query).await {
                Ok(vec) => {
                    tracing::debug!(dim = vec.len(), "查询向量已生成");
                    Some(vec)
                }
                Err(e) => {
                    tracing::warn!(%e, "查询向量生成失败，向量通道降级");
                    None
                }
            },
            Some(_) => {
                tracing::debug!("嵌入模型不可用，跳过向量通道");
                None
            }
            None => {
                tracing::debug!("嵌入模型未配置，跳过向量通道");
                None
            }
        }
    };

    // ---- 3. 关键词镜像通道预取（混合检索第四通道） ----
    // 镜像查询异步（语义层需 embedding），必须在检索器读锁之前完成；
    // 预取后仅把 (label, score) 纯数据交给同步 search（无句柄泄漏）。
    let mut keyword_hits: Option<Vec<(String, f64)>> = None;
    if input.gates.memory_rag && input.retrieval.enable_keyword_channel {
        let kw_top_k = input.retrieval.l1_retrieve_top_k as usize;
        let snapshot: Option<(Arc<CompositeIndex>, KeywordPoolSnapshot)> =
            input.keyword_mirror.with_keyword_mirror(|mirror| {
                mirror
                    .filter(|svc| svc.doc_count() > 0)
                    .map(|svc| (svc.composite_arc(), svc.pool_snapshot()))
            });
        match snapshot {
            Some((composite, pool)) => {
                let hits = query_text_labels(
                    &composite,
                    &pool,
                    input.query,
                    input.persona_uid,
                    input.embedding,
                    kw_top_k,
                )
                .await;
                if hits.is_empty() {
                    tracing::debug!("关键词镜像无命中，跳过关键词通道");
                } else {
                    tracing::debug!(hits = hits.len(), "关键词镜像通道命中");
                    keyword_hits = Some(hits);
                }
            }
            None => {
                tracing::debug!("关键词镜像无文档，跳过关键词通道");
            }
        }
    }

    // ---- 4. 多通道检索 + utt 检索（读锁内同步，无 await） ----
    let query_owned = input.query.to_string();
    let persona_owned = input.persona_uid.map(str::to_string);
    let search_top_k = input.retrieval.l1_retrieve_top_k as usize;
    let utt_top_k = input.utt.retrieve_top_k as usize;
    let rag_active = input.gates.memory_rag;
    let keyword_for_search = keyword_hits.clone();
    let query_vec_snapshot = query_vec.clone();

    let (mut results, utt_hits): (Vec<SearchResult>, Vec<UttHit>) =
        input.retriever.with_retriever(|retriever| {
            let Some(retriever) = retriever else {
                tracing::debug!("检索索引未加载，本次召回为空（不视为故障）");
                return (Vec::new(), Vec::new());
            };

            // 4.1 摘要路多通道检索（RAG 闸门关闭时不执行）
            let results = if rag_active {
                let request = SearchRequest {
                    query: query_owned.clone(),
                    persona_uid: persona_owned.clone(),
                    top_k: search_top_k,
                    filter_share: true,
                };
                match (query_vec_snapshot.as_deref(), keyword_for_search) {
                    (Some(qv), Some(hits)) => {
                        retriever.search_with_keyword_hits(&request, Some(qv), Some(hits))
                    }
                    (Some(qv), None) => retriever.search(&request, Some(qv)),
                    (None, Some(hits)) => {
                        retriever.search_with_keyword_hits(&request, None, Some(hits))
                    }
                    (None, None) => retriever.search(&request, None),
                }
            } else {
                Vec::new()
            };

            // 4.2 utt 原文块检索（白名单内且闸门开启；严格按 persona 隔离）
            let utt_hits = if utt_persona_allowed {
                match persona_owned.as_deref() {
                    Some(puid) => retriever.search_utt(
                        &query_owned,
                        query_vec_snapshot.as_deref(),
                        utt_top_k,
                        Some(puid),
                    ),
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };

            (results, utt_hits)
        });

    // ---- 4.3 记忆子层过滤（L1 / L2 分层开关） ----
    // 只请求单一子层时，另一层既不进段落文本也不进结构化条目（服务层 include 语义）；
    // 两层全开（在线管线默认）时整个分支跳过，零开销、行为不变。
    if !(input.memory_layers.l1 && input.memory_layers.l2) {
        let before = results.len();
        results.retain(|r| input.memory_layers.allows(&r.layer));
        tracing::debug!(
            before,
            after = results.len(),
            l1 = input.memory_layers.l1,
            l2 = input.memory_layers.l2,
            "记忆子层过滤已应用（未请求的子层不参与段落与条目）"
        );
    }

    // ---- 5. 时间衰减（含访问加成）+ touch 刷新 + 重排 ----
    if !results.is_empty() {
        let decay_config_l1 = DecayConfig::from_core(input.decay, "l1");
        let decay_config_l2 = DecayConfig::from_core(input.decay, "l2");

        // 收集命中的 L1 文档 id（供后续 touch 接线刷新 last_accessed_at）
        let mut touched_l1_ids: Vec<Uuid> = Vec::new();

        for r in &mut results {
            let decay_config = if r.layer == "l2" {
                &decay_config_l2
            } else {
                &decay_config_l1
            };

            // salience: SearchResult 不携带此字段，使用中性值 0.5
            let salience = 0.5;
            // calc_retention = calc_decay_r + apply_access_boost：
            // 近期被访问的 L1（last_accessed_at 已由 touch 刷新）保留率保底，
            // 使"刚聊过的话题"在衰减排序中更易召回。
            let decay_factor = calc_retention(
                r.created_at,
                r.last_accessed_at,
                input.now_ms,
                salience,
                decay_config,
            );
            r.rrf_score *= decay_factor;

            if r.layer == "l1" {
                if let DocId::L1(id) = r.doc_id {
                    touched_l1_ids.push(id);
                }
            }

            tracing::trace!(
                doc_id = %r.doc_id,
                layer = %r.layer,
                last_accessed = ?r.last_accessed_at,
                decay_factor = format!("{:.4}", decay_factor),
                rrf_adjusted = format!("{:.4}", r.rrf_score),
                "时间衰减已应用（含访问加成）"
            );
        }

        // ---- 5.1 touch 接线：检索命中更新 last_accessed_at ----
        // 异步更新不阻塞检索：失败仅 warn（本次降级为无访问加成），
        // 成功使近期被检索的 L1 在下次检索中获得保底保留率（recent_boost_floor）。
        if !touched_l1_ids.is_empty() {
            let ids_for_touch = touched_l1_ids;
            if let Err(e) = input.storage.touch_l1(&ids_for_touch, input.now_ms).await {
                tracing::warn!(
                    count = ids_for_touch.len(),
                    error = %e,
                    "L1 访问时间刷新失败（touch 降级，访问加成本次不生效）"
                );
            } else {
                tracing::debug!(
                    count = ids_for_touch.len(),
                    "L1 访问时间已刷新（touch 接线，激活 recent_boost_*）"
                );
            }
        }

        // 重新按衰减后 rrf_score 降序排序
        results.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // ---- 5.2 通道命中计数（诊断；基于融合后结果） ----
        output.channels = RecallChannels {
            vector: results.iter().filter(|r| r.vector_score.is_some()).count(),
            bm25: results.iter().filter(|r| r.bm25_score.is_some()).count(),
            keyword: keyword_hits.as_ref().map(Vec::len).unwrap_or(0),
            graph: results.iter().filter(|r| r.graph_score.is_some()).count(),
        };
        output.fused_count = results.len();

        // ---- 5.3 Persona-Aware 过滤 + 段落渲染 ----
        let persona_kind = input
            .persona_uid
            .map(PersonaKind::from_uid)
            .unwrap_or(PersonaKind::Rama);
        let rag_config = RagConfig::from_retrieval_config(input.retrieval);
        let filtered = filter_by_persona(&results, persona_kind, &rag_config);
        output.filtered_count = filtered.len();

        if filtered.is_empty() {
            tracing::debug!("Persona-Aware 过滤后无结果");
        } else {
            let context = format_context_text(&filtered, &rag_config);

            // 记录"实际进入上下文文本"的文档标识（RAG 覆盖集合）。
            // 语义边界: 与 format_context_text 同批——只取过滤后、受
            // rag_max_memories 截断约束的文档（L1 uuid / L2 事件 id）；
            // 图谱实体无文档映射，不纳入覆盖集合。
            output.doc_labels = filtered
                .iter()
                .take(rag_config.max_memories)
                .filter(|r| matches!(&r.doc_id, DocId::L1(_) | DocId::L2(_)))
                .map(|r| r.doc_id.to_string())
                .collect();

            output.hits = filtered
                .iter()
                .take(rag_config.max_memories)
                .map(|r| RecallHit {
                    layer: r.layer.clone(),
                    id: r.doc_id.to_string(),
                    text: r.doc_summary.clone(),
                    score: r.rrf_score,
                    created_at: r.created_at,
                })
                .collect();

            tracing::debug!(
                total_results = results.len(),
                filtered = filtered.len(),
                context_chars = context.chars().count(),
                covered_labels = output.doc_labels.len(),
                "记忆上下文已组装（含时间衰减）"
            );

            output.memory_context = Some(context);
        }
    } else {
        tracing::debug!("无记忆上下文（utt 通道继续）");
    }

    // ---- 6. utt 原文片段渲染（预算裁剪在渲染内完成，不做块内截断） ----
    if !utt_hits.is_empty() {
        let rendered = crate::prompt::builder::render_utt_context(
            &utt_hits,
            input.utt.max_block_chars as usize,
        );
        if !rendered.is_empty() {
            tracing::debug!(
                persona_uid = input.persona_uid.unwrap_or("none"),
                hits = utt_hits.len(),
                budget_chars = input.utt.max_block_chars,
                "utt 原文片段已渲染（不记录内容）"
            );
            output.utt_context = Some(rendered);
        }
    } else if utt_persona_allowed {
        tracing::debug!("utt 原文块无命中，跳过注入");
    } else {
        tracing::debug!(
            utt_gate = input.gates.utt,
            persona_uid = input.persona_uid.unwrap_or("none"),
            "utt 通道未生效（闸门关闭或 persona 不在白名单），跳过原文注入"
        );
    }

    output
}

// =========================================================
// 辅助
// =========================================================

/// 返回当前 Unix 毫秒时间（与内核 `now_ms` 同源，便于调用方构造输入）。
pub fn current_now_ms() -> i64 {
    now_ms()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyword::service::KeywordService;
    use crate::retriever::{L1DocView, L2DocView, UttDocView};
    use ramaria_core::config::RamariaConfig;
    use std::sync::{Arc, RwLock};

    /// 用真实 SQLite（临时文件库）构造存储后端：召回只用到 touch_l1，无需 mock 全 trait。
    async fn test_storage(tag: &str) -> Arc<dyn StorageBackend> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应可读")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ramaria-recall-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
        let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
            .await
            .expect("测试库初始化应成功");
        Arc::new(ramaria_storage::SqliteStorage::new(pool))
    }

    fn config() -> RamariaConfig {
        RamariaConfig::default()
    }

    fn seeded_retriever(summary: &str, persona_uid: &str) -> RwLock<Retriever> {
        let mut retriever = Retriever::new();
        retriever.index_l1(&L1DocView {
            id: Uuid::new_v4(),
            summary: summary.to_string(),
            keywords: Some("工作压力,加班".to_string()),
            persona_uid: Some(persona_uid.to_string()),
            created_at: 1_000,
            salience: 0.8,
            last_accessed_at: None,
        });
        RwLock::new(retriever)
    }

    /// 正常检索：命中 L1 → 上下文文本、覆盖集合、结构化条目齐备。
    #[tokio::test]
    async fn hit_assembles_context_and_hits() {
        let cfg = config();
        let storage = test_storage("hit").await;
        let retriever = seeded_retriever("用户最近工作压力很大", "persona-0001");

        let output = assemble_recall(RecallInput {
            retriever: &retriever,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("persona-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;

        let context = output.memory_context.expect("应组装记忆上下文");
        assert!(context.contains("工作压力"), "上下文应含摘要: {context}");
        assert_eq!(output.doc_labels.len(), 1, "覆盖集合应含命中 L1");
        assert_eq!(output.hits.len(), 1);
        assert_eq!(output.fused_count, 1);
        assert_eq!(output.filtered_count, 1);
    }

    /// 闸门全关：不检索、不生成向量，输出为空结构。
    #[tokio::test]
    async fn gates_closed_returns_empty() {
        let cfg = config();
        let storage = test_storage("gates").await;
        let retriever = seeded_retriever("用户最近工作压力很大", "persona-0001");

        let output = assemble_recall(RecallInput {
            retriever: &retriever,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("persona-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: false,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;

        assert!(output.memory_context.is_none());
        assert!(output.utt_context.is_none());
        assert!(output.hits.is_empty());
        assert_eq!(output.fused_count, 0);
    }

    /// 索引未加载（服务层懒加载槽为 None）：空召回且不报错。
    #[tokio::test]
    async fn unloaded_slot_returns_empty() {
        let cfg = config();
        let storage = test_storage("slot").await;
        let slot: RwLock<Option<Retriever>> = RwLock::new(None);

        let output = assemble_recall(RecallInput {
            retriever: &slot,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("persona-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;

        assert!(output.memory_context.is_none());
        assert_eq!(output.fused_count, 0);
    }

    /// Persona-Aware 过滤：低 share 的 L2 事件对角色类 persona 不可见。
    #[tokio::test]
    async fn persona_filter_hides_low_share_event() {
        let cfg = config();
        let storage = test_storage("filter").await;
        let retriever = RwLock::new(Retriever::new());
        {
            let mut guard = retriever.write().expect("锁可用");
            guard.index_l2(&L2DocView {
                id: 42,
                title: "低分享事件".to_string(),
                summary: "用户提到秘密".to_string(),
                keywords: Some("秘密".to_string()),
                attitude: None,
                paraphrase: None,
                persona_uid: "char-0001".to_string(),
                share: 0.1,
                confidence: 0.9,
                created_at: 1_000,
                salience: 0.8,
            });
        }

        let output = assemble_recall(RecallInput {
            retriever: &retriever,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "秘密",
            persona_uid: Some("char-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;

        assert_eq!(output.fused_count, 1, "融合层应命中事件");
        assert_eq!(output.filtered_count, 0, "share 低于角色阈值应被过滤");
        assert!(output.memory_context.is_none());
    }

    /// 记忆子层开关：只请求 L1 时不返回 L2（反向亦然），段落文本同步收窄。
    #[tokio::test]
    async fn memory_layer_switch_filters_sublayers() {
        let cfg = config();
        let storage = test_storage("layers").await;
        let retriever = RwLock::new(Retriever::new());
        {
            let mut guard = retriever.write().expect("锁可用");
            guard.index_l1(&L1DocView {
                id: Uuid::new_v4(),
                summary: "用户最近工作压力很大（摘要侧）".to_string(),
                keywords: Some("工作压力".to_string()),
                persona_uid: Some("char-0001".to_string()),
                created_at: 1_000,
                salience: 0.8,
                last_accessed_at: None,
            });
            guard.index_l2(&L2DocView {
                id: 7,
                title: "工作压力事件".to_string(),
                summary: "用户在群聊里被点名批评（事件侧）".to_string(),
                keywords: Some("工作压力".to_string()),
                attitude: None,
                paraphrase: None,
                persona_uid: "char-0001".to_string(),
                share: 1.0,
                confidence: 0.9,
                created_at: 1_000,
                salience: 0.8,
            });
        }

        let run = |layers: RecallMemoryLayers| {
            let retriever = &retriever;
            let storage = storage.as_ref();
            let cfg = &cfg;
            async move {
                assemble_recall(RecallInput {
                    retriever,
                    keyword_mirror: &RwLock::new(KeywordService::new()),
                    storage,
                    embedding: None,
                    query: "工作压力",
                    persona_uid: Some("char-0001"),
                    retrieval: &cfg.retrieval,
                    decay: &cfg.decay,
                    utt: &cfg.utt,
                    gates: RecallGates {
                        memory_rag: true,
                        utt: false,
                    },
                    memory_layers: layers,
                    now_ms: 2_000,
                })
                .await
            }
        };

        // 先确认两层都能被命中（否则下面的断言没有意义）
        let both = run(RecallMemoryLayers::both()).await;
        assert!(
            both.hits.iter().any(|h| h.layer == "l1") && both.hits.iter().any(|h| h.layer == "l2"),
            "两层应都能命中: {:?}",
            both.hits
        );

        // 只要 L1：无 L2 条目、段落不含事件侧文本
        let l1_only = run(RecallMemoryLayers::l1_only()).await;
        assert!(
            l1_only.hits.iter().all(|h| h.layer == "l1"),
            "不应含 L2 条目"
        );
        let context = l1_only.memory_context.unwrap_or_default();
        assert!(context.contains("摘要侧"), "应含 L1 文本: {context}");
        assert!(!context.contains("事件侧"), "不应含 L2 文本: {context}");

        // 只要 L2：无 L1 条目、段落不含摘要侧文本
        let l2_only = run(RecallMemoryLayers::l2_only()).await;
        assert!(
            l2_only.hits.iter().all(|h| h.layer == "l2"),
            "不应含 L1 条目"
        );
        let context = l2_only.memory_context.unwrap_or_default();
        assert!(context.contains("事件侧"), "应含 L2 文本: {context}");
        assert!(!context.contains("摘要侧"), "不应含 L1 文本: {context}");

        // 开关判定：图谱等非摘要层不受约束
        assert!(RecallMemoryLayers::l2_only().allows("graph"));
        assert!(RecallMemoryLayers::l1_only().any());
        assert!(!RecallMemoryLayers::l2_only().allows("l1"));
    }

    /// utt 原文通道：白名单内 persona + 闸门开启 → 渲染原文片段；白名单外不注入。
    #[tokio::test]
    async fn utt_respects_persona_whitelist() {
        let cfg = config();
        let storage = test_storage("utt").await;
        let retriever = RwLock::new(Retriever::new());
        {
            let mut guard = retriever.write().expect("锁可用");
            guard.index_utt(
                &UttDocView {
                    id: 1,
                    persona_uid: "char-0001".to_string(),
                    session_id: Uuid::new_v4(),
                    block_text: "今天天气真好我们一起去公园".to_string(),
                    msg_count: 2,
                    created_at: 1_000,
                },
                None,
            );
        }

        // 白名单内（char-0001）→ 注入
        let output = assemble_recall(RecallInput {
            retriever: &retriever,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "公园",
            persona_uid: Some("char-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: false,
                utt: true,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;
        assert!(output.utt_context.is_some(), "白名单内应注入原文片段");

        // 白名单外（rama-0001）→ 不注入
        let output = assemble_recall(RecallInput {
            retriever: &retriever,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "公园",
            persona_uid: Some("rama-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: false,
                utt: true,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;
        assert!(output.utt_context.is_none(), "白名单外不注入原文");
    }

    /// 关键词镜像通道：镜像为空时静默跳过（不影响 BM25 命中）。
    #[tokio::test]
    async fn keyword_mirror_empty_degrades_silently() {
        let cfg = config();
        let storage = test_storage("mirror").await;
        let retriever = seeded_retriever("用户讨论Rust编程", "persona-0001");

        let output = assemble_recall(RecallInput {
            retriever: &retriever,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "Rust",
            persona_uid: Some("persona-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;

        assert!(output.memory_context.is_some(), "BM25 通道应命中");
        assert_eq!(output.channels.keyword, 0, "镜像为空时关键词通道命中数为 0");
    }

    /// 对照测试：同一输入经两种句柄（在线管线的 `RwLock<Retriever>` 与服务层的
    /// `RwLock<Option<Retriever>>` 懒加载槽）产出完全一致 —— 召回同源的直接证据。
    #[tokio::test]
    async fn both_handles_produce_identical_output() {
        let cfg = config();
        let storage = test_storage("both").await;

        // 两份内容相同的索引（文档 id 不同，不参与文本/分数比对）
        let make_doc = || L1DocView {
            id: Uuid::new_v4(),
            summary: "用户最近工作压力很大".to_string(),
            keywords: Some("工作压力,加班".to_string()),
            persona_uid: Some("persona-0001".to_string()),
            created_at: 1_000,
            salience: 0.8,
            last_accessed_at: None,
        };
        let mut plain_retriever = Retriever::new();
        plain_retriever.index_l1(&make_doc());
        let plain = RwLock::new(plain_retriever);

        let mut slot_retriever = Retriever::new();
        slot_retriever.index_l1(&make_doc());
        let slot: RwLock<Option<Retriever>> = RwLock::new(Some(slot_retriever));

        let out_plain = assemble_recall(RecallInput {
            retriever: &plain,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("persona-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;

        let out_slot = assemble_recall(RecallInput {
            retriever: &slot,
            keyword_mirror: &RwLock::new(KeywordService::new()),
            storage: storage.as_ref(),
            embedding: None,
            query: "工作压力",
            persona_uid: Some("persona-0001"),
            retrieval: &cfg.retrieval,
            decay: &cfg.decay,
            utt: &cfg.utt,
            gates: RecallGates {
                memory_rag: true,
                utt: false,
            },
            memory_layers: RecallMemoryLayers::default(),
            now_ms: 2_000,
        })
        .await;

        assert_eq!(
            out_plain.memory_context, out_slot.memory_context,
            "两种句柄的上下文文本必须一致"
        );
        assert_eq!(out_plain.channels, out_slot.channels);
        assert_eq!(out_plain.fused_count, out_slot.fused_count);
        assert_eq!(out_plain.filtered_count, out_slot.filtered_count);
        let texts = |out: &RecallOutput| -> Vec<(String, String)> {
            out.hits
                .iter()
                .map(|h| (h.layer.clone(), format!("{:.6}", h.score)))
                .collect()
        };
        assert_eq!(texts(&out_plain), texts(&out_slot));
    }

    /// 通道命中计数映射：四个通道都出现在 `as_map` 输出中（含 0 值）。
    #[test]
    fn channels_map_contains_all_channels() {
        let map = RecallChannels {
            vector: 2,
            bm25: 3,
            keyword: 1,
            graph: 0,
        }
        .as_map();
        assert_eq!(map.get("vector"), Some(&2));
        assert_eq!(map.get("bm25"), Some(&3));
        assert_eq!(map.get("keyword"), Some(&1));
        assert_eq!(map.get("graph"), Some(&0));
    }
}
