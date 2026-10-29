//! crates/ramaria-service/src/index/fallback.rs - Ramaria 索引未就绪时的降级检索器
//!
//! 设计特点:
//! - 只读降级：直接从存储收集未吸收 L1 与 L2 事件建一次性 BM25 索引，不写库、不改索引状态
//! - 通道收窄：向量 / 图谱 / 关键词镜像一律关闭，召回只走 BM25（配置来自同一份检索配置）
//! - 语料同源：目标 persona 的未吸收 L1 + 无主 L1 + 该 persona 的 L2 事件（与重建路径一致）
//! - 失败即空记忆：任一步 I/O 失败记 warn 并返回 None，调用方按无记忆处理
//! - 不产出向量：索引条目一律不带向量，降级路径不触发任何嵌入调用

use std::sync::RwLock;

use ramaria_memory::decay::DecayConfig;
use ramaria_memory::recall::RetrieverSource;
use ramaria_memory::retriever::{Retriever, RetrieverConfig, SearchResult};

use crate::engine::Engine;

use super::build::load_persona_events;
use super::view::{l1_view, l2_view};

// =========================================================
// 降级检索器构建
// =========================================================

/// 以存储直读方式构建仅 BM25 的临时检索器（索引未就绪时的召回降级）。
///
/// 职责:
/// - 在内存索引尚未构建（或正在构建）时，让本次召回只依赖存储即可命中 L1 / L2 文本；
/// - 不进入构建路径：不写库、不改索引状态与代次、不置脏标记。
///
/// 参数:
/// - `engine`: 服务层引擎（读检索配置与存储）。
/// - `persona_uid`: 目标人格（只装载该 persona 的 L1 / L2，另加无主 L1 与重建口径一致）。
///
/// 返回:
/// - `Some(retriever)`: 构建完成（即使 0 文档）；
/// - `None`: 关键读取失败，调用方按空记忆处理。
pub(crate) async fn bm25_only_retriever(engine: &Engine, persona_uid: &str) -> Option<Retriever> {
    let storage = engine.storage_ref().as_ref();

    // 通道收窄：只保留 BM25；向量 / 图谱 / 关键词镜像在本路径不参与融合
    let mut config = RetrieverConfig::from_retrieval_config(&engine.config().retrieval);
    config.enable_bm25 = true;
    config.enable_vector = false;
    config.enable_graph = false;
    config.enable_keyword_channel = false;
    let mut retriever = Retriever::new();
    *retriever.config_mut() = config;

    // 词典与重建同口径：读取失败 → 空词典（纯 bigram 分词）
    let dictionary = match storage.list_established_keywords().await {
        Ok(dictionary) => dictionary,
        Err(e) => {
            tracing::debug!(error = %e, "降级检索器：读取已确认词表失败，按空词典构建");
            Vec::new()
        }
    };
    retriever.set_bm25_dictionary(&dictionary);

    let l1_docs = match storage.list_unabsorbed_l1(persona_uid).await {
        Ok(docs) => docs,
        Err(e) => {
            tracing::warn!(persona_uid, error = %e, "降级检索器：读取未吸收 L1 失败，本次按无记忆处理");
            return None;
        }
    };
    let unbound_docs = match storage.list_unabsorbed_l1_unbound().await {
        Ok(docs) => docs,
        Err(e) => {
            tracing::warn!(persona_uid, error = %e, "降级检索器：读取无主 L1 失败，本次按无记忆处理");
            return None;
        }
    };
    let events = match load_persona_events(engine, storage, persona_uid).await {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(persona_uid, error = %e, "降级检索器：读取 L2 事件失败，本次按无记忆处理");
            return None;
        }
    };

    for l1 in l1_docs.iter().chain(unbound_docs.iter()) {
        retriever.index_l1(&l1_view(l1));
    }
    for event in &events {
        retriever.index_l2(&l2_view(event));
    }

    tracing::debug!(
        persona_uid,
        l1 = l1_docs.len() + unbound_docs.len(),
        l2 = events.len(),
        "索引未就绪：已用存储直读 BM25 构建降级检索器"
    );
    Some(retriever)
}

// =========================================================
// 召回视图选择
// =========================================================

/// 本次召回使用的检索器视图：共享槽与降级实例二选一。
///
/// 说明:
/// - `RetrieverSource` 带泛型方法，不能作为 trait object 传递，故用本枚举统一入口；
/// - 降级实例由调用方以局部变量持有，视图只借用其生命周期。
pub(crate) enum RecallRetriever<'a> {
    /// 共享槽内的检索器（`None` = 未加载，按空记忆处理）。
    Slot(&'a RwLock<Option<Retriever>>),
    /// 存储直读的降级检索器（索引未就绪时）。
    Fallback(&'a Retriever),
}

impl RetrieverSource for RecallRetriever<'_> {
    fn with_retriever<R>(&self, f: impl FnOnce(Option<&Retriever>) -> R) -> R {
        match self {
            Self::Slot(slot) => slot.with_retriever(f),
            Self::Fallback(retriever) => retriever.with_retriever(f),
        }
    }

    fn search_narrative(
        &self,
        query: &str,
        persona_uid: &str,
        top_k: usize,
        now_ms: i64,
        decay_config: &DecayConfig,
    ) -> Vec<SearchResult> {
        match self {
            Self::Slot(slot) => {
                slot.search_narrative(query, persona_uid, top_k, now_ms, decay_config)
            }
            Self::Fallback(retriever) => {
                retriever.search_narrative(query, persona_uid, top_k, now_ms, decay_config)
            }
        }
    }
}
