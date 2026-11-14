//! crates/ramaria-eval/src/score/judge/client.rs - Ramaria 评测判分客户端
//!
//! 设计特点:
//! - 只包一层非流式调用：判分要整段 JSON，流式增量无收益
//! - 采样参数固定为常量：三次投票靠独立调用，不靠温度扰动
//! - 每次调用一个新 request_id，便于在日志里对齐三次投票
//! - 不注入响应缓存：同一提示词的三次调用必须真的各发一次
//! - 用量随每次调用返回并就地累加，只记 token 数值；并发调用各记各的那份
//! - 不记录提示词与回复正文，错误由调用方按票降级

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::{ChatRequest, LlmProvider, LlmUsage};
use uuid::Uuid;

use super::prompt::JUDGE_TEMPLATE_VERSION;
use super::{JUDGE_MAX_TOKENS, JUDGE_TEMPERATURE};

/// 判分调用封装。
///
/// 字段约定:
/// - `provider`: 判分模型 provider，与产品引擎的 provider 互不影响；
/// - `usage` / `calls`: 本客户端累计的用量与调用次数（克隆共享同一份计数）。
#[derive(Clone)]
pub struct JudgeClient {
    provider: Arc<dyn LlmProvider>,
    usage: Arc<Mutex<LlmUsage>>,
    calls: Arc<AtomicUsize>,
}

impl JudgeClient {
    /// 用给定 provider 构造客户端。
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            provider,
            usage: Arc::new(Mutex::new(LlmUsage::default())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 累计用量快照。
    pub fn usage(&self) -> LlmUsage {
        *lock_recover(&self.usage, "eval_judge.usage")
    }

    /// 累计调用次数（每次非流式调用 +1，含解析失败后的重试）。
    pub fn usage_calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }

    /// 发起一次非流式判分调用。
    ///
    /// 参数:
    /// - `system`: 任务系统提示；
    /// - `user`: 本题用户消息。
    ///
    /// 返回:
    /// - 成功时返回模型完整文本；调用失败按 provider 错误返回，由调用方记为无效票。
    pub async fn ask(&self, system: &str, user: &str) -> RamariaResult<String> {
        let request = ChatRequest {
            system_prompt: system.to_string(),
            memory_context: None,
            history: Vec::new(),
            user_message: user.to_string(),
            temperature: JUDGE_TEMPERATURE,
            max_tokens: JUDGE_MAX_TOKENS,
            enable_thinking: Some(false),
            request_id: Uuid::new_v4(),
            template_version: JUDGE_TEMPLATE_VERSION.to_string(),
        };
        // 先计数再调用：调用次数统计的是"发给模型的请求数"，失败票同样计入。
        self.calls.fetch_add(1, Ordering::AcqRel);
        let (reply, usage) = self.provider.chat_with_usage(&request).await?;
        // 每次调用只累加自己那份用量：并发时互不覆盖。
        if let Some(usage) = usage {
            lock_recover(&self.usage, "eval_judge.usage").add(usage);
        }
        Ok(reply)
    }
}
