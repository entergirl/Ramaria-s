//! crates/ramaria-service/src/index/vectors.rs - Ramaria 索引向量加载与缺失补算
//!
//! 设计特点:
//! - 构建期只从向量表读取（按模型指纹过滤），缺失条目在索引可用后批补算
//! - 向量路径受三重门禁：召回开关 / provider 可用 / 模型指纹非空
//! - 维度校验：行维度与向量长度都必须等于模型维度，不符跳过
//! - 补算降级：整批失败 / 超时改逐条嵌入，单条失败只跳过该条
//! - 批大小严格取 `[index].embed_batch_size`（至少 1）
//! - 日志只记文档类型 / 标识与错误，不记文本内容

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ramaria_core::lock::write_recover;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_core::types::{
    IndexStatus, MemoryVector, UttBlock, VECTOR_DOC_L1, VECTOR_DOC_L2, VECTOR_DOC_UTT, now_ms,
};
use ramaria_memory::VectorIndex;
use ramaria_memory::retriever::{L1DocView, L2DocView};

use crate::engine::Engine;

/// 单次嵌入调用（批量 / 单条）的超时上限。
const EMBED_TIMEOUT: Duration = Duration::from_secs(30);

/// 向量文档键：`(doc_type, doc_id)`。
pub(crate) type VectorKey = (String, String);

/// 本次构建的向量路径计划（None = 走无向量路径）。
pub(crate) struct VectorPlan {
    pub(crate) provider: Arc<dyn EmbeddingProvider>,
    pub(crate) model_id: String,
    pub(crate) fingerprint: String,
    pub(crate) dimension: usize,
}

impl VectorPlan {
    /// 评估本次构建是否启用向量路径。
    ///
    /// 返回:
    /// - `Some(plan)`: 召回开关开启、provider 可用且模型指纹非空。
    /// - `None`: 任一条件不满足（索引只建 BM25 + 图谱）。
    pub(crate) fn detect(engine: &Engine) -> Option<Self> {
        let config = engine.config();
        if !config.retrieval.enable_vector {
            tracing::debug!("向量通道已关闭，本次索引构建跳过向量路径");
            return None;
        }
        let Some(provider) = engine.embedding_ref() else {
            tracing::debug!("嵌入模型未配置，本次索引构建跳过向量路径");
            return None;
        };
        if !provider.is_available() {
            tracing::debug!("嵌入模型不可用，本次索引构建跳过向量路径");
            return None;
        }
        let info = provider.model_info();
        let fingerprint = crate::embedding_store::model_fingerprint(&info);
        if fingerprint.is_empty() {
            tracing::debug!("嵌入模型维度为 0，本次索引构建跳过向量路径");
            return None;
        }
        Some(Self {
            provider,
            model_id: info.model_id,
            fingerprint,
            dimension: info.dimension,
        })
    }
}

/// 把向量行装入查找表（维度不符的行跳过并记 debug）。
pub(crate) fn load(rows: Vec<MemoryVector>, dimension: usize) -> HashMap<VectorKey, Vec<f32>> {
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        if row.dim as usize != dimension || row.vector.len() != dimension {
            tracing::debug!(
                doc_type = %row.doc_type,
                doc_id = %row.doc_id,
                dim = row.dim,
                expected = dimension,
                "记忆向量维度不符，跳过该行"
            );
            continue;
        }
        map.insert((row.doc_type, row.doc_id), row.vector);
    }
    map
}

/// 取一条文档的向量副本（缺失返回 None）。
pub(crate) fn lookup(
    map: &HashMap<VectorKey, Vec<f32>>,
    doc_type: &str,
    doc_id: &str,
) -> Option<Vec<f32>> {
    map.get(&(doc_type.to_string(), doc_id.to_string()))
        .cloned()
}

/// 缺失向量的文档条目。
///
/// 字段约定:
/// - `doc_type`: `l1` / `l2` / `utt`。
/// - `doc_id`: 文档标识字符串。
/// - `text`: 待嵌入文本（L1 摘要 / L2 标题 / utt 块正文；不写日志）。
/// - `created_at`: 条目创建时间（写入内存向量索引的时间基准）。
pub(crate) struct MissingVector {
    pub(crate) doc_type: String,
    pub(crate) doc_id: String,
    pub(crate) text: String,
    pub(crate) created_at: i64,
}

/// 收集未拿到向量的文档条目。
pub(crate) fn collect_missing(
    l1_views: &[L1DocView],
    l2_views: &[L2DocView],
    utt_blocks: &[UttBlock],
    map: &HashMap<VectorKey, Vec<f32>>,
) -> Vec<MissingVector> {
    let mut missing = Vec::new();
    for doc in l1_views {
        if lookup(map, VECTOR_DOC_L1, &doc.id.to_string()).is_none() {
            missing.push(MissingVector {
                doc_type: VECTOR_DOC_L1.to_string(),
                doc_id: doc.id.to_string(),
                text: doc.summary.clone(),
                created_at: doc.created_at,
            });
        }
    }
    for doc in l2_views {
        if lookup(map, VECTOR_DOC_L2, &doc.id.to_string()).is_none() {
            missing.push(MissingVector {
                doc_type: VECTOR_DOC_L2.to_string(),
                doc_id: doc.id.to_string(),
                text: doc.title.clone(),
                created_at: doc.created_at,
            });
        }
    }
    for block in utt_blocks {
        if lookup(map, VECTOR_DOC_UTT, &block.id.to_string()).is_none() {
            missing.push(MissingVector {
                doc_type: VECTOR_DOC_UTT.to_string(),
                doc_id: block.id.to_string(),
                text: block.block_text.clone(),
                created_at: block.created_at,
            });
        }
    }
    missing
}

