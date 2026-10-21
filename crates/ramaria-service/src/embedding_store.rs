//! crates/ramaria-service/src/embedding_store.rs - 记忆向量写入门面
//!
//! 设计特点:
//! - 统一由服务层把生成的向量写入 memory_vectors（记忆算法层不感知存储表）
//! - 模型指纹 model_id@dim#size：size 为单行向量字节数（dim×4），加载时校验 BLOB 长度
//! - 全程降级：provider 不可用、维度不符、写入失败均返回 false 并记日志，不阻塞主流程
//! - 只做写入；读取与缺失补算由索引加载路径负责

use ramaria_core::traits::EmbeddingModelInfo;
use ramaria_core::types::{MemoryVector, now_ms};
use tracing::{debug, warn};

use crate::engine::Engine;

/// 计算模型指纹（`model_id@dim#size`）。
///
/// 参数:
/// - `info`: 嵌入模型信息。
///
/// 返回:
/// - 非空维度时返回 `{model_id}@{dim}#{dim*4}`。
/// - 维度为 0（模型未加载 / 维度未知）时返回空串，调用方据此跳过写入。
pub(crate) fn model_fingerprint(info: &EmbeddingModelInfo) -> String {
    if info.dimension == 0 {
        return String::new();
    }
    format!(
        "{}@{}#{}",
        info.model_id,
        info.dimension,
        info.dimension * 4
    )
}

/// 写入一条记忆向量。
///
/// 参数:
/// - `engine`: 服务层引擎（取嵌入 provider 与存储）。
/// - `doc_type`: 文档类型（l1 / l2 / utt）。
/// - `doc_id`: 文档标识。
/// - `vector`: 向量分量。
///
/// 返回:
/// - `true`: provider 可用且写入成功。
/// - `false`: provider 不可用、维度为 0、向量长度与模型维度不符、指纹为空或写入失败
///   （后者记 warn）。
pub(crate) async fn store(engine: &Engine, doc_type: &str, doc_id: &str, vector: &[f32]) -> bool {
    if !engine.config().retrieval.enable_vector {
        debug!(doc_type, doc_id, "向量通道已关闭，跳过记忆向量写入");
        return false;
    }
    let Some(provider) = engine.embedding() else {
        debug!(doc_type, doc_id, "嵌入 provider 不可用，跳过记忆向量写入");
        return false;
    };
    if !provider.is_available() {
        debug!(doc_type, doc_id, "嵌入模型不可用，跳过记忆向量写入");
        return false;
    }

    let info = provider.model_info();
    let fingerprint = model_fingerprint(&info);
    if fingerprint.is_empty() {
        debug!(doc_type, doc_id, "嵌入模型维度为 0，跳过记忆向量写入");
        return false;
    }
    if vector.len() != info.dimension {
        debug!(
            doc_type,
            doc_id,
            len = vector.len(),
            dim = info.dimension,
            "向量长度与模型维度不符，跳过记忆向量写入"
        );
        return false;
    }

    let row = MemoryVector {
        doc_type: doc_type.to_string(),
        doc_id: doc_id.to_string(),
        model: fingerprint,
        dim: info.dimension as u32,
        vector: vector.to_vec(),
        updated_at: now_ms(),
    };
    match engine.storage().upsert_memory_vector(&row).await {
        Ok(()) => true,
        Err(e) => {
            warn!(doc_type, doc_id, error = %e, "记忆向量写入失败（不影响主流程）");
            false
        }
    }
}

#[cfg(test)]
mod tests;
