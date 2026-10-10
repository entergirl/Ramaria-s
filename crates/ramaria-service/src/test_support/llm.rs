//! crates/ramaria-service/src/test_support/llm.rs - Ramaria 服务层测试用 LLM mock 模块
//!
//! 设计特点:
//! - 满足 `LlmProvider` 契约但不发起任何网络调用（CI 无外网依赖）；
//! - 固定回复 / 恒失败 / 流式脚本 / 健康探测重试等口径，覆盖只读用例与降级路径；
//! - 脚本化 LLM 按调用次序消费回复队列，覆盖多步 LLM 链路的序列场景；
//! - 调用计数与请求记录供 Prompt 结构断言使用，不落日志、不含真实密钥；
//! - 调用序列记录（`chat` / `chat_vision`）供"先理解后摘要"的链路顺序断言。

use std::collections::VecDeque;
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider, StreamDelta};
use ramaria_core::types::{BackendConfig, ModelCapability};

// =========================================================
// 最小 LLM mock
// =========================================================

/// 最小 LLM mock：服务层用例测试不依赖真实 LLM，也不发起网络调用。
///
/// 口径:
/// - [`MockLlm::local`]：`chat` 返回空串（模拟"调用成功但无内容"，适合只读用例）；
/// - [`MockLlm::with_reply`]：`chat` 返回固定文本、流式发单片段（模拟"LLM 可用"）；
/// - [`MockLlm::with_stream_chunks`]：流式按序发多片段（末片 `done=true`）；
/// - [`MockLlm::stream_fails_after`]：流式先发片段再返回错误（模拟"流中错误"）；
/// - [`MockLlm::failing`]：生成调用恒失败（模拟后端不可用，覆盖降级与"不落半条"路径）；
/// - [`MockLlm::online`]：线上 provider（DeepSeek）口径的 mock（本地不发网络请求）；
/// - [`MockLlm::with_health_failures`]：健康探测前 N 次失败（模拟后端启动中，覆盖探测重试）；
/// - `chat_vision`：按覆写回复 / 剩余失败次数返回（图片理解链路；默认成功）。
pub(crate) struct MockLlm {
    backend: BackendConfig,
    reply: Option<String>,
    /// 为 true 时所有生成调用返回 Llm 错误（不发起网络调用）。
    always_fail: bool,
    /// 流式片段脚本（None = 由 `reply` 决定：Some 发单片段、None 发空流）。
    stream_chunks: Option<Vec<String>>,
    /// 为 true 时流式先发完脚本片段再返回错误（模拟"流中错误"）。
    stream_fails_after_chunks: bool,
    /// 健康探测剩余失败次数（递减；0 表示探测成功）。
    health_failures: std::sync::atomic::AtomicUsize,
    /// 生成调用计数（`chat` 与 `chat_stream` 合计，含失败；健康探测不计入）。
    chat_calls: std::sync::atomic::AtomicUsize,
    /// 已收到的生成请求（按调用顺序；供 Prompt 结构断言使用，不落日志）。
    requests: std::sync::Mutex<Vec<ChatRequest>>,
    /// 图片理解覆写回复（None = 内置默认描述）。
    vision_reply: Option<String>,
    /// 图片理解剩余失败次数（递减；0 表示直接成功）。
    vision_failures: std::sync::atomic::AtomicUsize,
    /// 图片理解调用计数（含探测与内容理解；是否成功均计入）。
    vision_calls: std::sync::atomic::AtomicUsize,
    /// 调用序列记录（按发生顺序；`"chat"` = 文本生成，`"chat_vision"` = 图片理解）。
    call_log: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

impl MockLlm {
    /// 本地 LM Studio 口径的 mock（无 API key、无网络；chat 返回空串、流式为空流）。
    pub(crate) fn local() -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: None,
            always_fail: false,
            stream_chunks: None,
            stream_fails_after_chunks: false,
            health_failures: std::sync::atomic::AtomicUsize::new(0),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
            vision_reply: None,
            vision_failures: std::sync::atomic::AtomicUsize::new(0),
            vision_calls: std::sync::atomic::AtomicUsize::new(0),
            call_log: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// 返回固定文本的 mock（供需要 LLM 真实响应的用例，如 L1 摘要 JSON）。
    pub(crate) fn with_reply(reply: &str) -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: Some(reply.to_string()),
            always_fail: false,
            stream_chunks: None,
            stream_fails_after_chunks: false,
            health_failures: std::sync::atomic::AtomicUsize::new(0),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
            vision_reply: None,
            vision_failures: std::sync::atomic::AtomicUsize::new(0),
            vision_calls: std::sync::atomic::AtomicUsize::new(0),
            call_log: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// 恒失败的 mock（模拟"LLM 后端不可用"，用于降级与不落半条写入的断言）。
    pub(crate) fn failing() -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: None,
            always_fail: true,
            stream_chunks: None,
            stream_fails_after_chunks: false,
            health_failures: std::sync::atomic::AtomicUsize::new(0),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
            vision_reply: None,
            vision_failures: std::sync::atomic::AtomicUsize::new(0),
            vision_calls: std::sync::atomic::AtomicUsize::new(0),
            call_log: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// 线上 provider（DeepSeek）口径的 mock（供隐私门禁用例；不发网络请求、不含真实 key）。
    pub(crate) fn online() -> Self {
        Self {
            backend: BackendConfig::deepseek_default(),
            reply: None,
            always_fail: false,
            stream_chunks: None,
            stream_fails_after_chunks: false,
            health_failures: std::sync::atomic::AtomicUsize::new(0),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
            vision_reply: None,
            vision_failures: std::sync::atomic::AtomicUsize::new(0),
            vision_calls: std::sync::atomic::AtomicUsize::new(0),
            call_log: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// 构造"流式多片段"mock（按序发出片段；末片 `done=true`）。
    pub(crate) fn with_stream_chunks(chunks: &[&str]) -> Self {
        Self {
            stream_chunks: Some(chunks.iter().map(|chunk| chunk.to_string()).collect()),
            ..Self::local()
        }
    }

    /// 构造"流中错误"mock（先发片段再返回 Llm 错误，模拟流式生成中断）。
    pub(crate) fn stream_fails_after(chunks: &[&str]) -> Self {
        Self {
            stream_chunks: Some(chunks.iter().map(|chunk| chunk.to_string()).collect()),
            stream_fails_after_chunks: true,
            ..Self::local()
        }
    }

    /// 指定健康探测前 N 次失败（链式调用；N = 0 表示探测直接成功）。
    pub(crate) fn with_health_failures(self, failures: usize) -> Self {
        Self {
            health_failures: std::sync::atomic::AtomicUsize::new(failures),
            ..self
        }
    }

    /// 覆写图片理解回复（链式调用；未设置时返回内置默认描述）。
    pub(crate) fn with_vision_reply(self, reply: &str) -> Self {
        Self {
            vision_reply: Some(reply.to_string()),
            ..self
        }
    }

    /// 指定图片理解前 N 次失败（链式调用；N = 0 表示直接成功）。
    pub(crate) fn vision_failing(self, failures: usize) -> Self {
        Self {
            vision_failures: std::sync::atomic::AtomicUsize::new(failures),
            ..self
        }
    }

    /// 设置图片理解剩余失败次数（供"探测已缓存成功后再编排调用失败"的场景）。
    pub(crate) fn set_vision_failures(&self, failures: usize) {
        self.vision_failures
            .store(failures, std::sync::atomic::Ordering::Release);
    }

    /// 图片理解调用次数（含探测与内容理解；成功与失败均计入）。
    pub(crate) fn vision_call_count(&self) -> usize {
        self.vision_calls.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 调用序列副本（按发生顺序；`"chat"` = 文本生成，`"chat_vision"` = 图片理解）。
    // 仅导入链路用例消费；未启用 `importer` feature 时无调用方。
    #[cfg_attr(not(feature = "importer"), allow(dead_code))]
    pub(crate) fn call_log(&self) -> Vec<&'static str> {
        self.call_log
            .lock()
            .expect("MockLlm 的调用序列锁不应中毒")
            .clone()
    }

    /// 生成调用次数（`chat` 与 `chat_stream` 合计，含成功与失败；健康探测不计入）。
    pub(crate) fn chat_calls(&self) -> usize {
        self.chat_calls.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 已收到的生成请求副本（按调用顺序；供 Prompt 结构断言）。
    pub(crate) fn requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .expect("MockLlm 的请求记录锁不应中毒")
            .clone()
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        self.chat_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.requests
            .lock()
            .expect("MockLlm 的请求记录锁不应中毒")
            .push(request.clone());
        self.call_log
            .lock()
            .expect("MockLlm 的调用序列锁不应中毒")
            .push("chat");
        if self.always_fail {
            return Err(RamariaError::llm("MockLlm 恒失败（模拟 LLM 后端不可用）"));
        }
        Ok(self.reply.clone().unwrap_or_default())
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> RamariaResult<
        std::pin::Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>,
    > {
        self.chat_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.requests
            .lock()
            .expect("MockLlm 的请求记录锁不应中毒")
            .push(request.clone());
        self.call_log
            .lock()
            .expect("MockLlm 的调用序列锁不应中毒")
            .push("chat");
        if self.always_fail {
            return Err(RamariaError::llm("MockLlm 恒失败（模拟 LLM 后端不可用）"));
        }

        // 片段脚本：显式脚本优先；否则由固定回复决定（空回复 → 空流）
        let chunks: Vec<String> = match &self.stream_chunks {
            Some(chunks) => chunks.clone(),
            None => match &self.reply {
                Some(reply) if !reply.is_empty() => vec![reply.clone()],
                _ => Vec::new(),
            },
        };
        // 流中错误模式下不标记终止片段（done=true 是终止信号，错误在其后送达）
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
        Ok(Box::pin(futures::stream::iter(items)))
    }

    /// 图片理解：按剩余失败次数返回错误，否则返回覆写回复 / 内置默认描述。
    async fn chat_vision(
        &self,
        _request: &ChatRequest,
        _image_data_uris: &[String],
    ) -> RamariaResult<String> {
        use std::sync::atomic::Ordering;

        self.vision_calls.fetch_add(1, Ordering::Relaxed);
        self.call_log
            .lock()
            .expect("MockLlm 的调用序列锁不应中毒")
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

    /// 健康探测：按剩余失败次数返回错误（模拟后端启动中，之后转为可达）。
    async fn health_check(&self) -> RamariaResult<()> {
        use std::sync::atomic::Ordering;

        if self.health_failures.load(Ordering::Acquire) > 0 {
            self.health_failures.fetch_sub(1, Ordering::AcqRel);
            return Err(RamariaError::llm("MockLlm 健康探测失败（模拟后端未就绪）"));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MockLlm"
    }
}

// =========================================================
// 脚本化 LLM
// =========================================================

/// 脚本化 LLM：按调用次序返回预设回复（供多步 LLM 链路的序列场景）。
///
/// 口径:
/// - 按调用次序消费回复队列；队列用尽后回落空串（模拟"调用成功但无内容"）；
/// - 调用次数可查询，用于断言"链路的下一轮 LLM 调用是否被发起"；
/// - 流式调用消费下一条脚本回复并以单片段发出（多步链路可覆盖）；
///   恒失败口径见 [`MockLlm::failing`]。
pub(crate) struct ScriptedLlm {
    replies: std::sync::Mutex<VecDeque<String>>,
    chat_calls: std::sync::atomic::AtomicUsize,
    backend: BackendConfig,
}

impl ScriptedLlm {
    /// 按调用次序构造（队列用尽后回落空串）。
    pub(crate) fn replies(list: &[&str]) -> Self {
        Self {
            replies: std::sync::Mutex::new(list.iter().map(|reply| reply.to_string()).collect()),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
            backend: BackendConfig::lm_studio_default(),
        }
    }

    /// `chat` 调用次数（含成功与失败）。
    pub(crate) fn call_count(&self) -> usize {
        self.chat_calls.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[async_trait::async_trait]
impl LlmProvider for ScriptedLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        self.chat_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let reply = self
            .replies
            .lock()
            .expect("脚本化 LLM 的回复队列锁不应中毒")
            .pop_front()
            .unwrap_or_default();
        Ok(reply)
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<
        std::pin::Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>,
    > {
        self.chat_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let reply = self
            .replies
            .lock()
            .expect("脚本化 LLM 的回复队列锁不应中毒")
            .pop_front()
            .unwrap_or_default();
        let items = vec![Ok(StreamDelta {
            content: reply,
            done: true,
            metadata: Some("stop".to_string()),
        })];
        Ok(Box::pin(futures::stream::iter(items)))
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
