//! crates/ramaria-service/tests/parity/support/mocks.rs - 确定性 mock 依赖（LLM / 嵌入）
//!
//! 设计特点:
//! - 完全离线：不发起网络调用、不加载真实模型，CI 与本地行为一致
//! - 脚本化 LLM：按调用次序返回预设回复（用尽后回落到固定回复），记录每次请求供
//!   "Prompt 装配结构"断言（只取长度与段信息，不落全文，遵守隐私红线）
//! - 预计算向量嵌入：文本 → 确定性向量（字符 unigram + 相邻二元组哈希到固定维度并归一化），
//!   相同文本恒等、共享字词的文本余弦相似度更高，足以驱动向量通道的确定性检索
//! - 计数可观测：调用次数可查询，用于断言"某通道确实启用 / 未启用"
//! - 失败可编程：`failing` 口径模拟 LLM 后端不可用，覆盖降级与"不落半条"路径
//! - 仅测试基建使用，不进入生产代码路径

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::{Stream, stream};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{
    ChatRequest, EmbeddingModelInfo, EmbeddingProvider, LlmProvider, StreamDelta,
};
use ramaria_core::types::{BackendConfig, ModelCapability};

// =========================================================
// 脚本化 LLM
// =========================================================

/// 脚本化 LLM：按调用次序返回预设回复。
///
/// 职责:
/// - 为非流式用例（封存 L1 摘要 / chat_send）提供固定、可预期的回复内容；
/// - 记录全部 `ChatRequest`，供快照提取 Prompt 结构指标（长度 / 段数 / 模板版本）。
///
/// 字段约定:
/// - `replies`: 依次消费的回复队列；用尽后使用 `fallback`；
/// - `fallback`: 兜底回复（默认空串，模拟"调用成功但无内容"）；
/// - `requests`: 已收到的请求（按调用顺序，含流式请求）；
/// - `failure`: `Some(消息)` 时所有生成调用返回 Llm 错误。
pub struct ScriptedLlm {
    replies: Mutex<VecDeque<String>>,
    fallback: String,
    requests: Mutex<Vec<ChatRequest>>,
    failure: Option<String>,
    backend: BackendConfig,
}

impl ScriptedLlm {
    /// 构造固定回复的 LLM（所有调用返回同一文本）。
    pub fn reply(text: &str) -> Self {
        Self {
            replies: Mutex::new(VecDeque::new()),
            fallback: text.to_string(),
            requests: Mutex::new(Vec::new()),
            failure: None,
            backend: BackendConfig::lm_studio_default(),
        }
    }

    /// 构造按次序回复的 LLM（队列用尽后回落到空串）。
    ///
    /// 用法:
    /// - 封存路径先返回 L1 摘要 JSON；多会话连续封存的场景按次序给出多条回复。
    pub fn replies(list: &[&str]) -> Self {
        Self {
            replies: Mutex::new(list.iter().map(|item| item.to_string()).collect()),
            fallback: String::new(),
            requests: Mutex::new(Vec::new()),
            failure: None,
            backend: BackendConfig::lm_studio_default(),
        }
    }

    /// 构造恒失败的 LLM（模拟后端不可用，用于降级与不落半条路径）。
    pub fn failing(message: &str) -> Self {
        Self {
            replies: Mutex::new(VecDeque::new()),
            fallback: String::new(),
            requests: Mutex::new(Vec::new()),
            failure: Some(message.to_string()),
            backend: BackendConfig::lm_studio_default(),
        }
    }

