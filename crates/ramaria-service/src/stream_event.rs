//! crates/ramaria-service/src/stream_event.rs - 流式事件领域模型
//!
//! 设计特点:
//! - 把 LLM 流式响应封装为领域事件：Delta（增量文本）/ Done（流结束）/ Error（流中错误）
//! - 每个事件携带 request_id 与 created_at，消费方按请求串联事件、按序渲染与追踪
//! - 实现 Serialize（internally tagged `type` 字段）：事件流可直接 JSON 序列化消费
//! - Done 事件携带会话定位（session_id）与后端标识，供消费方聚合输出与会话追踪
//! - 与 `ramaria_core::traits::StreamDelta` 互补：StreamDelta 是 provider 层协议，
//!   本模块是面向宿主的领域事件（增加 request_id / created_at / 语义化错误）
//! - `ChatStreamHandle` 把事件流与会话归属打包返回，宿主据此维护活跃指针与事件桥

use ramaria_core::types::now_ms;
use serde::Serialize;
use uuid::Uuid;

// =========================================================
// StreamEvent 枚举
// =========================================================

/// 流式对话的领域事件。
///
/// 职责:
/// - 将 LLM provider 的原始 `StreamDelta` 转换为宿主友好的事件
/// - 统一流式增量、完成通知和错误三种场景
/// - 每个事件独立携带时间戳，支持消费方按序渲染
///
/// 变体:
/// - `Delta`: LLM 输出的增量文本片段
/// - `Done`: 流式输出完成信号（含总字符数、session_id 和 provider 元数据）
/// - `Error`: 流式输出中的错误（上层可选择重试或显示）
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamEvent {
    /// LLM 增量文本输出。
    Delta {
        /// 当前请求唯一标识
        request_id: Uuid,
        /// 增量文本内容
        content: String,
        /// 事件生成时间（Unix 毫秒）
        created_at: i64,
    },

    /// 流式输出完成。
    Done {
        /// 当前请求唯一标识
        request_id: Uuid,
        /// 本次回复所属会话 ID（None 表示不可用/未创建）
        session_id: Option<Uuid>,
        /// provider 返回的后端标识（如 finish_reason: "stop"）
        backend_id: Option<String>,
        /// 本次回复总字符数
        total_chars: usize,
        /// 事件生成时间（Unix 毫秒）
        created_at: i64,
    },

    /// 流式输出中的错误。
    Error {
        /// 当前请求唯一标识
        request_id: Uuid,
        /// 面向用户的错误提示
        error: String,
        /// 事件生成时间（Unix 毫秒）
        created_at: i64,
    },
}

impl StreamEvent {
    /// 创建 Delta 事件。
    ///
    /// 参数:
    /// - `request_id`: 当前请求 ID。
    /// - `content`: LLM 增量文本。
    pub fn delta(request_id: Uuid, content: String) -> Self {
        Self::Delta {
            request_id,
            content,
            created_at: now_ms(),
        }
    }

    /// 创建 Done 事件。
    ///
    /// 参数:
    /// - `request_id`: 当前请求 ID。
    /// - `session_id`: 本次回复所属会话 ID（供消费方聚合输出）。
    /// - `backend_id`: provider 返回的 finish_reason 或后端标识。
    /// - `total_chars`: 累计输出字符数。
    pub fn done(
        request_id: Uuid,
        session_id: Option<Uuid>,
        backend_id: Option<String>,
        total_chars: usize,
    ) -> Self {
        Self::Done {
            request_id,
            session_id,
            backend_id,
            total_chars,
            created_at: now_ms(),
        }
    }

    /// 创建 Error 事件。
    ///
    /// 参数:
    /// - `request_id`: 当前请求 ID。
    /// - `error`: 面向用户的错误消息。
    pub fn error(request_id: Uuid, error: String) -> Self {
        Self::Error {
            request_id,
            error,
            created_at: now_ms(),
        }
    }

