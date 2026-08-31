//! crates/ramaria-memory/src/utt/mod.rs - utt 话语块模块（v1.4 表达层）
//!
//! 设计特点:
//! - 原文切分（splitter）+ 全量/增量构建（builder）两个子模块
//! - 原文是最高敏感层：块按 persona_uid 严格隔离，内容不写日志
//! - embedding 以 f32 小端 BLOB 持久化，编解码集中在本模块
//! - 切分参数全部可配置（θ_gap 时间间隙 / 条数上限），默认值对齐 config 默认
//!
//! 边界:
//! - 本模块不直接访问数据库：builder 通过 `StorageBackend` trait 读写
//! - 不依赖具体 LLM/embedding provider：embedding 由调用方注入 trait 对象

pub mod builder;
pub mod splitter;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Message, MessageRole};

/// utt 切分配置。
///
/// 字段约定:
/// - `theta_gap_minutes`: 时间间隙阈值（分钟）。相邻消息间隔超过此值切分为新块。
/// - `max_msgs_per_block`: 单块最大消息条数。超过此条数强制切分（单边合并可突破上限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UttSplitterConfig {
    /// 时间间隙阈值（分钟），默认 10（窄切分、更细粒度分块）
    pub theta_gap_minutes: u32,
    /// 单块最大消息条数，默认 80（更大块、更少切分）
    pub max_msgs_per_block: u32,
}

impl Default for UttSplitterConfig {
    fn default() -> Self {
        Self {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
        }
    }
}

/// 切分产生的话语块（未落库）。
///
/// 字段约定:
/// - `messages`: 块内消息（按时间升序，已过滤系统/工具消息）。
/// - `start_msg_id` / `end_msg_id`: 消息区间（供幂等去重与桥接定位）。
/// - `time_span_ms`: 首末消息时间跨度（毫秒），单条消息为 0。
#[derive(Debug, Clone)]
pub struct UttChunk {
    /// 块内消息（时间升序）
    pub messages: Vec<Message>,
    /// 块内首条消息 ID
    pub start_msg_id: uuid::Uuid,
    /// 块内末条消息 ID
    pub end_msg_id: uuid::Uuid,
    /// 块内消息条数
    pub msg_count: u32,
    /// 首末消息时间跨度（毫秒）
    pub time_span_ms: i64,
}

impl UttChunk {
    /// 由消息序列直接构造单块（不切分，v1.4 整会话路径）。
    ///
    /// 参数:
    /// - `messages`: 已按时间升序的消息（调用方负责排序/过滤）。
    ///
    /// 返回:
    /// - 单个话语块（消息首末区间元数据自动计算）。
    pub fn from_messages(messages: Vec<Message>) -> Self {
        let first = messages.first().expect("块非空");
        let last = messages.last().expect("块非空");
        Self {
            start_msg_id: first.id,
            end_msg_id: last.id,
            msg_count: messages.len() as u32,
            time_span_ms: last.created_at - first.created_at,
            messages,
        }
    }
}

/// 判断消息是否为"目标 persona 发言"。
///
/// 规则:
/// - 仅 Assistant 角色的消息可能成为目标发言（User/System/Tool 恒非目标，
///   即使携带目标 persona_uid——导入数据可能给用户消息带 uid，但发言权属于角色）。
/// - `target` 为 Some(uid) 时：`msg.persona_uid == Some(uid)` 才算目标发言
///   （其他 assistant 消息视为对方侧，避免跨 persona 混淆）。
/// - `target` 为 None（rama 自身会话）时：无 persona_uid 的 assistant 消息视为目标发言。
///
/// 参数:
/// - `msg`: 消息。
/// - `target`: 目标 persona UID（None 表示 rama 自身会话）。
///
/// 返回:
/// - 是否为目标 persona 的发言。
pub fn is_target_speech(msg: &Message, target: Option<&str>) -> bool {
    if msg.role != MessageRole::Assistant {
        return false;
    }
    match target {
        Some(uid) => msg.persona_uid.as_deref() == Some(uid),
        None => msg.persona_uid.is_none(),
    }
}

/// 从会话消息推断目标 persona UID（防御 `session.persona_uid=NULL` 存量场景，P0-2）。
///
/// 规则:
/// - 取第一条（时间升序）Assistant 角色且带 persona_uid 的消息的 uid；
///   对话中 persona 的回复必然以 assistant 角色落库，故首条 assistant 发言
///   即该会话绑定的对话人格。
/// - 无任何 assistant 发言（纯用户会话/空会话）→ None，由调用方回退默认值。
///
/// 参数:
/// - `messages`: 会话消息（时间升序；调用方负责排序）。
///
/// 返回:
/// - 推断出的 persona UID。
pub fn infer_target_persona_from_messages(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .find(|m| m.role == MessageRole::Assistant && m.persona_uid.is_some())
        .and_then(|m| m.persona_uid.clone())
}

