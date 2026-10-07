//! crates/ramaria-memory/src/l1/summarizer/tests/mod.rs - L0-L1 摘要管线单元测试
//!
//! 设计特点:
//! - 覆盖 config 默认值 / JSON 解析 / evidence_notes 宽容校验 / 渐进式触发 / 上文构建 / 集成写库。
//! - LLM 路径使用 mock LlmProvider；存储使用 MockStorage（与 summarizer 主体同 crate 测试夹具）。
//! - 隐私: 测试仅用合成消息，不依赖真实 LLM/embedding/数据库。
use super::types::L1SummaryResponse;
use super::*;
use crate::l1::mock::{MockStorage, make_msg};
use crate::utt::UttChunk;
use ramaria_core::MemoryL1;
use ramaria_core::keyword::KeywordToken;
use ramaria_core::types::{EvidenceNote, Message, MessageRole};
use uuid::Uuid;

/// 构造带 persona_uid 的 assistant 消息（目标发言）。
fn target_msg(session_id: Uuid, created_at: i64, content: &str) -> Message {
    let mut m = make_msg(session_id, MessageRole::Assistant, content);
    m.created_at = created_at;
    m.persona_uid = Some("char-0001".to_string());
    m
}

/// 构造用户消息（非目标侧）。
fn user_msg(session_id: Uuid, created_at: i64, content: &str) -> Message {
    let mut m = make_msg(session_id, MessageRole::User, content);
    m.created_at = created_at;
    m
}

/// 构造一个消息块。
fn make_chunk(msgs: Vec<Message>) -> UttChunk {
    UttChunk::from_messages(msgs)
}

/// 构造带 continuation 的 mock LLM 响应 JSON。
fn llm_json(summary: &str, continuation: Option<&str>) -> String {
    let mut obj = serde_json::json!({
        "summary": summary,
        "keywords": "测试,关键词",
        "time_period": "上午",
        "atmosphere": "平静",
        "valence": 0.0,
        "salience": 0.5,
        "evidence_notes": []
    });
    if let Some(c) = continuation {
        obj["continuation"] = serde_json::json!(c);
    }
    obj.to_string()
}

mod evidence;
mod fanout;
mod integration;
mod progressive;
mod utt;
mod validate;
