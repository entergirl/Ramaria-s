//! crates/ramaria-testkit/src/mock/llm.rs - Ramaria 测试 LLM mock 模块
//!
//! 设计特点:
//! - 固定回复 / 响应队列 / 恒失败三种口径，覆盖多步链路与降级路径
//! - 流式按脚本片段或回复正文逐字发出，末片标记 done 与 stop
//! - 健康探测按剩余失败次数返回错误，覆盖后端启动重试
//! - 图片理解记录收到的 data URI 序列与调用次数，回复可覆写
//! - 调用序列（chat / chat_vision）可查询，不发起任何网络请求

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::{Stream, stream};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider, StreamDelta};
use ramaria_core::types::{BackendConfig, ModelCapability};

// =========================================================
// MockLlm
// =========================================================

/// 可配置的测试 LLM。
///
/// 口径:
/// - [`MockLlm::new`] / [`MockLlm::with_reply`]：返回固定文本；
/// - [`MockLlm::local`]：返回空串（只读用例）；
/// - [`MockLlm::online`]：线上 provider 配置（不发网络请求）；
/// - [`MockLlm::failing`]：生成调用恒失败；
/// - `set_response` / `set_responses`：单值或队列预设（队列优先消费）。
pub struct MockLlm {
    name: &'static str,
    strict_response: bool,
    /// 无显式流式脚本时，固定回复是否按字符切分为多个片段。
    stream_reply_as_chars: bool,
    backend: BackendConfig,
    reply: Mutex<String>,
    response: Mutex<Option<String>>,
    queue: Mutex<VecDeque<String>>,
    always_fail: bool,
    stream_chunks: Option<Vec<String>>,
    stream_fails_after_chunks: bool,
    health_failures: AtomicUsize,
    chat_calls: AtomicUsize,
    requests: Mutex<Vec<ChatRequest>>,
    last_request: Mutex<Option<ChatRequest>>,
    vision_reply: Option<String>,
    vision_failures: AtomicUsize,
    vision_calls: AtomicUsize,
    vision_images: Mutex<Vec<Vec<String>>>,
    call_log: Mutex<Vec<&'static str>>,
}

impl MockLlm {
    /// 创建返回固定回复的 mock。
    pub fn new(reply: &str) -> Self {
        Self {
            name: "MockLlm",
            strict_response: false,
            stream_reply_as_chars: true,
            backend: BackendConfig::lm_studio_default(),
            reply: Mutex::new(reply.to_string()),
            response: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            always_fail: false,
            stream_chunks: None,
            stream_fails_after_chunks: false,
            health_failures: AtomicUsize::new(0),
            chat_calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            last_request: Mutex::new(None),
            vision_reply: None,
            vision_failures: AtomicUsize::new(0),
            vision_calls: AtomicUsize::new(0),
            vision_images: Mutex::new(Vec::new()),
            call_log: Mutex::new(Vec::new()),
        }
    }

    /// 本地 LM Studio 口径的 mock（返回空串、流式空流）。
    pub fn local() -> Self {
        Self {
            stream_reply_as_chars: false,
            ..Self::new("")
        }
    }

    /// 返回固定文本的 mock（流式为单片段）。
    pub fn with_reply(reply: &str) -> Self {
        Self {
            stream_reply_as_chars: false,
            ..Self::new(reply)
        }
    }

    /// 恒失败的 mock（模拟 LLM 后端不可用）。
    pub fn failing() -> Self {
        Self {
            always_fail: true,
            stream_reply_as_chars: false,
            ..Self::new("")
        }
    }

    /// 线上 provider（DeepSeek）口径的 mock（不发网络请求、不含真实密钥）。
    pub fn online() -> Self {
        Self {
            backend: BackendConfig::deepseek_default(),
            stream_reply_as_chars: false,
            ..Self::new("")
        }
    }