/// 消息是否为可切分的对话消息（系统/工具消息不进入原文块）。
pub fn is_chat_message(msg: &Message) -> bool {
    !matches!(msg.role, MessageRole::System | MessageRole::Tool)
}

// =========================================================
// embedding f32 小端 BLOB 编解码
// =========================================================

/// 将 f32 向量编码为小端 BLOB（与 `utt_blocks.embedding` 列约定一致）。
///
/// 返回:
/// - `Vec<u8>`：长度恒为 `vec.len() * 4`。
pub fn encode_embedding(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for v in vec {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// 将 f32 小端 BLOB 解码为向量。
///
/// 错误:
/// - BLOB 长度不是 4 的倍数 → `Validation` 错误（数据损坏防御）。
pub fn decode_embedding(blob: &[u8]) -> RamariaResult<Vec<f32>> {
    if !blob.len().is_multiple_of(4) {
        return Err(RamariaError::validation(format!(
            "embedding BLOB 长度 {} 不是 4 的倍数（数据损坏）",
            blob.len()
        )));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: MessageRole, persona_uid: Option<&str>, _created_at: i64) -> Message {
        Message::new(
            uuid::Uuid::new_v4(),
            role,
            "内容".to_string(),
            ramaria_core::types::MessageSource::Local,
        )
        .with_persona_uid(persona_uid.map(|s| s.to_string()))
    }

    #[test]
    fn encode_decode_roundtrip() {
        let vec = vec![0.1_f32, -0.5, 3.25, 0.0];
        let blob = encode_embedding(&vec);
        assert_eq!(blob.len(), 16);
        let back = decode_embedding(&blob).unwrap();
        assert_eq!(back, vec);
    }

    #[test]
    fn decode_empty_blob_is_empty_vector() {
        let back = decode_embedding(&[]).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn decode_invalid_len_returns_validation_error() {
        let err = decode_embedding(&[1, 2, 3]).unwrap_err();
        assert_eq!(err.category(), "validation");
    }

    #[test]
    fn is_target_speech_matches_uid() {
        let m = msg(MessageRole::Assistant, Some("char-0001"), 1000);
        assert!(is_target_speech(&m, Some("char-0001")));
        assert!(!is_target_speech(&m, Some("char-0002")));
        assert!(!is_target_speech(&m, None));
    }

    #[test]
    fn is_target_speech_none_target_means_no_persona_uid() {
        let m = msg(MessageRole::Assistant, None, 1000);
        assert!(is_target_speech(&m, None));
        assert!(!is_target_speech(&m, Some("rama-0001")));
    }

    #[test]
    fn is_target_speech_user_message_never_target() {
        let m = msg(MessageRole::User, Some("char-0001"), 1000);
        assert!(!is_target_speech(&m, Some("char-0001")));
    }

    #[test]
    fn is_target_speech_system_tool_never_speech() {
        let sys = msg(MessageRole::System, None, 1000);
        let tool = msg(MessageRole::Tool, None, 1000);
        assert!(!is_target_speech(&sys, None));
        assert!(!is_target_speech(&tool, None));
        assert!(!is_chat_message(&sys));
        assert!(!is_chat_message(&tool));
        assert!(is_chat_message(&msg(MessageRole::User, None, 1000)));
    }

    #[test]
    fn splitter_config_defaults() {
        let c = UttSplitterConfig::default();
        assert_eq!(c.theta_gap_minutes, 10);
        assert_eq!(c.max_msgs_per_block, 80);
    }

    // ---- P0-2：NULL 会话目标 persona 推断 ----

    #[test]
    fn infer_target_persona_finds_first_assistant() {
        let user = msg(MessageRole::User, None, 1000);
        let assistant = msg(MessageRole::Assistant, Some("char-0001"), 2000);
        let assistant2 = msg(MessageRole::Assistant, Some("char-0002"), 3000);
        let inferred = infer_target_persona_from_messages(&[user, assistant, assistant2]);
        assert_eq!(inferred.as_deref(), Some("char-0001"));
    }

    #[test]
    fn infer_target_persona_none_without_assistant() {
        let user = msg(MessageRole::User, None, 1000);
        let sys = msg(MessageRole::System, None, 2000);
        assert_eq!(infer_target_persona_from_messages(&[user, sys]), None);
    }

    #[test]
    fn infer_target_persona_skips_null_assistant() {
        // assistant 无 persona_uid（rama 自身会话）不参与推断
        let assistant_null = msg(MessageRole::Assistant, None, 1000);
        let assistant_uid = msg(MessageRole::Assistant, Some("char-0009"), 2000);
        let inferred = infer_target_persona_from_messages(&[assistant_null, assistant_uid]);
        assert_eq!(inferred.as_deref(), Some("char-0009"));
    }
}
