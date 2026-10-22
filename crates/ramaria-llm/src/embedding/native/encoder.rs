//! crates/ramaria-llm/src/embedding/native/encoder.rs - 原生嵌入编码器枚举与设备解析
//!
//! 设计特点:
//! - `Encoder` 用 enum 封装三种架构编码器，避免 trait object 的虚调用开销
//! - `load_encoder` 在主架构加载失败时回退另一架构；`LlamaHeadDim` 不回退（head_dim 不兼容）
//! - `resolve_device` 把 `EmbeddingDevice` 解析为 candle 设备，CUDA 不可用时回退 CPU
//! - `sync_actual_dimension` 为纯函数：把加载得到的真实维度写入维度槽

use candle_core::Device;
use ramaria_core::config::EmbeddingDevice;
use ramaria_core::error::RamariaResult;
use std::path::Path;

use super::super::models::{self, ModelArchitecture};

// =========================================================
// 设备解析
// =========================================================

/// 将 `EmbeddingDevice` 配置解析为具体的 candle 计算设备。
///
/// 规则:
/// - `cpu` → `Device::Cpu`。
/// - `cuda` → 尝试 CUDA；不可用（未编译 feature / 无 GPU）回退 CPU 并记 warn。
/// - `auto` → 自动探测：CUDA 可用则用 GPU，否则 CPU。
///
/// 说明:
/// - CUDA 功能需在编译时启用 `cuda` feature（`candle-core/cuda`）；
///   未启用时 `Device::Cuda` 变体不可用，恒回退 CPU。
/// - 设备选择不改变向量语义，仅影响推理性能。
pub(super) fn resolve_device(device: EmbeddingDevice) -> Device {
    match device {
        EmbeddingDevice::Cpu => Device::Cpu,
        EmbeddingDevice::Cuda | EmbeddingDevice::Auto => {
            #[cfg(feature = "cuda")]
            {
                match Device::cuda_if_available(0) {
                    Ok(d) => {
                        tracing::info!(
                            device = %device.as_str(),
                            "嵌入模型使用 CUDA GPU 推理"
                        );
                        d
                    }
                    Err(e) => {
                        tracing::warn!(
                            requested = %device.as_str(),
                            error = %e,
                            "CUDA 设备不可用，嵌入模型回退 CPU 推理"
                        );
                        Device::Cpu
                    }
                }
            }
            #[cfg(not(feature = "cuda"))]
            {
                tracing::warn!(
                    requested = %device.as_str(),
                    "未编译 CUDA 支持（缺少 cuda feature），嵌入模型使用 CPU 推理"
                );
                Device::Cpu
            }
        }
    }
}

// =========================================================
// 内部编码器枚举
// =========================================================

/// 统一嵌入编码器类型。
///
/// 职责:
/// - 封装不同架构的编码器实现，对外提供统一接口
/// - 通过 enum 而非 trait object 避免虚函数调用开销
pub(super) enum Encoder {
    Bert(models::bert::BertEncoder),
    Llama(models::llama::LlamaEncoder),
    LlamaHeadDim(models::llama_head_dim::LlamaHeadDimEncoder),
}

impl Encoder {
    pub(super) fn embed_text(&self, text: &str) -> RamariaResult<Vec<f32>> {
        match self {
            Self::Bert(e) => e.embed_text(text),
            Self::Llama(e) => e.embed_text(text),
            Self::LlamaHeadDim(e) => e.embed_text(text),
        }
    }

    pub(super) fn embed_batch_texts(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        match self {
            Self::Bert(e) => e.embed_batch_texts(texts),
            Self::Llama(e) => e.embed_batch_texts(texts),
            Self::LlamaHeadDim(e) => e.embed_batch_texts(texts),
        }
    }

    pub(super) fn dimension(&self) -> usize {
        match self {
            Self::Bert(e) => e.dimension(),
            Self::Llama(e) => e.dimension(),
            Self::LlamaHeadDim(e) => e.dimension(),
        }
    }

    pub(super) fn architecture(&self) -> ModelArchitecture {
        match self {
            Self::Bert(_) => ModelArchitecture::Bert,
            Self::Llama(_) => ModelArchitecture::Llama,
            Self::LlamaHeadDim(_) => ModelArchitecture::LlamaHeadDim,
        }
    }
}

