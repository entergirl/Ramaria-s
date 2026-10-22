//! crates/ramaria-llm/src/embedding/native.rs - 原生 safetensors 嵌入 Provider
//!
//! 设计特点:
//! - 取代旧的 ONNX 方案，直接从 HuggingFace safetensors 格式加载嵌入模型
//! - 支持 BERT 架构（bge-small-zh-v1.5，mean pooling）和 LLaMA/Qwen3 架构（last token pooling）
//! - 架构通过 config.json 自动检测；维度先由构造时探测，模型加载成功后把实际维度同步回 model_info
//! - 惰性加载：首次 `embed` 调用时才加载模型权重到内存
//! - 线程安全：编码器通过 `Arc<Encoder>` 在 `Mutex` 内保护，推理时克隆 Arc 后立即释放锁
//! - P-1 修复：推理操作在锁外执行（先 clone Arc → 释放锁 → 推理），不阻塞其他请求
//! - CPU 密集型推理使用 `tokio::task::block_in_place` 避免阻塞 async 运行时
//! - 超时保护：单条 30s、批量 120s，防止模型加载/推理卡死
//! - 设备选择：由调用方传入 `EmbeddingDevice`（cpu/cuda/auto），CUDA 不可用时静默回退 CPU
//!
//! 模型目录约定:
//! - 需包含: config.json, model.safetensors, tokenizer.json
//! - 加载顺序: 检测架构（构造时）→ 选择编码器 → 加载权重（首次 embed 时）
//!
//! 错误处理:
//! - 所有错误使用 `RamariaError::Embedding` / `Config` 变体，含清晰错误信息和模型路径
//! - 区分错误类型: 文件缺失、格式无效、架构不支持、推理失败、超时

mod encoder;
mod provider;

use encoder::{Encoder, load_encoder, resolve_device, sync_actual_dimension};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use ramaria_core::config::EmbeddingDevice;
use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::EmbeddingModelInfo;

use super::models;

// =========================================================
// NativeEmbeddingProvider
// =========================================================

/// 原生 safetensors 嵌入模型 Provider。
///
/// 职责:
/// - 实现 `EmbeddingProvider` trait
/// - 惰性加载：构造时仅检测架构和维度（从 config.json），不加载权重
/// - 线程安全：通过 `Mutex<Option<Arc<Encoder>>>` 保护内部编码器状态，推理时克隆 Arc 后释放锁
///
/// 字段:
/// - `model_dir`: 模型目录路径
/// - `model_info`: 模型元信息（构造时从 config.json 确定，之后不可变）
/// - `encoder`: 惰性加载的编码器（Arc 包裹，推理时 clone Arc 后释放锁，不阻塞其他请求）
/// - `progress`: 模型就绪进度（文件齐全时 = 1.0，否则 = 0.0）
/// - `device`: 计算设备选择（cpu/cuda/auto；cuda 不可用时回退 cpu）
///
/// 用法:
/// 需本机 safetensors 模型目录（`/path/to/model` 为占位路径），示例仅示意，不参与编译。
/// ```ignore
/// let provider = NativeEmbeddingProvider::new("/path/to/model")?;
/// // model_info 此时已可用（维度从 config.json 读取）
/// let vec = provider.embed("你好世界").await?; // 首次调用触发权重加载
/// ```
pub struct NativeEmbeddingProvider {
    /// 模型目录路径
    model_dir: PathBuf,
    /// 模型信息（构造时确定 model_id；dimension 在模型加载后同步为实际值）
    ///
    /// 线程安全说明:
    /// - 构造时模型文件不存在（先构造后下载流程）→ dimension 为默认占位 384；
    /// - `ensure_loaded` 加载成功后把实际加载维度写回本字段（维度同步），
    ///   避免 validate 报"维度不匹配"；
    /// - 用 `Mutex` 保护读写：`model_info()` 按值返回（trait 签名），无并发数据竞争。
    model_info: Mutex<EmbeddingModelInfo>,
    /// 惰性加载的编码器（Arc 包裹，推理时 clone Arc 后释放锁，不阻塞其他请求）
    encoder: Mutex<Option<Arc<Encoder>>>,
    /// 模型就绪进度
    progress: Mutex<f64>,
    /// 计算设备选择（cpu/cuda/auto）；CUDA 不可用时静默回退 CPU。
    device: EmbeddingDevice,
}

impl NativeEmbeddingProvider {
    /// 创建新的原生嵌入 provider。
    ///
    /// 参数:
    /// - `model_dir`: 模型目录路径，需包含 config.json、model.safetensors、tokenizer.json。
    ///
    /// 返回:
    /// - 成功时返回 provider 实例（模型权重尚未加载到内存）。
    ///
    /// 说明:
    /// - 构造时读取 config.json 检测架构和维度。
    /// - 如果 config.json 不可用（模型未下载），使用默认维度 384。
    /// - 模型权重的实际加载延迟到首次 `embed` 调用。
    /// - 计算设备默认使用自动探测（`EmbeddingDevice::Auto`）。
    pub fn new(model_dir: impl Into<PathBuf>) -> RamariaResult<Self> {
        Self::with_device(model_dir, EmbeddingDevice::Auto)
    }

