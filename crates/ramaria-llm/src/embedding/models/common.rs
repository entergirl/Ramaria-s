//! rust/crates/ramaria-llm/src/embedding/models/common.rs - 嵌入模型共享工具
//!
//! 设计特点:
//! - 提供 BERT 和 LLaMA/Qwen 编码器共用的纯函数工具
//! - L2 归一化：对任意维度的 f32 向量做单位化，零向量安全
//! - safetensors header 读取：从文件末尾 8 字节反向定位并读取 JSON header
//! - 所有函数零副作用，纯计算或纯 I/O

use candle_core::{DType, Device, Tensor};
use ramaria_core::error::RamariaResult;
use std::path::Path;

// =========================================================
// L2 归一化（BERT 和 LLaMA/Qwen 共用）
// =========================================================

/// 对张量执行 L2 归一化。
///
/// 参数:
/// - `vector`: 一维张量 `[hidden_size]`。
/// - `device`: 计算设备。
///
/// 返回:
/// - L2 归一化后的张量。若原始向量范数极小（< 1e-8），返回零向量。
///
/// 数学:
/// - `normalized[i] = vector[i] / |vector|₂`
pub(crate) fn l2_normalize(vector: &Tensor, device: &Device) -> RamariaResult<Tensor> {
    let squared = vector
        .sqr()
        .map_err(|e| ramaria_core::error::RamariaError::embedding(format!("L2 sqr 失败: {}", e)))?;
    let sum_sq = squared.sum_all().map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("L2 sum_all 失败: {}", e))
    })?;
    let l2_norm = sum_sq.sqrt().map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("L2 sqrt 失败: {}", e))
    })?;

    let norm_scalar = l2_norm.to_scalar::<f32>().map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("L2 scalar 失败: {}", e))
    })?;

    if norm_scalar < 1e-8 {
        // 零向量或极小向量，返回零向量（避免除以零）
        let dim = vector.dims1().map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("dim 获取失败: {}", e))
        })?;
        return Tensor::zeros(dim, DType::F32, device).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!("zeros 失败: {}", e))
        });
    }

    // vector: [H], l2_norm: scalar [] → broadcast_div 广播标量
    vector.broadcast_div(&l2_norm).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("L2 broadcast_div 失败: {}", e))
    })
}

// =========================================================
// safetensors header 读取（架构检测 + 键名日志共用）
// =========================================================

/// 从 safetensors 文件读取 JSON header 的原始字节（兼容新旧格式）。
///
/// safetensors 有两种文件格式:
/// - **新格式** (v0.3+, HuggingFace 默认): [tensor data] + [JSON header] + [header_size: u64 LE]
/// - **旧格式** (v0.1-0.2): [header_size: u64 LE] + [JSON header] + [tensor data]
///
/// 检测策略: 先尝试新格式（末尾 8 字节），若 header_size 超出合理范围则尝试旧格式（开头 8 字节）。
/// candle 的 `safetensors` crate 透明支持两种格式，此处手动实现以保持一致。
///
/// 参数:
/// - `st_path`: safetensors 文件路径。
///
/// 返回:
/// - header JSON 的原始字节（`Vec<u8>`），可由调用方解析为键名列表或做字符串匹配。
///
/// 说明:
/// - 只读取 header（通常 < 100KB），不加载权重数据，非常快。
/// - 架构检测（`models/mod.rs`）和键名日志（`llama.rs`）共用此函数。
pub(crate) fn read_safetensors_header(st_path: &Path) -> RamariaResult<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(st_path).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "无法打开 safetensors: {} — {}",
            st_path.display(),
            e
        ))
    })?;
    let file_len = file
        .metadata()
        .map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "无法获取文件元数据: {} — {}",
                st_path.display(),
                e
            ))
        })?
        .len();

    if file_len < 8 {
        return Err(ramaria_core::error::RamariaError::embedding(
            "safetensors 文件过小（< 8 bytes），可能损坏",
        ));
    }

    // header_size 合理性检查: (0, file_len - 8] 且 < 100MB
    let is_reasonable = |size: usize| -> bool {
        size > 0 && size <= (file_len as usize).saturating_sub(8) && size < 100 * 1024 * 1024
    };

    // ---- 策略 1: 新格式（header 在文件末尾） ----
    let mut header_size_buf = [0u8; 8];
    file.seek(SeekFrom::End(-8)).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("safetensors seek 失败: {}", e))
    })?;
    file.read_exact(&mut header_size_buf).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!("safetensors read 失败: {}", e))
    })?;
    let header_size_new = u64::from_le_bytes(header_size_buf) as usize;

    if is_reasonable(header_size_new) {
        file.seek(SeekFrom::End(-8 - header_size_new as i64))
            .map_err(|e| {
                ramaria_core::error::RamariaError::embedding(format!(
                    "safetensors header seek 失败: {}",
                    e
                ))
            })?;
        let mut header_bytes = vec![0u8; header_size_new];
        file.read_exact(&mut header_bytes).map_err(|e| {
            ramaria_core::error::RamariaError::embedding(format!(
                "safetensors header read 失败: {}",
                e
            ))
        })?;
        tracing::trace!(
            path = %st_path.display(),
            header_size = header_size_new,
            "safetensors header 已读取（新格式：末尾）"
        );
        return Ok(header_bytes);
    }

    // ---- 策略 2: 旧格式（header 在文件开头） ----
    tracing::debug!(
        path = %st_path.display(),
        new_format_header_size = header_size_new,
        "新格式 header_size 不合理，尝试旧格式（文件开头）"
    );

    file.seek(SeekFrom::Start(0)).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "safetensors seek to start 失败: {}",
            e
        ))
    })?;
    file.read_exact(&mut header_size_buf).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "safetensors header start read 失败: {}",
            e
        ))
    })?;
    let header_size_old = u64::from_le_bytes(header_size_buf) as usize;

    if !is_reasonable(header_size_old) {
        // 读取文件首尾各 64 字节用于诊断
        let mut head_buf = [0u8; 64];
        let mut tail_buf = [0u8; 64];
        let _ = file.seek(SeekFrom::Start(0));
        let head_n = file.read(&mut head_buf).unwrap_or(0);
        let _ = file.seek(SeekFrom::End(-64));
        let tail_n = file.read(&mut tail_buf).unwrap_or(0);

        let hex_dump = |buf: &[u8], n: usize| -> String {
            buf[..n]
                .chunks(16)
                .map(|chunk| {
                    chunk
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join("\n  ")
        };

        return Err(ramaria_core::error::RamariaError::embedding(format!(
            "无法解析 safetensors header（新旧格式均失败）。\n\
             - 新格式（末尾 8 字节）: {} (0x{:016x})\n\
             - 旧格式（开头 8 字节）: {} (0x{:016x})\n\
             文件: {}（{} bytes）\n\
             --- 文件头 {} bytes ---\n  {}\n\
             --- 文件尾 {} bytes ---\n  {}\n\
             提示: 文件可能不是有效的 safetensors 格式，或为 PyTorch bin 格式。\n\
             Python 使用 sentence-transformers 加载（透明支持多格式），Rust 仅支持 safetensors。",
            header_size_new,
            header_size_new,
            header_size_old,
            header_size_old,
            st_path.display(),
            file_len,
            head_n,
            hex_dump(&head_buf, head_n),
            tail_n,
            hex_dump(&tail_buf, tail_n),
        )));
    }

    // 旧格式: header 紧跟在 header_size 之后
    let mut header_bytes = vec![0u8; header_size_old];
    file.read_exact(&mut header_bytes).map_err(|e| {
        ramaria_core::error::RamariaError::embedding(format!(
            "safetensors header read 失败（旧格式）: {}",
            e
        ))
    })?;

    // 额外校验: header 应以 '{' 开头
    if header_bytes.first() != Some(&b'{') {
        let first_byte = header_bytes.first().copied().unwrap_or(0);
        return Err(ramaria_core::error::RamariaError::embedding(format!(
            "safetensors header 首字节异常（旧格式）: 0x{first_byte:02x}，期望 '{{' (0x7b)。\
             该文件可能不是 safetensors 格式。"
        )));
    }

    tracing::trace!(
        path = %st_path.display(),
        header_size = header_size_old,
        "safetensors header 已读取（旧格式：开头）"
    );
    Ok(header_bytes)
}

// =========================================================
// config.json 解析辅助（减少闭包样板）
// =========================================================

/// 从 `serde_json::Value` 中提取 `usize` 字段，缺失时返回默认值。
pub(crate) fn config_usize(raw: &serde_json::Value, key: &str, default: usize) -> usize {
    raw.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default)
}

