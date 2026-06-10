//! OpenAI-compatible 流式客户端（Phase 0 POC — 已废弃）
//!
//! Phase 3.1 已重构为正式 provider：
//! - `transport.rs`: 真正的 SSE 流式传输（不一次性读取响应体）
//! - `provider.rs`: ProviderBase + RetryConfig（重试/超时/消息组装）
//! - `lm_studio.rs` / `deepseek.rs` / `openai.rs`: LlmProvider trait 实现
//!
//! 此文件保留供参考，Phase 3 完成后可删除。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 一条对话消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// 聊天请求
#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
}

/// SSE data 行中 delta 片段
#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    #[serde(rename = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

/// OpenAI-compatible 流式客户端
pub struct LlmClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl LlmClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("创建 HTTP 客户端失败")?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            http,
        })
    }

    /// 流式发送聊天请求，每收到一个 delta 调用一次 `on_delta`
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        mut on_delta: impl FnMut(String),
    ) -> Result<()> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = ChatRequest {
            model: model.to_string(),
            messages,
            stream: true,
        };

        let mut req = self
            .http
            .post(&url)
            .json(&body)
            .header("Content-Type", "application/json");

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let response = req.send().await.context("请求失败")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status, text);
        }

        // Phase 0 POC: 一次性读取响应体，逐行解析 SSE
        // Phase 3 将改为真正的流式处理
        let body = response.text().await.context("读取响应失败")?;

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    break;
                }
                if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data)
                    && let Some(choice) = chunk.choices.first()
                {
                    if let Some(ref content) = choice.delta.content {
                        on_delta(content.clone());
                    }
                    if choice.finish_reason.is_some() {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}