    /// 创建 provider 并指定计算设备。
    ///
    /// 参数:
    /// - `model_dir`: 模型目录路径。
    /// - `device`: 计算设备选择（cpu/cuda/auto）。
    ///
    /// 说明:
    /// - 设备选择在首次加载权重时解析为具体 `candle Device`；
    ///   CUDA 不可用时静默回退 CPU（回归红线：静默降级）。
    pub fn with_device(
        model_dir: impl Into<PathBuf>,
        device: EmbeddingDevice,
    ) -> RamariaResult<Self> {
        let dir: PathBuf = model_dir.into();
        let model_exists = Self::check_files_exist(&dir);

        // 从 config.json 检测维度（若可用）
        let (dimension, _arch_name) = if model_exists {
            models::detect_architecture(&dir)
                .map(|(arch, dim)| {
                    let name = arch.name().to_string();
                    tracing::info!(
                        model_dir = %dir.display(),
                        architecture = %arch.name(),
                        dimension = dim,
                        "嵌入模型架构已检测"
                    );
                    (dim, name)
                })
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        model_dir = %dir.display(),
                        error = %e,
                        "无法检测模型架构，使用默认维度 384"
                    );
                    (384, "unknown".to_string())
                })
        } else {
            tracing::info!(
                model_dir = %dir.display(),
                "模型文件不存在，等待下载（默认维度 384）"
            );
            (384, "pending".to_string())
        };

        let info = EmbeddingModelInfo {
            model_id: format!("native:{}", dir.display()),
            dimension,
        };

        let progress = if model_exists { 1.0 } else { 0.0 };

        tracing::info!(
            model_dir = %dir.display(),
            model_exists,
            dimension,
            progress,
            device = device.as_str(),
            "NativeEmbeddingProvider 已创建"
        );

        Ok(Self {
            model_dir: dir,
            model_info: Mutex::new(info),
            encoder: Mutex::new(None),
            progress: Mutex::new(progress),
            device,
        })
    }

    /// 确保编码器已加载（惰性初始化）。
    ///
    /// 首次调用时:
    /// 1. 检测模型架构（从 config.json）
    /// 2. 根据架构选择编码器（BERT 或 LLaMA）
    /// 3. 加载 safetensors 权重和分词器
    ///
    /// 后续调用直接返回已缓存的编码器。
    fn ensure_loaded(&self) -> RamariaResult<()> {
        let mut guard = lock_recover(&self.encoder, "embedding_native.encoder");

        if guard.is_some() {
            return Ok(());
        }

        tracing::info!(
            model_dir = %self.model_dir.display(),
            "开始惰性加载嵌入模型权重..."
        );

        // 检测架构
        let (architecture, _dimension) = models::detect_architecture(&self.model_dir)?;

        // 解析计算设备（cuda/auto 在 CUDA 不可用时回退 CPU，静默降级）
        let device = resolve_device(self.device);

        // 根据架构加载编码器 — 带容错回退
        let encoder = load_encoder(architecture, &self.model_dir, &device)?;

        let actual_dim = encoder.dimension();
        let arch_name = encoder.architecture().name();

        // 维度同步：保存实际加载维度，覆盖构造时的占位/猜测值。
        // 先构造后下载流程中构造时维度可能为默认 384，此处更新为真实维度，
        // 使 validate 的"维度不匹配"校验与实际模型一致。
        // 同步动作收敛到纯函数 sync_actual_dimension，便于独立单测。
        // 锁顺序：encoder 锁 → model_info 锁（model_info() 只持 model_info 锁，无反向嵌套，不死锁）。
        {
            let mut info = lock_recover(&self.model_info, "embedding_native.model_info");
            let prev_dim = info.dimension;
            if sync_actual_dimension(&mut info.dimension, actual_dim) {
                tracing::info!(
                    from = prev_dim,
                    to = info.dimension,
                    "嵌入模型维度已同步为实际加载值"
                );
            }
        }

        // 更新进度
        *lock_recover(&self.progress, "embedding_native.progress") = 1.0;

        *guard = Some(Arc::new(encoder));

        tracing::info!(
            architecture = arch_name,
            dimension = actual_dim,
            "嵌入模型加载完成"
        );

        Ok(())
    }

    /// 检查模型必需文件是否存在。
    ///
    /// 必需文件: config.json, model.safetensors, tokenizer.json
    fn check_files_exist(dir: &Path) -> bool {
        dir.join("config.json").exists()
            && dir.join("model.safetensors").exists()
            && dir.join("tokenizer.json").exists()
    }
}
// =========================================================
// 工厂函数
// =========================================================

/// 创建原生 safetensors 嵌入 provider 的便捷工厂。
///
/// 参数:
/// - `model_dir`: 模型目录路径。
///
/// 返回:
/// - `NativeEmbeddingProvider` 实例（计算设备默认自动探测）。
pub fn create_native_provider(
    model_dir: impl Into<PathBuf>,
) -> RamariaResult<NativeEmbeddingProvider> {
    NativeEmbeddingProvider::new(model_dir)
}

/// 创建原生 safetensors 嵌入 provider 并指定计算设备。
///
/// 参数:
/// - `model_dir`: 模型目录路径。
/// - `device`: 计算设备选择（cpu/cuda/auto）。
///
/// 返回:
/// - `NativeEmbeddingProvider` 实例。
pub fn create_native_provider_with_device(
    model_dir: impl Into<PathBuf>,
    device: EmbeddingDevice,
) -> RamariaResult<NativeEmbeddingProvider> {
    NativeEmbeddingProvider::with_device(model_dir, device)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
