//! Phase 0 POC: 验证 LLM streaming 通路
//!
//! 用法（三选一）：
//!
//! ```powershell
//! # 1. LM Studio（本地，无需 API key）
//! $env:LLM_BASE_URL="http://localhost:1234/v1"
//! $env:LLM_MODEL="auto"
//! cargo run --example streaming_poc -p ramaria-llm
//!
//! # 2. DeepSeek
//! $env:LLM_BASE_URL="https://api.deepseek.com/v1"
//! $env:LLM_API_KEY="sk-xxx"
//! $env:LLM_MODEL="deepseek-chat"
//! cargo run --example streaming_poc -p ramaria-llm
//!
//! # 3. OpenAI
//! $env:LLM_BASE_URL="https://api.openai.com/v1"
//! $env:LLM_API_KEY="sk-xxx"
//! $env:LLM_MODEL="gpt-4o-mini"
//! cargo run --example streaming_poc -p ramaria-llm
//! ```

use ramaria_llm::client::{ChatMessage, LlmClient};

#[tokio::main]
async fn main() {
    let base_url = std::env::var("LLM_BASE_URL")
        .expect("请设置 LLM_BASE_URL，如 http://localhost:1234/v1");
    let api_key = std::env::var("LLM_API_KEY").ok();
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| "auto".into());

    println!("=== LLM Streaming POC ===");
    println!("base_url: {base_url}");
    println!("model   : {model}");
    println!("api_key : {}", if api_key.is_some() { "已设置" } else { "无（本地模式）" });
    println!("--- 开始请求 ---\n");

    let client = LlmClient::new(base_url, api_key).expect("创建客户端失败");

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: "用一句话介绍你自己。".to_string(),
    }];

    let mut char_count = 0;
    client
        .chat_stream(&model, messages, |delta| {
            print!("{delta}");
            char_count += delta.chars().count();
        })
        .await
        .expect("流式请求失败");

    println!("\n\n--- 请求完成 ---");
    println!("✅ 收到 {char_count} 个字符，streaming 通路正常");
}
