//! crates/ramaria-service/src/test_support/embedding.rs - Ramaria 服务层测试用嵌入 mock 模块
//!
//! 设计特点:
//! - 确定性向量：无模型文件、无网络、无随机性；
//! - 让"嵌入模型已加载"的路径在无模型环境下可测（热更新 / 读取 / 可用性判定）；
//! - 固定维度，供维度断言复用；
//! - 单条与批量嵌入共用同一确定性算法，结果可复现。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ramaria_core::error::RamariaResult;

// =========================================================
// 最小嵌入 mock
// =========================================================

/// 最小嵌入 mock：确定性向量（无模型文件、无网络、无随机性）。
///
/// 职责:
/// - 让"嵌入模型已加载"的路径在无模型环境下可测（热更新 / 读取 / 可用性判定）；
/// - 提供固定维度，供维度断言复用。
pub(crate) struct DeterministicEmbedding {
    info: ramaria_core::traits::EmbeddingModelInfo,
}

impl DeterministicEmbedding {
    /// 向量维度（足够区分测试文本即可）。
    pub(crate) const DIMENSION: usize = 16;

    /// 构造可用的嵌入 provider。
    pub(crate) fn new() -> Self {
        Self {
            info: ramaria_core::traits::EmbeddingModelInfo {
                model_id: "mock-deterministic-embedding".to_string(),
                dimension: Self::DIMENSION,
            },
        }
    }

    /// 计算确定性向量（字符哈希分桶后 L2 归一化）。
    pub(crate) fn vector(text: &str) -> Vec<f32> {
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

#[async_trait::async_trait]
impl ramaria_core::traits::EmbeddingProvider for DeterministicEmbedding {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        Ok(Self::vector(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| Self::vector(text)).collect())
    }

    fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
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
// 计数嵌入 mock（调用次数与批大小断言）
// =========================================================

/// 计数嵌入 mock：确定性向量 + 调用计数 / 批大小记录 / 批量失败注入。
///
/// 职责:
/// - 断言向量通道门禁（关闭时调用次数为 0）；
/// - 断言补算批大小严格不超过配置值；
/// - 注入批量失败验证逐条降级路径。
pub(crate) struct CountingEmbedding {
    info: ramaria_core::traits::EmbeddingModelInfo,
    embed_calls: AtomicUsize,
    batch_calls: AtomicUsize,
    batch_sizes: Mutex<Vec<usize>>,
    fail_batch: AtomicBool,
}

impl CountingEmbedding {
    /// 模型标识（指纹为 `mock-counting-embedding@16#64`）。
    pub(crate) const MODEL_ID: &'static str = "mock-counting-embedding";
    /// 向量维度。
    pub(crate) const DIMENSION: usize = DeterministicEmbedding::DIMENSION;

    /// 构造可用且默认批量成功的计数 mock。
    pub(crate) fn new() -> Self {
        Self {
            info: ramaria_core::traits::EmbeddingModelInfo {
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
    pub(crate) fn vector(text: &str) -> Vec<f32> {
        DeterministicEmbedding::vector(text)
    }

    /// 模型指纹（与 `model_fingerprint` 同口径）。
    pub(crate) fn fingerprint() -> String {
        format!(
            "{}@{}#{}",
            Self::MODEL_ID,
            Self::DIMENSION,
            Self::DIMENSION * 4
        )
    }

    /// 单条嵌入调用次数。
    pub(crate) fn embed_calls(&self) -> usize {
        self.embed_calls.load(Ordering::Acquire)
    }

    /// 批量嵌入调用次数。
    pub(crate) fn batch_calls(&self) -> usize {
        self.batch_calls.load(Ordering::Acquire)
    }

    /// 每批实际条数（按调用顺序）。
    pub(crate) fn batch_sizes(&self) -> Vec<usize> {
        self.batch_sizes.lock().expect("批大小锁").clone()
    }

    /// 切换批量嵌入失败（true = `embed_batch` 返回错误，逐条 `embed` 仍成功）。
    pub(crate) fn set_fail_batch(&self, fail: bool) {
        self.fail_batch.store(fail, Ordering::Release);
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::EmbeddingProvider for CountingEmbedding {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        self.embed_calls.fetch_add(1, Ordering::AcqRel);
        Ok(DeterministicEmbedding::vector(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        self.batch_calls.fetch_add(1, Ordering::AcqRel);
        self.batch_sizes.lock().expect("批大小锁").push(texts.len());
        if self.fail_batch.load(Ordering::Acquire) {
            return Err(ramaria_core::error::RamariaError::validation(
                "批量嵌入失败（测试注入）",
            ));
        }
        Ok(texts
            .iter()
            .map(|text| DeterministicEmbedding::vector(text))
            .collect())
    }

    fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
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
