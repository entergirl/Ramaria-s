//! crates/ramaria-testkit/src/mock/routed.rs - Ramaria 测试按内容路由的 LLM mock 模块
//!
//! 设计特点:
//! - 按 system / user 双重子串路由：并发下响应与请求一一对应，不依赖调用次序
//! - 可配固定延迟：让并发窗口真实存在，供同时在飞数与并发上限断言
//! - 记录调用数、峰值在飞数与请求副本：并发上限与缓存线程安全可直接观测
//! - 用量随每次调用返回（固定值或按序取值）：并发累加的 token 总和与串行一致
//! - 只保存请求副本，不发起任何网络请求

use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider, LlmUsage, StreamDelta};
use ramaria_core::types::{BackendConfig, ModelCapability};

/// 按内容路由的延迟 LLM mock。
///
/// 字段约定:
/// - `routes`: (system 子串, user 子串, 回复) 三元组，按登记顺序取首个命中；
/// - `fallback`: 没有路由命中时的回复；
/// - `delay`: 每次调用等待的时长；
/// - `usage`: 每次调用返回的固定用量；`None` 表示不上报（按序取值优先于它）；
/// - `usages`: 按调用序号轮回取用的用量序列（用于并发下区分每次调用）；
/// - `calls` / `in_flight` / `peak`: 调用数、当前在飞数与历史峰值；
/// - `requests`: 已收到的请求副本（按进入顺序）。
pub struct MockRoutedLlm {
    routes: Vec<(String, String, String)>,
    fallback: String,
    delay: Duration,
    usage: Option<LlmUsage>,
    usages: Vec<LlmUsage>,
    backend: BackendConfig,
    calls: AtomicUsize,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    requests: Mutex<Vec<ChatRequest>>,
}

impl MockRoutedLlm {
    /// 用回退回复构造（无路由命中时一律返回该文本）。
    pub fn new(fallback: &str) -> Self {
        Self {
            routes: Vec::new(),
            fallback: fallback.to_string(),
            delay: Duration::ZERO,
            usage: None,
            usages: Vec::new(),
            backend: BackendConfig::lm_studio_default(),
            calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 追加一条路由（system 与 user 都按子串匹配）。
    ///
    /// 说明: 同一次请求命中多条路由时取登记顺序中的第一条。
    pub fn route(mut self, system: &str, user: &str, reply: impl Into<String>) -> Self {
        self.routes
            .push((system.to_string(), user.to_string(), reply.into()));
        self
    }

    /// 设置每次调用的等待时长。
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// 设置每次调用返回的固定用量。
    pub fn with_usage(mut self, usage: LlmUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// 设置按调用序号轮回取用的用量序列（优先于固定用量）。
    ///
    /// 说明: 序号在调用进入时分配，便于在并发下区分每一次调用的用量；
    /// 只要序列长度整除调用总数，总和就与调用完成顺序无关。
    pub fn with_usage_seq(mut self, usages: Vec<LlmUsage>) -> Self {
        self.usages = usages;
        self
    }

    /// 已发生的 chat 调用次数。
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }

    /// 历史最大同时在飞调用数。
    pub fn peak_in_flight(&self) -> usize {
        self.peak.load(Ordering::Acquire)
    }

    /// 已收到的请求副本（按进入顺序）。
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("请求记录锁").clone()
    }

    /// 第 `call_index` 次调用返回的用量。
    fn usage_for(&self, call_index: usize) -> Option<LlmUsage> {
        if self.usages.is_empty() {
            return self.usage;
        }
        Some(self.usages[call_index % self.usages.len()])
    }

    /// 按路由表匹配回复。
    fn reply_for(&self, request: &ChatRequest) -> String {
        self.routes
            .iter()
            .find(|(system, user, _)| {
                request.system_prompt.contains(system) && request.user_message.contains(user)
            })
            .map(|(_, _, reply)| reply.clone())
            .unwrap_or_else(|| self.fallback.clone())
    }
}

#[async_trait]
impl LlmProvider for MockRoutedLlm {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        Ok(self.chat_with_usage(request).await?.0)
    }

    async fn chat_with_usage(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<(String, Option<LlmUsage>)> {
        let call_index = self.calls.fetch_add(1, Ordering::AcqRel);
        let current = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(current, Ordering::AcqRel);
        self.requests
            .lock()
            .expect("请求记录锁")
            .push(request.clone());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        Ok((self.reply_for(request), self.usage_for(call_index)))
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Err(RamariaError::unsupported(
            "MockRoutedLlm 不提供流式调用（只服务判分并发用例）",
        ))
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
        "MockRoutedLlm"
    }
}