// =========================================================
// 维度同步纯函数
// =========================================================

/// 把实际加载得到的向量维度同步到存储的模型信息维度槽。
///
/// 用途:
/// - 模型权重加载完成后，若实际维度与构造时占位/猜测维度不同
///   （先构造后下载流程中构造时可能为默认 384），此处修正为真实维度，
///   使 `validate` 的维度一致性校验与实际模型一致。
///
/// 参数:
/// - `stored`: 存储的维度槽（`model_info.dimension` 的可变借用）。
/// - `actual`: 模型加载后得到的真实向量维度。
///
/// 返回:
/// - `true`: 维度发生变更（stored 已更新为 actual）。
/// - `false`: stored 与 actual 相同，未发生变更。
///
/// 说明:
/// - 纯函数：无锁、无 I/O，便于并发/顺序单测覆盖"加载后维度同步"逻辑。
/// - 仅在加载成功拿到 `actual` 后才调用；加载失败不进入此函数，
///   因此失败路径保持旧维度不变。
/// - 返回是否变更，便于调用方决定是否打"维度已同步"日志。
pub(super) fn sync_actual_dimension(stored: &mut usize, actual: usize) -> bool {
    if *stored == actual {
        return false;
    }
    *stored = actual;
    true
}

/// 加载编码器，如果检测到的架构加载失败，自动回退尝试另一种架构。
///
/// 说明:
/// - BGE 等模型的 config.json 有时使用非标准 architecture 名，
///   导致检测为 LLaMA 但 safetensors 实际是 BERT 格式。
/// - 此容错机制可处理这类边缘情况。
/// - `device` 已由调用方解析为具体 candle 设备（CPU 或 CUDA）。
pub(super) fn load_encoder(
    detected: ModelArchitecture,
    model_dir: &Path,
    device: &Device,
) -> RamariaResult<Encoder> {
    // 先尝试检测到的架构
    let primary_result = try_load(detected, model_dir, device);
    match primary_result {
        Ok(encoder) => Ok(encoder),
        Err(primary_err) => {
            // LlamaHeadDim 无法回退到 Llama（head_dim 不兼容，必定 shape mismatch）
            if matches!(detected, ModelArchitecture::LlamaHeadDim) {
                return Err(primary_err);
            }
            // 尝试另一种架构
            let fallback = detected.opposite();
            tracing::warn!(
                detected = %detected.name(),
                error = %primary_err,
                fallback = %fallback.name(),
                "主架构加载失败，尝试回退架构"
            );

            match try_load(fallback, model_dir, device) {
                Ok(encoder) => {
                    tracing::info!(
                        from = %detected.name(),
                        to = %fallback.name(),
                        "回退架构加载成功"
                    );
                    Ok(encoder)
                }
                Err(fallback_err) => {
                    // 两种都失败，返回原始错误（更可能是 config.json 检测的架构）
                    Err(ramaria_core::error::RamariaError::embedding(format!(
                        "模型加载失败（已尝试 {} 和 {} 两种架构）:\n\
                         - {} 错误: {}\n\
                         - {} 错误: {}\n\
                         请检查 config.json 和 model.safetensors 是否来自同一模型。",
                        detected.name(),
                        fallback.name(),
                        detected.name(),
                        primary_err,
                        fallback.name(),
                        fallback_err,
                    )))
                }
            }
        }
    }
}

/// 尝试加载指定架构的编码器。
pub(super) fn try_load(
    arch: ModelArchitecture,
    model_dir: &Path,
    device: &Device,
) -> RamariaResult<Encoder> {
    match arch {
        ModelArchitecture::Bert => {
            let bert = models::bert::BertEncoder::load(model_dir, device)?;
            Ok(Encoder::Bert(bert))
        }
        ModelArchitecture::Llama => {
            let llama = models::llama::LlamaEncoder::load(model_dir, device)?;
            Ok(Encoder::Llama(llama))
        }
        ModelArchitecture::LlamaHeadDim => {
            let encoder = models::llama_head_dim::LlamaHeadDimEncoder::load(model_dir, device)?;
            Ok(Encoder::LlamaHeadDim(encoder))
        }
    }
}