    /// 返回事件类型标签（用于日志/消费方路由）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Delta { .. } => "delta",
            Self::Done { .. } => "done",
            Self::Error { .. } => "error",
        }
    }

    /// 返回 request_id。
    pub fn request_id(&self) -> Uuid {
        match self {
            Self::Delta { request_id, .. }
            | Self::Done { request_id, .. }
            | Self::Error { request_id, .. } => *request_id,
        }
    }
}

// =========================================================
// 事件流句柄
// =========================================================

/// 流式生成的事件流类型（增量消费；错误以流内 `Err` 项表达）。
pub type ChatEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = ramaria_core::error::RamariaResult<StreamEvent>> + Send>,
>;

/// 流式生成句柄（会话归属 + 增量事件流）。
///
/// 职责:
/// - 承载一次流式生成的会话定位与增量事件流，供交互入口消费。
///
/// 字段约定:
/// - `request_id`: 本次请求唯一标识（与事件内的 `request_id` 一致）；
/// - `session_id`: 本轮回复所属会话（宿主据此维护活跃指针与事件桥目标）；
/// - `events`: 增量事件流（Delta… → Done / Error；宿主逐项消费，用户消息已先行落库）。
pub struct ChatStreamHandle {
    pub request_id: Uuid,
    pub session_id: Uuid,
    pub events: ChatEventStream,
}

impl std::fmt::Debug for ChatStreamHandle {
    /// 调试输出：事件流不可打印（trait object），只展示会话定位字段。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatStreamHandle")
            .field("request_id", &self.request_id)
            .field("session_id", &self.session_id)
            .field("events", &"<event stream>")
            .finish()
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_event() {
        let id = Uuid::new_v4();
        let event = StreamEvent::delta(id, "你好".into());
        assert_eq!(event.kind(), "delta");
        assert_eq!(event.request_id(), id);
        match event {
            StreamEvent::Delta { content, .. } => assert_eq!(content, "你好"),
            _ => panic!("应为 Delta"),
        }
    }

    #[test]
    fn done_event() {
        let id = Uuid::new_v4();
        let sid = Uuid::new_v4();
        let event = StreamEvent::done(id, Some(sid), Some("stop".into()), 42);
        assert_eq!(event.kind(), "done");
        match event {
            StreamEvent::Done {
                session_id,
                backend_id,
                total_chars,
                ..
            } => {
                assert_eq!(session_id, Some(sid));
                assert_eq!(backend_id.as_deref(), Some("stop"));
                assert_eq!(total_chars, 42);
            }
            _ => panic!("应为 Done"),
        }
    }

    #[test]
    fn error_event() {
        let id = Uuid::new_v4();
        let event = StreamEvent::error(id, "连接超时".into());
        assert_eq!(event.kind(), "error");
        match event {
            StreamEvent::Error { error, .. } => assert_eq!(error, "连接超时"),
            _ => panic!("应为 Error"),
        }
    }

    #[test]
    fn event_has_created_at() {
        let event = StreamEvent::delta(Uuid::new_v4(), "test".into());
        match event {
            StreamEvent::Delta { created_at, .. } => assert!(created_at > 0),
            _ => panic!("应为 Delta"),
        }
    }

    #[test]
    fn serialize_delta_event() {
        let id = Uuid::new_v4();
        let event = StreamEvent::delta(id, "你好".into());
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "delta");
        assert_eq!(parsed["request_id"], id.to_string());
        assert_eq!(parsed["content"], "你好");
        assert!(parsed["created_at"].is_number());
    }

    #[test]
    fn serialize_done_event_includes_session_id() {
        let id = Uuid::new_v4();
        let sid = Uuid::new_v4();
        let event = StreamEvent::done(id, Some(sid), Some("stop".into()), 42);
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "done");
        assert_eq!(parsed["session_id"], sid.to_string());
        assert_eq!(parsed["backend_id"], "stop");
        assert_eq!(parsed["total_chars"], 42);
    }

    #[test]
    fn serialize_error_event() {
        let id = Uuid::new_v4();
        let event = StreamEvent::error(id, "连接超时".into());
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "error");
        assert_eq!(parsed["error"], "连接超时");
    }
}
