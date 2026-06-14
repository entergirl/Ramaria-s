//! rust/crates/ramaria-llm/src/embedding/native.rs - 原生 safetensors 嵌入 Provider
//!
//! 设计特点:
//! - 取代旧的 ONNX 方案，直接从 HuggingFace safetensors 格式加载嵌入模型
//! - 支持 BERT 架构（bge-small-zh-v1.5，mean pooling）和 LLaMA/Qwen3 架构（last token pooling）
//! - 架构通过 config.json 自动检测，维度在构造时确定（不依赖模型加载）
//! - 惰性加载：首次 `embed()` 调用时才加载模型权重到内存
//! - 线程安全：内部状态通过 `Mutex` 保护，满足 `EmbeddingProvider: Send + Sync`
//! - CPU 密集型推理使用 `tokio::task::block_in_place` 避免阻塞 async 运行时
//! - 超时保护：单条 30s、批量 120s，防止模型加载/推理卡死
//!
//! 模型目录约定:
//! - 需包含: config.json, model.safetensors, tokenizer.json
//! - 加载顺序: 检测架构（构造时）→ 选择编码器 → 加载权重（首次 embed 时）
//!
//! 错误处理:
//! - 所有错误使用 `RamariaError::Embedding` / `Config` 变体，含清晰错误信息和模型路径
//! - 区分错误类型: 文件缺失、格式无效、架构不支持、推理失败、超时

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};

use super::models::{self, ModelArchitecture};

// =========================================================
// 超时常量
// =========================================================

/// 单条嵌入超时（秒）。BGE-small: ~50-200ms，Qwen3: ~200-500ms，30s 对慢速 CPU 足够。
const EMBED_TIMEOUT_SECS: u64 = 30;

/// 批量嵌入超时（秒）。最多 100 条，每条 500ms → 50s，120s 留有充足余量。
const EMBED_BATCH_TIMEOUT_SECS: u64 = 120;

// =========================================================
// 内部编码器枚举
// =========================================================

/// 统一嵌入编码器类型。
///
/// 职责:
/// - 封装不同架构的编码器实现，对外提供统一接口
/// - 通过 enum 而非 trait object 避免虚函数调用开销
enum Encoder {
    Bert(super::models::bert::BertEncoder),
    Llama(super::models::llama::LlamaEncoder),
    LlamaHeadDim(super::models::llama_head_dim::LlamaHeadDimEncoder),
}

impl Encoder {
    fn embed_text(&self, text: &str) -> RamariaResult<Vec<f32>> {
        match self {
            Self::Bert(e) => e.embed_text(text),
            Self::Llama(e) => e.embed_text(text),
            Self::LlamaHeadDim(e) => e.embed_text(text),
        }
    }

    fn embed_batch_texts(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        match self {
            Self::Bert(e) => e.embed_batch_texts(texts),
            Self::Llama(e) => e.embed_batch_texts(texts),
            Self::LlamaHeadDim(e) => e.embed_batch_texts(texts),
        }
    }

    fn dimension(&self) -> usize {
        match self {
            Self::Bert(e) => e.dimension(),
            Self::Llama(e) => e.dimension(),
            Self::LlamaHeadDim(e) => e.dimension(),
        }
    }

    fn architecture(&self) -> ModelArchitecture {
        match self {
            Self::Bert(_) => ModelArchitecture::Bert,
            Self::Llama(_) => ModelArchitecture::Llama,
            Self::LlamaHeadDim(_) => ModelArchitecture::LlamaHeadDim,
        }
    }
}

// =========================================================
// NativeEmbeddingProvider
// =========================================================

/// 原生 safetensors 嵌入模型 Provider。
///
/// 职责:
/// - 实现 `EmbeddingProvider` trait
/// - 惰性加载：构造时仅检测架构和维度（从 config.json），不加载权重
/// - 线程安全：通过 `Mutex<Option<Encoder>>` 保护内部编码器状态
///
/// 字段:
/// - `model_dir`: 模型目录路径
/// - `model_info`: 模型元信息（构造时从 config.json 确定，之后不可变）
/// - `encoder`: 惰性加载的编码器（Mutex 保护，首次 embed() 时初始化）
/// - `progress`: 模型就绪进度（文件齐全时 = 1.0，否则 = 0.0）
///
/// 用法:
/// ```ignore
/// let provider = NativeEmbeddingProvider::new("/path/to/model")?;
/// // model_info() 此时已可用（维度从 config.json 读取）
/// let vec = provider.embed("你好世界").await?;  // 首次调用触发权重加载
/// ```
pub struct NativeEmbeddingProvider {
    /// 模型目录路径
    model_dir: PathBuf,
    /// 模型信息（构造时确定，之后只读）
    model_info: EmbeddingModelInfo,
    /// 惰性加载的编码器
    encoder: Mutex<Option<Encoder>>,
    /// 模型就绪进度
    progress: Mutex<f64>,
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
    /// - 模型权重的实际加载延迟到首次 `embed()` 调用。
    pub fn new(model_dir: impl Into<PathBuf>) -> RamariaResult<Self> {
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
            "NativeEmbeddingProvider 已创建"
        );

