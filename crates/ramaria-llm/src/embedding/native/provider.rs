//! crates/ramaria-llm/src/embedding/native/provider.rs - 原生嵌入 Provider 的 trait 实现
//!
//! 设计特点:
//! - `embed` / `embed_batch` 推理在锁外执行（先 clone `Arc` 再释放锁），CPU 密集计算走 `block_in_place`
//! - 超时保护：单条 30s、批量 120s，防止模型加载/推理卡死
//! - `validate` 做一次短文本推理，校验维度、非全零与无 NaN

use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};

use super::NativeEmbeddingProvider;

// =========================================================
// 超时常量
// =========================================================

/// 单条嵌入超时（秒）。BGE-small: ~50-200ms，Qwen3: ~200-500ms，30s 对慢速 CPU 足够。
const EMBED_TIMEOUT_SECS: u64 = 30;

/// 批量嵌入超时（秒）。最多 100 条，每条 500ms → 50s，120s 留有充足余量。
const EMBED_BATCH_TIMEOUT_SECS: u64 = 120;

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

        // P-1 修复：先克隆 Arc 再释放锁，推理在锁外执行。
        // 编码器推理是 CPU 密集型操作（50ms-2s/条），持锁期间会串行化所有请求。
        // 修复后：锁仅保护 Arc 的读取（<1μs），推理不持锁。
        let encoder = {
            let guard = lock_recover(&self.encoder, "embedding_native.encoder");
            guard
                .as_ref()
                .ok_or_else(|| RamariaError::embedding("编码器未初始化 — 请先调用 ensure_loaded()"))
                .cloned() // 轻量 Arc clone（仅增加引用计数）
        }?;

        // 推理在锁外执行，其他请求可同时读取模型
        let text = text.to_string();
        tokio::time::timeout(std::time::Duration::from_secs(EMBED_TIMEOUT_SECS), async {
            tokio::task::block_in_place(|| encoder.embed_text(&text))
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

        // P-1 修复：先克隆 Arc 再释放锁，推理在锁外执行
        let encoder = {
            let guard = lock_recover(&self.encoder, "embedding_native.encoder");
            guard
                .as_ref()
                .ok_or_else(|| RamariaError::embedding("编码器未初始化"))
                .cloned()
        }?;

        let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let count = texts.len();

        // 推理在锁外执行，大规模批次（最多 100 条，~50s）不阻塞其他请求
        tokio::time::timeout(
            std::time::Duration::from_secs(EMBED_BATCH_TIMEOUT_SECS),
            async {
                tokio::task::block_in_place(|| {
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

    fn model_info(&self) -> EmbeddingModelInfo {
        // 按值返回（trait 签名）：从 Mutex 内 clone，避免返回引用带来的并发竞争。
        // dimension 在 ensure_loaded 后为实际加载维度（构造时可能为占位 384）。
        lock_recover(&self.model_info, "embedding_native.model_info").clone()
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

        // P-1 修复：先克隆 Arc 再释放锁，测试推理在锁外执行
        let encoder = {
            let guard = lock_recover(&self.encoder, "embedding_native.encoder");
            guard
                .as_ref()
                .ok_or_else(|| RamariaError::embedding("编码器未初始化"))
                .cloned()
        }?;

        // 用短测试文本验证完整管线（不持锁）
        let test_vec = encoder.embed_text("测试")?;
        if test_vec.is_empty() {
            return Err(RamariaError::embedding("测试向量为空 — 模型可能未正确加载"));
        }

        let expected_dim = self.model_info().dimension;
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
        *lock_recover(&self.progress, "embedding_native.progress")
    }

    fn is_available(&self) -> bool {
        Self::check_files_exist(&self.model_dir)
    }
}
