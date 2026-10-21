//! crates/ramaria-testkit/src/mock/embedding.rs - Ramaria 测试嵌入 mock 模块
//!
//! 设计特点:
//! - 零向量占位：固定 128 维，供检索链路的确定性装配
//! - 确定性向量：字符哈希分桶后 L2 归一化，同文本恒得同向量
//! - 计数口径：单条 / 批量调用次数与每批条数可查询，支持批量失败注入
//! - 不加载模型、不联网、不触碰文件系统

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};

// =========================================================
// 零向量占位
// =========================================================

/// 零向量占位嵌入（固定 128 维，不上真实模型）。
pub struct MockEmbedding {
    model_info: EmbeddingModelInfo,
}

impl Default for MockEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

impl MockEmbedding {
    /// 构造 128 维零向量嵌入。
    pub fn new() -> Self {
        Self {
            model_info: EmbeddingModelInfo {
                model_id: "mock-embedding".to_string(),
                dimension: 128,
            },
        }
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbedding {
    async fn embed(&self, _text: &str) -> RamariaResult<Vec<f32>> {
        Ok(vec![0.0; 128])
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; 128]).collect())
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        self.model_info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
    }
}

// =========================================================
// 确定性嵌入
// =========================================================

/// 确定性嵌入：字符哈希分桶后 L2 归一化（无模型、无网络、无随机性）。
pub struct DeterministicEmbedding {
    info: EmbeddingModelInfo,
}

impl DeterministicEmbedding {
    /// 向量维度（足够区分测试文本即可）。
    pub const DIMENSION: usize = 16;

    /// 构造可用的嵌入 provider。
    pub fn new() -> Self {
        Self {
            info: EmbeddingModelInfo {
                model_id: "mock-deterministic-embedding".to_string(),
                dimension: Self::DIMENSION,
            },
        }
    }

    /// 计算确定性向量（字符哈希分桶后 L2 归一化）。
    pub fn vector(text: &str) -> Vec<f32> {
        use std::hash::{Hash, Hasher};

        let mut vector = vec![0.0_f32; Self::DIMENSION];
        for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            ch.hash(&mut hasher);
            let index = (hasher.finish() as usize) % Self::DIMENSION;
            vector[index] += 1.0;
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector
    }
}

impl Default for DeterministicEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbeddingProvider for DeterministicEmbedding {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        Ok(Self::vector(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| Self::vector(text)).collect())
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        self.info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
    }
}

// =========================================================
// 计数嵌入
// =========================================================

/// 计数嵌入：确定性向量 + 调用计数 / 批大小记录 / 批量失败注入。
pub struct CountingEmbedding {
    info: EmbeddingModelInfo,
    embed_calls: AtomicUsize,
    batch_calls: AtomicUsize,
    batch_sizes: Mutex<Vec<usize>>,
    fail_batch: AtomicBool,
}

impl CountingEmbedding {
    /// 模型标识（指纹为 `mock-counting-embedding@16#64`）。
    pub const MODEL_ID: &'static str = "mock-counting-embedding";
    /// 向量维度。
    pub const DIMENSION: usize = DeterministicEmbedding::DIMENSION;

    /// 构造可用且默认批量成功的计数 mock。
    pub fn new() -> Self {
        Self {
            info: EmbeddingModelInfo {
                model_id: Self::MODEL_ID.to_string(),
                dimension: Self::DIMENSION,
            },
            embed_calls: AtomicUsize::new(0),
            batch_calls: AtomicUsize::new(0),
            batch_sizes: Mutex::new(Vec::new()),
            fail_batch: AtomicBool::new(false),
        }
    }

    /// 计算确定性向量（与单条 / 批量嵌入同一算法）。
    pub fn vector(text: &str) -> Vec<f32> {
        DeterministicEmbedding::vector(text)
    }

    /// 模型指纹（与 `model_fingerprint` 同口径）。
    pub fn fingerprint() -> String {
        format!(
            "{}@{}#{}",
            Self::MODEL_ID,
            Self::DIMENSION,
            Self::DIMENSION * 4
        )
    }

    /// 单条嵌入调用次数。
    pub fn embed_calls(&self) -> usize {
        self.embed_calls.load(Ordering::Acquire)
    }

    /// 批量嵌入调用次数。
    pub fn batch_calls(&self) -> usize {
        self.batch_calls.load(Ordering::Acquire)
    }

    /// 每批实际条数（按调用顺序）。
    pub fn batch_sizes(&self) -> Vec<usize> {
        self.batch_sizes.lock().expect("批大小锁").clone()
    }

    /// 切换批量嵌入失败（true = `embed_batch` 返回错误，逐条 `embed` 仍成功）。
    pub fn set_fail_batch(&self, fail: bool) {
        self.fail_batch.store(fail, Ordering::Release);
    }
}

impl Default for CountingEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbeddingProvider for CountingEmbedding {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        self.embed_calls.fetch_add(1, Ordering::AcqRel);
        Ok(DeterministicEmbedding::vector(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        self.batch_calls.fetch_add(1, Ordering::AcqRel);
        self.batch_sizes.lock().expect("批大小锁").push(texts.len());
        if self.fail_batch.load(Ordering::Acquire) {
            return Err(RamariaError::validation("批量嵌入失败（测试注入）"));
        }
        Ok(texts
            .iter()
            .map(|text| DeterministicEmbedding::vector(text))
            .collect())
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        self.info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
    }
}