    /// 已收到的请求副本（按调用顺序）。
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .expect("脚本化 LLM 的请求记录锁不应中毒")
            .clone()
    }

    /// 已发生的生成调用次数（非流式 + 流式合计）。
    pub fn call_count(&self) -> usize {
        self.requests
            .lock()
            .expect("脚本化 LLM 的请求记录锁不应中毒")
            .len()
    }

    /// 取出本次调用的回复：队列优先，其次兜底。
    fn next_reply(&self) -> String {
        let mut queue = self
            .replies
            .lock()
            .expect("脚本化 LLM 的回复队列锁不应中毒");
        queue.pop_front().unwrap_or_else(|| self.fallback.clone())
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        self.requests
            .lock()
            .expect("脚本化 LLM 的请求记录锁不应中毒")
            .push(request.clone());
        if let Some(message) = &self.failure {
            return Err(RamariaError::llm(message.clone()));
        }
        Ok(self.next_reply())
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        self.requests
            .lock()
            .expect("脚本化 LLM 的请求记录锁不应中毒")
            .push(request.clone());
        if let Some(message) = &self.failure {
            return Err(RamariaError::llm(message.clone()));
        }
        let reply = self.next_reply();
        let deltas: Vec<char> = reply.chars().collect();
        let last = deltas.len().saturating_sub(1);
        let items: Vec<RamariaResult<StreamDelta>> = deltas
            .into_iter()
            .enumerate()
            .map(|(index, ch)| {
                Ok(StreamDelta {
                    content: ch.to_string(),
                    done: index == last,
                    metadata: if index == last {
                        Some("stop".to_string())
                    } else {
                        None
                    },
                })
            })
            .collect();
        Ok(Box::pin(stream::iter(items)))
    }

    fn capability(&self) -> &ModelCapability {
        &self.backend.capability
    }

    fn config(&self) -> &BackendConfig {
        &self.backend
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "ScriptedLlm"
    }
}

// =========================================================
// 预计算向量嵌入
// =========================================================

/// 预计算向量嵌入：文本到确定性向量，模拟"嵌入模型已就绪"的向量通道。
///
/// 职责:
/// - 让索引构建与召回的向量通道在无模型环境下可运行、可断言；
/// - 提供调用计数，验证"向量通道确实被使用 / 确实被跳过"。
///
/// 实现要求:
/// - 算法固定且无随机性：同文本在任何进程 / 任何时刻得到同一向量；
/// - 共享字词的文本余弦相似度更高（unigram + bigram 桶累加后 L2 归一化）。
pub struct DeterministicEmbedding {
    calls: AtomicUsize,
    info: EmbeddingModelInfo,
}

impl DeterministicEmbedding {
    /// 向量维度（足够小以便快速计算，又足以区分常见测试文本）。
    pub const DIMENSION: usize = 128;

    /// 构造可用的嵌入 provider。
    pub fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            info: EmbeddingModelInfo {
                model_id: "parity-deterministic-embedding".to_string(),
                dimension: Self::DIMENSION,
            },
        }
    }

    /// 已发生的向量化调用次数（含批量调用，按调用次数而非文本条数计）。
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl Default for DeterministicEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

/// 计算确定性嵌入向量（与 provider 内部算法保持一致，供断言复用）。
///
/// 算法:
/// 1. 去除空白字符后取字符序列；
/// 2. 每个单字与相邻二元组分别哈希到 `DIMENSION` 个桶（单字权重 1.0，二元组权重 2.0）；
/// 3. 向量做 L2 归一化（零向量保持零向量）。
fn deterministic_vector(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0_f32; DeterministicEmbedding::DIMENSION];
    let chars: Vec<char> = text.chars().filter(|ch| !ch.is_whitespace()).collect();

    for ch in &chars {
        let mut buffer = String::new();
        buffer.push(*ch);
        bump(&mut vector, &buffer, 1.0);
    }
    for pair in chars.windows(2) {
        let token: String = pair.iter().collect();
        bump(&mut vector, &token, 2.0);
    }

    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

/// 把 `token` 哈希到固定维度并累加权重。
fn bump(vector: &mut [f32], token: &str, weight: f32) {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    let index = (hasher.finish() as usize) % vector.len();
    vector[index] += weight;
}

#[async_trait]
impl EmbeddingProvider for DeterministicEmbedding {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Ok(deterministic_vector(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Ok(texts
            .iter()
            .map(|text| deterministic_vector(text))
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