/// 从 `serde_json::Value` 中提取 `f64` 字段，缺失时返回默认值。
pub(crate) fn config_f64(raw: &serde_json::Value, key: &str, default: f64) -> f64 {
    raw.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}

/// 从 `serde_json::Value` 中提取 `f32` 字段，缺失时返回默认值。
pub(crate) fn config_f32(raw: &serde_json::Value, key: &str, default: f32) -> f32 {
    raw.get(key)
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(default)
}

/// 从 `serde_json::Value` 中提取 `bool` 字段，缺失时返回默认值。
pub(crate) fn config_bool(raw: &serde_json::Value, key: &str, default: bool) -> bool {
    raw.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// 从 `serde_json::Value` 中提取 `u32` 字段（可选）。
pub(crate) fn config_u32_opt(raw: &serde_json::Value, key: &str) -> Option<u32> {
    raw.get(key).and_then(|v| v.as_u64()).map(|v| v as u32)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::Device;

    /// 测试 L2 归一化：单位向量不变
    #[test]
    fn l2_normalize_unit_vector() {
        let device = Device::Cpu;
        let v = Tensor::new(&[1.0f32, 0.0f32, 0.0f32], &device).unwrap();
        let n = l2_normalize(&v, &device).unwrap();
        let result: Vec<f32> = n.to_vec1().unwrap();
        assert!((result[0] - 1.0).abs() < 1e-6);
        assert!(result[1].abs() < 1e-6);
        assert!(result[2].abs() < 1e-6);
    }

    /// 测试 L2 归一化：零向量安全返回零向量
    #[test]
    fn l2_normalize_zero_vector() {
        let device = Device::Cpu;
        let v = Tensor::new(&[0.0f32, 0.0f32, 0.0f32], &device).unwrap();
        let n = l2_normalize(&v, &device).unwrap();
        let result: Vec<f32> = n.to_vec1().unwrap();
        assert!(result.iter().all(|&x| x.abs() < 1e-6));
    }

    /// 测试 config 辅助函数
    #[test]
    fn config_helpers() {
        let raw: serde_json::Value =
            serde_json::from_str(r#"{"hidden_size": 768, "num_layers": 12}"#).unwrap();
        assert_eq!(config_usize(&raw, "hidden_size", 384), 768);
        assert_eq!(config_usize(&raw, "nonexistent", 384), 384);
        assert!(config_bool(&raw, "use_cache", true));
    }
}