    /// 使用自定义 BackendConfig 创建 mock（能力与配置取自入参）。
    pub fn with_config(reply: &str, config: BackendConfig) -> Self {
        Self {
            backend: config,
            ..Self::new(reply)
        }
    }

    /// 构造流式多片段 mock（按序发出片段，末片标记 done）。
    pub fn with_stream_chunks(chunks: &[&str]) -> Self {
        Self {
            stream_chunks: Some(chunks.iter().map(|chunk| chunk.to_string()).collect()),
            ..Self::local()
        }
    }

    /// 链式覆盖流式片段（保留既有 provider / 回复配置）。
    pub fn streaming(mut self, chunks: &[&str]) -> Self {
        self.stream_chunks = Some(chunks.iter().map(|chunk| chunk.to_string()).collect());
        self
    }

    /// 构造流中错误 mock（先发片段再返回错误）。
    pub fn stream_fails_after(chunks: &[&str]) -> Self {
        Self {
            stream_chunks: Some(chunks.iter().map(|chunk| chunk.to_string()).collect()),
            stream_fails_after_chunks: true,
            ..Self::local()
        }
    }

    /// 指定健康探测前 N 次失败（0 表示探测直接成功）。
    pub fn with_health_failures(self, failures: usize) -> Self {
        self.health_failures.store(failures, Ordering::Release);
        self
    }

    /// 覆写图片理解回复（未设置时返回内置默认描述）。
    pub fn with_vision_reply(mut self, reply: &str) -> Self {
        self.vision_reply = Some(reply.to_string());
        self
    }

    /// 指定图片理解前 N 次失败（0 表示直接成功）。
    pub fn vision_failing(self, failures: usize) -> Self {
        self.vision_failures.store(failures, Ordering::Release);
        self
    }

    /// 设置图片理解剩余失败次数。
    pub fn set_vision_failures(&self, failures: usize) {
        self.vision_failures.store(failures, Ordering::Release);
    }

    /// 图片理解调用次数（含探测与内容理解；成功与失败均计入）。
    pub fn vision_call_count(&self) -> usize {
        self.vision_calls.load(Ordering::Acquire)
    }

    /// 图片理解收到的 data URI 副本（按调用顺序）。
    pub fn vision_images(&self) -> Vec<Vec<String>> {
        self.vision_images.lock().expect("图片记录锁").clone()
    }

