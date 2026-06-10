//! rust/crates/ramaria-app/src/stream_event.rs - 流式事件领域模型
//!
//! 设计特点:
//! - 封装 LLM 流式响应的领域事件，统一 CLI 和 Desktop 消费
//! - 三种事件: Delta（增量文本）、Done（流结束）、Error（流中错误）
//! - 每个事件携带 request_id 和 created_at，便于前端串联和日志追踪
//! - backend_id 记录 provider 返回的 finish_reason 或 error 类型
//! - 与 `ramaria_core::traits::StreamDelta` 互补：StreamDelta 是 provider 层协议，
//!   StreamEvent 是 app 层领域事件（增加 request_id/created_at/语义化错误）

use ramaria_core::types::now_ms;
use uuid::Uuid;

// =========================================================
// StreamEvent 枚举
// =========================================================

/// 流式对话的领域事件。
///
/// 职责:
/// - 将 LLM provider 的原始 `StreamDelta` 转换为 UI 友好的事件
/// - 统一流式增量、完成通知和错误三种场景
/// - 每个事件独立携带时间戳，支持前端按序渲染
///
/// 变体:
/// - `Delta`: LLM 输出的增量文本片段
/// - `Done`: 流式输出完成信号（含总字符数和 provider 元数据）
/// - `Error`: 流式输出中的可恢复错误（上层可选择重试或显示）
#[derive(Debug, Clone)]
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
    /// - `backend_id`: provider 返回的 finish_reason 或后端标识。
    /// - `total_chars`: 累计输出字符数。
    pub fn done(request_id: Uuid, backend_id: Option<String>, total_chars: usize) -> Self {
        Self::Done {
            request_id,
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

    /// 返回事件类型标签（用于日志/前端路由）。
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
        let event = StreamEvent::done(id, Some("stop".into()), 42);
        assert_eq!(event.kind(), "done");
        match event {
            StreamEvent::Done {
                backend_id,
                total_chars,
                ..
            } => {
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
}
