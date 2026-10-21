//! crates/ramaria-core/src/types/vector.rs - Ramaria 记忆向量数据类型模块
//!
//! 设计特点:
//! - 定义记忆向量行（L1 摘要 / L2 事件 / utt 话语块的向量持久化载体）
//! - 提供 f32 小端字节序列的编码与解码，供存储层 BLOB 列与内存索引共用
//! - 与业务表解耦：doc_id 为多态标识（L1 用 UUID 串，L2 / utt 用自增 id 十进串）

use crate::error::{RamariaError, RamariaResult};

/// L1 摘要向量文档类型。
pub const VECTOR_DOC_L1: &str = "l1";
/// L2 事件向量文档类型。
pub const VECTOR_DOC_L2: &str = "l2";
/// utt 话语块向量文档类型。
pub const VECTOR_DOC_UTT: &str = "utt";

// =========================================================
// 记忆向量行
// =========================================================

/// 记忆向量行——一条记忆载体在特定模型下的向量。
///
/// 职责:
/// - 承载向量持久化所需的全部字段，供存储层 upsert 与批量加载。
/// - 与 `memory_vectors` 表一行对应。
///
/// 字段约定:
/// - `doc_type`: 文档类型，取值 [`VECTOR_DOC_L1`] / [`VECTOR_DOC_L2`] / [`VECTOR_DOC_UTT`]。
/// - `doc_id`: 文档标识；L1 为 UUID 字符串，L2 / utt 为自增 id 的十进制字符串。
/// - `model`: 模型指纹 `model_id@dim#size`，size 为单行向量字节数（dim×4）。
/// - `dim`: 向量维度。
/// - `vector`: 向量分量（f32）；落库时按小端序编码为 BLOB。
/// - `updated_at`: 写入时刻（Unix 毫秒）。
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryVector {
    /// 文档类型（l1 / l2 / utt）
    pub doc_type: String,
    /// 文档标识
    pub doc_id: String,
    /// 模型指纹 model_id@dim#size
    pub model: String,
    /// 向量维度
    pub dim: u32,
    /// 向量分量
    pub vector: Vec<f32>,
    /// 写入时刻（Unix 毫秒）
    pub updated_at: i64,
}

// =========================================================
// 向量字节编解码
// =========================================================

/// 将向量编码为 f32 小端字节序列。
///
/// 参数:
/// - `vector`: 向量分量。
///
/// 返回:
/// - 按小端序逐分量拼接的字节序列（长度 = 分量数 × 4）。
pub fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for v in vector {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// 将 f32 小端字节序列解码为向量。
///
/// 参数:
/// - `bytes`: 字节序列，长度须为 4 的整数倍。
///
/// 返回:
/// - 成功时返回解码后的向量；维度由字节长度推导，不读取外部维度声明。
/// - 空字节或长度非 4 的整数倍时返回 `RamariaError::Validation`。
pub fn decode_vector(bytes: &[u8]) -> RamariaResult<Vec<f32>> {
    if bytes.is_empty() {
        return Err(RamariaError::validation("向量字节为空"));
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(RamariaError::validation(format!(
            "向量字节长度 {} 不是 4 的整数倍",
            bytes.len()
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

#[cfg(test)]
mod tests;