/// 分批补算缺失向量。
///
/// 参数:
/// - `engine`: 服务层引擎（写入向量表 / 推进状态）。
/// - `plan`: 本次向量路径计划（provider / 指纹 / 维度）。
/// - `missing`: 缺失条目（顺序即补算顺序）。
/// - `loaded`: 已加载向量文档数（成功一条加一）。
/// - `total`: 向量文档总数（进度分母）。
/// - `started_ms`: 构建开始时刻（进度耗时基准）。
///
/// 返回:
/// - `(success, failed)`: 成功写入条数与失败条数。
///
/// 降级:
/// - 整批嵌入失败 / 超时 / 条数不符 → 改逐条嵌入；
/// - 单条嵌入失败 / 超时 / 维度不符 / 写库失败 → 记 warn 并跳过该条。
pub(crate) async fn backfill(
    engine: &Engine,
    plan: &VectorPlan,
    missing: &[MissingVector],
    loaded: &mut usize,
    total: usize,
    started_ms: i64,
) -> (usize, usize) {
    let batch_size = engine.config().index.embed_batch_size.max(1) as usize;
    let mut success = 0usize;
    let mut failed = 0usize;

    for batch in missing.chunks(batch_size) {
        let texts: Vec<&str> = batch.iter().map(|item| item.text.as_str()).collect();
        let mut vectors: Vec<Option<Vec<f32>>> = vec![None; batch.len()];

        match tokio::time::timeout(EMBED_TIMEOUT, plan.provider.embed_batch(&texts)).await {
            Ok(Ok(result)) if result.len() == batch.len() => {
                for (slot, vector) in vectors.iter_mut().zip(result) {
                    *slot = Some(vector);
                }
            }
            Ok(Ok(result)) => {
                tracing::warn!(
                    expected = batch.len(),
                    got = result.len(),
                    "批量向量化返回条数不符，改逐条补算"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "批量向量化失败，改逐条补算");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_secs = EMBED_TIMEOUT.as_secs(),
                    "批量向量化超时，改逐条补算"
                );
            }
        }

        for (item, slot) in batch.iter().zip(vectors.iter_mut()) {
            if slot.is_some() {
                continue;
            }
            match tokio::time::timeout(EMBED_TIMEOUT, plan.provider.embed(&item.text)).await {
                Ok(Ok(vector)) => *slot = Some(vector),
                Ok(Err(e)) => {
                    failed += 1;
                    tracing::warn!(
                        doc_type = %item.doc_type,
                        doc_id = %item.doc_id,
                        error = %e,
                        "单条向量化失败，跳过该条目"
                    );
                }
                Err(_) => {
                    failed += 1;
                    tracing::warn!(
                        doc_type = %item.doc_type,
                        doc_id = %item.doc_id,
                        timeout_secs = EMBED_TIMEOUT.as_secs(),
                        "单条向量化超时，跳过该条目"
                    );
                }
            }
        }

        for (item, slot) in batch.iter().zip(vectors) {
            let Some(vector) = slot else {
                continue;
            };
            if vector.len() != plan.dimension {
                failed += 1;
                tracing::debug!(
                    doc_type = %item.doc_type,
                    doc_id = %item.doc_id,
                    len = vector.len(),
                    expected = plan.dimension,
                    "补算向量维度不符，跳过该条目"
                );
                continue;
            }
            let row = MemoryVector {
                doc_type: item.doc_type.clone(),
                doc_id: item.doc_id.clone(),
                model: plan.fingerprint.clone(),
                dim: plan.dimension as u32,
                vector: vector.clone(),
                updated_at: now_ms(),
            };
            match engine.storage_ref().upsert_memory_vector(&row).await {
                Ok(()) => {
                    index_vector(
                        engine,
                        &item.doc_type,
                        &item.doc_id,
                        vector,
                        item.created_at,
                    );
                    *loaded += 1;
                    success += 1;
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(
                        doc_type = %item.doc_type,
                        doc_id = %item.doc_id,
                        error = %e,
                        "补算向量写入失败，跳过该条目"
                    );
                }
            }
        }

        engine.set_index_progress(
            IndexStatus::Building,
            *loaded,
            total,
            Some(plan.model_id.clone()),
            now_ms().saturating_sub(started_ms) as u64,
        );
    }

    (success, failed)
}

/// 把补算成功的向量写入当前内存检索器。
fn index_vector(engine: &Engine, doc_type: &str, doc_id: &str, vector: Vec<f32>, created_at: i64) {
    // utt 块的向量标签沿用 `L0:`（与检索通道解析口径一致）
    let layer = if doc_type == VECTOR_DOC_UTT {
        "l0"
    } else {
        doc_type
    };
    let label = ramaria_memory::vector::make_vector_label(layer, doc_id);
    let slot = engine.retriever_slot();
    let mut guard = write_recover(&*slot, "index.retriever_slot");
    if let Some(retriever) = guard.as_mut() {
        retriever.vector_mut().add(&label, vector, created_at);
    }
}