        Ok(Self {
            model_dir: dir,
            model_info: info,
            encoder: Mutex::new(None),
            progress: Mutex::new(progress),
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
        let mut guard = self.encoder.lock().unwrap_or_else(|e| e.into_inner());

        if guard.is_some() {
            return Ok(());
        }

        tracing::info!(
            model_dir = %self.model_dir.display(),
            "开始惰性加载嵌入模型权重..."
        );

        // 检测架构
        let (architecture, _dimension) = models::detect_architecture(&self.model_dir)?;

        // 根据架构加载编码器 — 带容错回退
        let encoder = Self::load_encoder(architecture, &self.model_dir)?;

        let actual_dim = encoder.dimension();
        let arch_name = encoder.architecture().name();

        // 更新进度
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) = 1.0;

        *guard = Some(encoder);

        tracing::info!(
            architecture = arch_name,
            dimension = actual_dim,
            "嵌入模型加载完成"
        );

        Ok(())
    }

    /// 加载编码器，如果检测到的架构加载失败，自动回退尝试另一种架构。
    ///
    /// 说明:
    /// - BGE 等模型的 config.json 有时使用非标准 architecture 名，
    ///   导致检测为 LLaMA 但 safetensors 实际是 BERT 格式。
    /// - 此容错机制可处理这类边缘情况。
    fn load_encoder(detected: ModelArchitecture, model_dir: &Path) -> RamariaResult<Encoder> {
        // 先尝试检测到的架构
        let primary_result = Self::try_load(detected, model_dir);
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

                match Self::try_load(fallback, model_dir) {
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
    fn try_load(arch: ModelArchitecture, model_dir: &Path) -> RamariaResult<Encoder> {
        match arch {
            ModelArchitecture::Bert => {
                let bert = super::models::bert::BertEncoder::load(model_dir)?;
                Ok(Encoder::Bert(bert))
            }
            ModelArchitecture::Llama => {
                let llama = super::models::llama::LlamaEncoder::load(model_dir)?;
                Ok(Encoder::Llama(llama))
            }
            ModelArchitecture::LlamaHeadDim => {
                let encoder = super::models::llama_head_dim::LlamaHeadDimEncoder::load(model_dir)?;
                Ok(Encoder::LlamaHeadDim(encoder))
            }
        }
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
// EmbeddingProvider trait 实现
// =========================================================

#[async_trait]
impl EmbeddingProvider for NativeEmbeddingProvider {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        if text.is_empty() {
            return Err(RamariaError::validation("嵌入文本不能为空"));
        }

        self.ensure_loaded()?;

        // 编码器推理是 CPU 密集型操作（50ms-2s/条）。
        // 策略: block_in_place（避免阻塞 tokio worker）+ timeout（防止卡死）。
        // timeout 包裹 async { block_in_place(...) }：async 块使其成为 Future，
        // block_in_place 在首次 poll 时同步执行，完成后 Future 立即就绪。
        let text = text.to_string();
        tokio::time::timeout(std::time::Duration::from_secs(EMBED_TIMEOUT_SECS), async {
            tokio::task::block_in_place(|| {
                let guard = self.encoder.lock().unwrap_or_else(|e| e.into_inner());
                let encoder = guard.as_ref().ok_or_else(|| {
                    RamariaError::embedding("编码器未初始化 — 请先调用 ensure_loaded()")
                })?;
                encoder.embed_text(&text)
            })
        })
        .await
        .map_err(|_elapsed| {
            RamariaError::embedding(format!(
                "单条嵌入超时（{}s）。文本长度: {} 字符。请检查模型是否过大或 CPU 是否过载。",
                EMBED_TIMEOUT_SECS,
                text.chars().count()
            ))
        })?
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_loaded()?;

        let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let count = texts.len();

        tokio::time::timeout(
            std::time::Duration::from_secs(EMBED_BATCH_TIMEOUT_SECS),
            async {
                tokio::task::block_in_place(|| {
                    let guard = self.encoder.lock().unwrap_or_else(|e| e.into_inner());
                    let encoder = guard
                        .as_ref()
                        .ok_or_else(|| RamariaError::embedding("编码器未初始化"))?;
                    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                    encoder.embed_batch_texts(&text_refs)
                })
            },
        )
        .await
        .map_err(|_elapsed| {
            RamariaError::embedding(format!(
                "批量嵌入超时（{}s）。批次大小: {} 条。请检查模型是否过大或 CPU 是否过载。",
                EMBED_BATCH_TIMEOUT_SECS, count
            ))
        })?
    }

    fn model_info(&self) -> &EmbeddingModelInfo {
        // model_info 在构造时确定（从 config.json 读取维度），之后不可变
        // 因此可以直接返回 &self.model_info，无需锁
        &self.model_info
    }

    async fn validate(&self) -> RamariaResult<()> {
        // 验证模型目录存在
        if !self.model_dir.exists() {
            return Err(RamariaError::config(format!(
                "模型目录不存在: {}",
                self.model_dir.display()
            )));
        }

        // 验证必需文件存在
        for file in &["config.json", "model.safetensors", "tokenizer.json"] {
            let path = self.model_dir.join(file);
            if !path.exists() {
                return Err(RamariaError::config(format!(
                    "模型文件缺失: {}",
                    path.display()
                )));
            }
        }

        // 加载模型并执行测试推理
        self.ensure_loaded()?;

        let guard = self.encoder.lock().unwrap_or_else(|e| e.into_inner());
        let encoder = guard
            .as_ref()
            .ok_or_else(|| RamariaError::embedding("编码器未初始化"))?;

        // 用短测试文本验证完整管线
        let test_vec = encoder.embed_text("测试")?;
        if test_vec.is_empty() {
            return Err(RamariaError::embedding("测试向量为空 — 模型可能未正确加载"));
        }

        let expected_dim = self.model_info.dimension;
        if test_vec.len() != expected_dim {
            return Err(RamariaError::embedding(format!(
                "向量维度不匹配: 期望 {}，实际 {}。\n\
                 请检查 config.json 中的 hidden_size 是否与实际模型一致",
                expected_dim,
                test_vec.len()
            )));
        }

        // 验证向量非平凡（不全为零或 NaN）
        let has_nonzero = test_vec.iter().any(|&v| v.abs() > 1e-8);
        if !has_nonzero {
            return Err(RamariaError::embedding(
                "测试向量全为零 — 模型推理可能异常，请检查权重文件",
            ));
        }

        let has_nan = test_vec.iter().any(|v| v.is_nan());
        if has_nan {
            return Err(RamariaError::embedding(
                "测试向量包含 NaN — 模型推理异常，请检查权重文件是否完整",
            ));
        }

        tracing::info!(
            dimension = expected_dim,
            architecture = %encoder.architecture().name(),
            "嵌入模型验证通过"
        );

        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        // safetensors 模型通过 ModelManager 下载，本 provider 仅检查本地文件
        if self.is_available() {
            return Ok(());
        }

        Err(RamariaError::config(format!(
            "模型文件不存在于目录: {}。\n\
             需包含以下文件:\n\
             - config.json（模型配置）\n\
             - model.safetensors（模型权重）\n\
             - tokenizer.json（分词器）\n\n\
             可通过 HuggingFace 下载:\n\
             - bge-small-zh-v1.5: https://huggingface.co/BAAI/bge-small-zh-v1.5\n\
             - Qwen3-Embedding-0.6B: https://huggingface.co/Qwen/Qwen3-Embedding-0.6B\n\n\
             或通过应用内「设置 → 嵌入模型」自动下载。",
            self.model_dir.display()
        )))
    }

    fn download_progress(&self) -> f64 {
        *self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_available(&self) -> bool {
        Self::check_files_exist(&self.model_dir)
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
/// - `NativeEmbeddingProvider` 实例。
pub fn create_native_provider(
    model_dir: impl Into<PathBuf>,
) -> RamariaResult<NativeEmbeddingProvider> {
    NativeEmbeddingProvider::new(model_dir)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 provider 构造（无模型文件）
    #[test]
    fn provider_creation_without_model() {
        let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
        assert!(!provider.is_available());
        assert_eq!(provider.download_progress(), 0.0);
    }

    /// 测试空文本 embed 应报错
    #[tokio::test]
    async fn embed_empty_text_returns_error() {
        let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
        let result = provider.embed("").await;
        assert!(result.is_err());
    }

    /// 测试批量空列表
    #[tokio::test]
    async fn embed_batch_empty_list_returns_empty() {
        let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
        let result = provider.embed_batch(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    /// 测试在无模型目录时 validate 报错
    #[tokio::test]
    async fn validate_without_model_fails() {
        let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
        let result = provider.validate().await;
        assert!(result.is_err());
    }

    /// 测试 download_model 在无模型时报错
    #[tokio::test]
    async fn download_without_model_errors() {
        let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
        let result = provider.download_model().await;
        assert!(result.is_err());
    }

    /// 测试 model_info 在未加载时返回默认维度
    #[test]
    fn model_info_default_dimension() {
        let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
        assert_eq!(provider.model_info().dimension, 384);
    }

    /// 测试进度初始值
    #[test]
    fn progress_starts_at_zero_for_missing_model() {
        let provider = NativeEmbeddingProvider::new("/nonexistent/path").unwrap();
        assert_eq!(provider.download_progress(), 0.0);
    }

    /// 测试 model_info 模型 ID 格式
    #[test]
    fn model_info_id_format() {
        let provider = NativeEmbeddingProvider::new("/test/model/dir").unwrap();
        assert!(provider.model_info().model_id.starts_with("native:"));
    }
}
