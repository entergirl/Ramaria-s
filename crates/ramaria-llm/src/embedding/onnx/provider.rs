//! crates/ramaria-llm/src/embedding/onnx/provider.rs - ONNX 嵌入 Provider 实现
//!
//! 设计特点:
//! - 实现 `EmbeddingProvider` trait，提供 ONNX 推理能力
//! - 惰性加载模型（首次 `embed` 调用时才加载 ONNX 模型到内存）
//! - 线程安全：内部状态通过 `Mutex` 保护；`model_info` 构造时确定后不可变
//! - 已知局限：模型实际维度与 config.json 不一致时仅 warn，不回写 `model_info`

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};

use super::session::{MODEL_FILE, OnnxSession, TOKENIZER_FILE};

// =========================================================
// OnnxEmbeddingProvider
// =========================================================

/// ONNX 嵌入模型 Provider。
///
/// 职责:
/// - 实现 `EmbeddingProvider` trait，提供 ONNX 推理能力
/// - 惰性加载模型（首次 `embed` 调用时才加载 ONNX 模型到内存）
/// - 线程安全：内部状态通过 `Mutex` 保护；`model_info` 构造时确定后不可变
///
/// 用法:
/// 需本机 ONNX 模型目录（`/path/to/bge-model` 为占位路径），示例仅示意，不参与编译。
/// ```ignore
/// let provider = OnnxEmbeddingProvider::new("/path/to/bge-model")?;
/// if provider.is_available() {
/// let vec = provider.embed("你好世界").await?;
/// }
/// ```
pub struct OnnxEmbeddingProvider {
    /// 模型目录路径
    model_dir: PathBuf,
    /// 模型信息（构造时从 config.json 读取维度，之后不可变——无数据竞争）
    model_info: EmbeddingModelInfo,
    /// 惰性加载的 ONNX 会话
    session: Mutex<Option<OnnxSession>>,
    /// 下载进度（当前版本从本地加载，进度始终为 1.0）
    progress: Mutex<f64>,
}

impl OnnxEmbeddingProvider {
    /// 创建新的 ONNX 嵌入 provider。
    ///
    /// 参数:
    /// - `model_dir`: 模型目录路径，应包含 model.onnx 和 tokenizer.json。
    ///
    /// 返回:
    /// - 成功时返回 provider 实例（模型尚未加载，首次调用 embed 时加载）。
    ///
    /// 说明:
    /// - 构造时尝试从 config.json 读取 `hidden_size` 确定维度；若无则默认 384。
    /// - `model_info` 构造后不可变（无数据竞争）。
    /// - 模型是否存在通过 `is_available` 检查（检查文件是否存在）。
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        let dir = model_dir.into();
        let model_exists = dir.join(MODEL_FILE).exists() && dir.join(TOKENIZER_FILE).exists();

        // 构造时确定维度：从 config.json 读取（如有），否则默认 384
        let dimension = Self::read_dimension_from_config(&dir).unwrap_or(384);

        let info = EmbeddingModelInfo {
            model_id: format!("onnx:{}", dir.display()),
            dimension,
        };

        tracing::info!(
            model_dir = %dir.display(),
            model_exists,
            dimension,
            "OnnxEmbeddingProvider 已创建"
        );

        Self {
            model_dir: dir,
            model_info: info,
            session: Mutex::new(None),
            progress: Mutex::new(if model_exists { 1.0 } else { 0.0 }),
        }
    }

    /// 从 config.json 读取 `hidden_size` 作为向量维度。
    ///
    /// 说明:
    /// - 仅用于构造时确定 `model_info.dimension`。
    /// - 读取失败（文件缺失、JSON 无效、字段缺失）返回 None，由调用方使用默认值。
    fn read_dimension_from_config(dir: &Path) -> Option<usize> {
        let config_path = dir.join("config.json");
        if !config_path.exists() {
            return None;
        }

        let file = std::fs::File::open(&config_path).ok()?;
        let raw: serde_json::Value = serde_json::from_reader(file).ok()?;
        raw.get("hidden_size")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
    }

    /// 确保 ONNX 会话已加载（惰性初始化）。
    ///
    /// 说明:
    /// - 首次调用时加载模型，后续调用直接返回已缓存的会话。
    /// - 加载失败时返回错误，不缓存失败状态（下次调用会重试）。
    /// - **不再修改 `self.model_info`**——维度已在构造时从 config.json 确定。
    fn ensure_loaded(&self) -> RamariaResult<()> {
        let mut guard = lock_recover(&self.session, "embedding_onnx.session");

        if guard.is_some() {
            return Ok(());
        }

        tracing::info!(model_dir = %self.model_dir.display(), "开始加载 ONNX 嵌入模型...");

        let session = OnnxSession::load(&self.model_dir)?;

        // 验证实际维度与构造时检测的维度一致
        let actual_dim = session.dimension;
        let expected_dim = self.model_info.dimension;
        if actual_dim != expected_dim {
            tracing::warn!(
                actual = actual_dim,
                expected = expected_dim,
                "ONNX 模型实际维度与 config.json 不一致，以实际维度为准"
            );
            // 注意：这里不修改 self.model_info（保持构造时不可变语义），
            // 后续 validate 会检测维度不匹配并报错。
        }

        *guard = Some(session);

        // 更新进度
        *lock_recover(&self.progress, "embedding_onnx.progress") = 1.0;

        tracing::info!(dimension = actual_dim, "ONNX 嵌入模型加载完成");
        Ok(())
    }
}