    /// 调用序列副本（`chat` / `chat_vision`，按发生顺序）。
    pub fn call_log(&self) -> Vec<&'static str> {
        self.call_log.lock().expect("调用序列锁").clone()
    }

    /// 生成调用次数（chat 与 chat_stream 合计，含失败）。
    pub fn chat_calls(&self) -> usize {
        self.chat_calls.load(Ordering::Acquire)
    }

    /// 生成调用次数（chat 与 chat_stream 合计，含失败）。
    pub fn call_count(&self) -> u32 {
        self.chat_calls.load(Ordering::Acquire) as u32
    }

    /// 已收到的生成请求副本（按调用顺序）。
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("请求记录锁").clone()
    }

    /// 最近一次生成请求（供 Prompt 内容断言）。
    pub fn last_request(&self) -> Option<ChatRequest> {
        self.last_request.lock().expect("请求记录锁").clone()
    }

    /// 预设 / 更新单值回复。
    pub fn set_reply(&self, reply: &str) {
        *self.reply.lock().expect("回复锁") = reply.to_string();
    }

    /// 预设下次 chat 的返回值（一次性消费）。
    pub fn set_response(&self, text: impl Into<String>) {
        *self.response.lock().expect("单次响应锁") = Some(text.into());
    }

    /// 预设响应队列（逐次消费；队列耗尽后回退单值回复）。
    pub fn set_responses(&self, texts: Vec<String>) {
        let mut queue = self.queue.lock().expect("响应队列锁");
        for text in texts {
            queue.push_back(text);
        }
    }

    /// 记录一次生成调用（请求与计数）。
    fn record_chat(&self, request: &ChatRequest) {
        self.chat_calls.fetch_add(1, Ordering::Relaxed);
        self.requests
            .lock()
            .expect("请求记录锁")
            .push(request.clone());
        *self.last_request.lock().expect("请求记录锁") = Some(request.clone());
        self.call_log.lock().expect("调用序列锁").push("chat");
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        self.record_chat(request);
        if self.always_fail {
            return Err(RamariaError::llm("MockLlm 恒失败（模拟 LLM 后端不可用）"));
        }
        if let Some(text) = self.queue.lock().expect("响应队列锁").pop_front() {
            return Ok(text);
        }
        if let Some(text) = self.response.lock().expect("单次响应锁").take() {
            return Ok(text);
        }
        if self.strict_response {
            return Err(RamariaError::llm("mock: 未预设 chat 返回值"));
        }
        Ok(self.reply.lock().expect("回复锁").clone())
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        self.record_chat(request);
        if self.always_fail {
            return Err(RamariaError::llm("MockLlm 恒失败（模拟 LLM 后端不可用）"));
        }

        // 片段脚本优先；否则由固定回复决定（非空 → 单片段，空 → 空流）
        let chunks: Vec<String> = match &self.stream_chunks {
            Some(chunks) => chunks.clone(),
            None => {
                let reply = self.reply.lock().expect("回复锁").clone();
                if reply.is_empty() {
                    Vec::new()
                } else if self.stream_reply_as_chars {
                    reply.chars().map(|ch| ch.to_string()).collect()
                } else {
                    vec![reply]
                }
            }
        };
        let last = chunks.len().saturating_sub(1);
        let terminal = !self.stream_fails_after_chunks;
        let mut items: Vec<RamariaResult<StreamDelta>> = chunks
            .into_iter()
            .enumerate()
            .map(|(index, content)| {
                let is_last = index == last;
                Ok(StreamDelta {
                    content,
                    done: terminal && is_last,
                    metadata: if terminal && is_last {
                        Some("stop".to_string())
                    } else {
                        None
                    },
                })
            })
            .collect();
        if self.stream_fails_after_chunks {
            items.push(Err(RamariaError::llm(
                "MockLlm 流中错误（模拟流式生成中断）",
            )));
        }
        Ok(Box::pin(stream::iter(items)))
    }

    async fn chat_vision(
        &self,
        _request: &ChatRequest,
        image_data_uris: &[String],
    ) -> RamariaResult<String> {
        self.vision_calls.fetch_add(1, Ordering::Relaxed);
        self.vision_images
            .lock()
            .expect("图片记录锁")
            .push(image_data_uris.to_vec());
        self.call_log
            .lock()
            .expect("调用序列锁")
            .push("chat_vision");
        if self.vision_failures.load(Ordering::Acquire) > 0 {
            self.vision_failures.fetch_sub(1, Ordering::AcqRel);
            return Err(RamariaError::llm("MockLlm 图片理解失败（模拟调用失败）"));
        }
        Ok(self
            .vision_reply
            .clone()
            .unwrap_or_else(|| "一张测试图片".to_string()))
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

    async fn health_check(&self) -> RamariaResult<()> {
        if self.health_failures.load(Ordering::Acquire) > 0 {
            self.health_failures.fetch_sub(1, Ordering::AcqRel);
            return Err(RamariaError::llm("MockLlm 健康探测失败（模拟后端未就绪）"));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        self.name
    }
}

// =========================================================
// MockLlmProvider（L1 管线：命名 + 严格响应）
// =========================================================

/// L1 管线使用的 LLM mock。
///
/// 口径:
/// - 构造仅接收名称（写入事件名，不影响回复内容）；
/// - 严格模式：未通过 set_response / set_responses 预设时 chat 返回错误，
///   避免默认空回复掩盖管线漏设响应；
/// - 代理底层 mock 的响应队列、请求记录与调用计数。
pub struct MockLlmProvider {
    inner: MockLlm,
}

impl MockLlmProvider {
    /// 用指定名称构造严格模式 mock。
    pub fn new(name: &'static str) -> Self {
        let mut inner = MockLlm::new("");
        inner.name = name;
        inner.strict_response = true;
        Self { inner }
    }

    /// 预设下次 chat 的返回值（一次性消费）。
    pub fn set_response(&self, text: impl Into<String>) {
        self.inner.set_response(text);
    }

    /// 预设响应队列（逐次消费；队列优先于单次响应）。
    pub fn set_responses(&self, texts: Vec<String>) {
        self.inner.set_responses(texts);
    }

    /// 最近一次请求副本。
    pub fn last_request(&self) -> Option<ChatRequest> {
        self.inner.last_request()
    }

    /// 全部请求副本（按调用顺序）。
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.inner.requests()
    }

    /// 调用次数。
    pub fn call_count(&self) -> u32 {
        self.inner.call_count()
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        self.inner.chat(request).await
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        self.inner.chat_stream(request).await
    }

    async fn chat_vision(
        &self,
        request: &ChatRequest,
        image_data_uris: &[String],
    ) -> RamariaResult<String> {
        self.inner.chat_vision(request, image_data_uris).await
    }

    fn capability(&self) -> &ModelCapability {
        self.inner.capability()
    }

    fn config(&self) -> &BackendConfig {
        self.inner.config()
    }

    async fn validate(&self) -> RamariaResult<()> {
        self.inner.validate().await
    }

    async fn health_check(&self) -> RamariaResult<()> {
        self.inner.health_check().await
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
}

// =========================================================
// MockFailingLlm
// =========================================================

/// 始终返回错误的 LLM mock（覆盖错误处理与降级路径）。
pub struct MockFailingLlm {
    error_msg: String,
    model_capability: ModelCapability,
    config: BackendConfig,
}

impl MockFailingLlm {
    /// 用指定错误文本构造恒失败 mock。
    pub fn new(error_msg: &str) -> Self {
        Self {
            error_msg: error_msg.to_string(),
            model_capability: ModelCapability {
                provider: ramaria_core::types::LlmProvider::LmStudio,
                model_id: "mock-model".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config: BackendConfig::lm_studio_default(),
        }
    }
}

#[async_trait]
impl LlmProvider for MockFailingLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Err(RamariaError::llm(self.error_msg.clone()))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Err(RamariaError::llm(self.error_msg.clone()))
    }

    fn capability(&self) -> &ModelCapability {
        &self.model_capability
    }

    fn config(&self) -> &BackendConfig {
        &self.config
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MockFailingLlm"
    }
}

// =========================================================
// ScriptedLlm
// =========================================================

/// 脚本化 LLM：按调用次序返回预设回复，队列用尽后回落空串。
pub struct ScriptedLlm {
    replies: Mutex<VecDeque<String>>,
    chat_calls: AtomicUsize,
    backend: BackendConfig,
}

impl ScriptedLlm {
    /// 按调用次序构造。
    pub fn replies(list: &[&str]) -> Self {
        Self {
            replies: Mutex::new(list.iter().map(|reply| reply.to_string()).collect()),
            chat_calls: AtomicUsize::new(0),
            backend: BackendConfig::lm_studio_default(),
        }
    }

    /// chat 调用次数（含成功与失败）。
    pub fn call_count(&self) -> usize {
        self.chat_calls.load(Ordering::Acquire)
    }

    fn next_reply(&self) -> String {
        self.replies
            .lock()
            .expect("脚本回复队列锁")
            .pop_front()
            .unwrap_or_default()
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        self.chat_calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.next_reply())
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        self.chat_calls.fetch_add(1, Ordering::Relaxed);
        let reply = self.next_reply();
        let items = vec![Ok(StreamDelta {
            content: reply,
            done: true,
            metadata: Some("stop".to_string()),
        })];
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