#[async_trait]
impl EmbeddingProvider for OnnxEmbeddingProvider {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        if text.is_empty() {
            return Err(RamariaError::validation("嵌入文本不能为空"));
        }

        self.ensure_loaded()?;

        // ONNX 推理是 CPU 密集型操作（50-200ms/条），
        // 使用 block_in_place 将当前任务移出 tokio 工作线程，
        // 避免阻塞同线程上的其他异步任务（如流式 LLM 响应处理）。
        let text = text.to_string();
        tokio::task::block_in_place(|| {
            let guard = lock_recover(&self.session, "embedding_onnx.session");
            let session = guard
                .as_ref()
                .ok_or_else(|| RamariaError::validation("ONNX 会话未初始化"))?;
            session.embed_text(&text)
        })
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_loaded()?;

        // 将所有文本 clone 为自有 String（block_in_place 闭包要求 'static 或自有数据）
        let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();

        // 批量 ONNX 推理同样使用 block_in_place 避免阻塞 tokio 工作线程
        tokio::task::block_in_place(|| {
            let guard = lock_recover(&self.session, "embedding_onnx.session");
            let session = guard
                .as_ref()
                .ok_or_else(|| RamariaError::validation("ONNX 会话未初始化"))?;
            let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            session.embed_batch_texts(&text_refs)
        })
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        self.model_info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        // 验证模型目录存在
        if !self.model_dir.exists() {
            return Err(RamariaError::config(format!(
                "模型目录不存在: {}",
                self.model_dir.display()
            )));
        }

        // 验证模型文件存在
        let model_path = self.model_dir.join(MODEL_FILE);
        if !model_path.exists() {
            return Err(RamariaError::config(format!(
                "ONNX 模型文件缺失: {}",
                model_path.display()
            )));
        }

        let tokenizer_path = self.model_dir.join(TOKENIZER_FILE);
        if !tokenizer_path.exists() {
            return Err(RamariaError::config(format!(
                "分词器文件缺失: {}",
                tokenizer_path.display()
            )));
        }

        // 加载并执行测试推理
        self.ensure_loaded()?;

        let guard = lock_recover(&self.session, "embedding_onnx.session");
        let session = guard
            .as_ref()
            .ok_or_else(|| RamariaError::validation("ONNX 会话未初始化"))?;

        // 用短测试文本验证 pipeline
        let test_vec = session.embed_text("测试")?;
        if test_vec.is_empty() {
            return Err(RamariaError::validation("测试向量为空"));
        }

        let expected_dim = self.model_info.dimension;
        if test_vec.len() != expected_dim {
            return Err(RamariaError::validation(format!(
                "向量维度不匹配: 期望 {}，实际 {}",
                expected_dim,
                test_vec.len()
            )));
        }

        tracing::info!(dimension = expected_dim, "ONNX 嵌入模型验证通过");

        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        // ONNX 模型从本地目录加载，不需要下载
        // 如果用户需要下载，通过 ModelManager 完成
        if self.is_available() {
            return Ok(());
        }

        Err(RamariaError::config(format!(
            "模型文件不存在于目录: {}。请将 model.onnx 和 tokenizer.json 放入此目录",
            self.model_dir.display()
        )))
    }

    fn download_progress(&self) -> f64 {
        *lock_recover(&self.progress, "embedding_onnx.progress")
    }

    fn is_available(&self) -> bool {
        // 检查模型文件是否存在
        self.model_dir.join(MODEL_FILE).exists() && self.model_dir.join(TOKENIZER_FILE).exists()
    }
}

// =========================================================
// 工厂函数
// =========================================================

/// 创建 ONNX 嵌入 provider 的便捷工厂。
///
/// 参数:
/// - `model_dir`: 模型目录路径。
///
/// 返回:
/// - OnnxEmbeddingProvider 实例。
pub fn create_onnx_provider(model_dir: impl Into<PathBuf>) -> OnnxEmbeddingProvider {
    OnnxEmbeddingProvider::new(model_dir)
}
