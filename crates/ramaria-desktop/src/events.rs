//! rust/crates/ramaria-desktop/src/events.rs - Tauri 事件负载类型定义
//!
//! 设计特点:
//! - 定义前端通过 Tauri Event 接收的所有事件负载结构
//! - 所有类型实现 Serialize，确保 JSON 序列化到前端一致
//! - 事件名称固定为字符串常量，前端和 Rust 端共享契约
//! - 聊天流式事件（Delta/Done/Error）对齐 ramaria_app::StreamEvent
//! - 应用状态变更事件独立于聊天事件，便于前端状态管理

use serde::Serialize;

// =========================================================
// 事件名称常量
// =========================================================

/// 聊天增量文本事件名
pub const EVENT_CHAT_DELTA: &str = "chat-delta";
/// 聊天完成事件名
pub const EVENT_CHAT_DONE: &str = "chat-done";
/// 聊天错误事件名
pub const EVENT_CHAT_ERROR: &str = "chat-error";
/// 关闭窗口确认事件名（前端弹窗后用户选择操作）
pub const EVENT_CLOSE_REQUESTED: &str = "close-requested";
/// 应用状态变更事件名（预留，Phase 5 后续批次启用）
#[allow(dead_code)]
pub const EVENT_APP_STATE: &str = "app-state-changed";
/// 导入进度事件名（v1.1）
pub const EVENT_IMPORT_PROGRESS: &str = "import-progress";

// =========================================================
// 聊天流式事件负载
// =========================================================

/// 聊天增量文本事件负载。
///
/// 职责:
/// - 携带 LLM 流式输出的单次增量文本片段
/// - 通过 request_id 关联前端发起的某次请求
///
/// 字段约定:
/// - `request_id`: 前端调用 send_message 时后端返回的唯一标识
/// - `content`: 本次增量文本（通常是几个 token）
#[derive(Debug, Clone, Serialize)]
pub struct ChatDeltaPayload {
    pub request_id: String,
    pub content: String,
}

/// 聊天完成事件负载。
///
/// 职责:
/// - 通知前端某次 LLM 请求已完成，所有增量文本已发送完毕
#[derive(Debug, Clone, Serialize)]
pub struct ChatDonePayload {
    pub request_id: String,
    /// LLM 后端标识（如 "deepseek-chat"）
    pub backend_id: Option<String>,
    /// 累计输出字符数
    pub total_chars: usize,
}

/// 聊天错误事件负载。
///
/// 职责:
/// - 携带 LLM 调用过程中发生的错误信息
/// - 包含用户友好的错误标题和详情，前端可直接展示
#[derive(Debug, Clone, Serialize)]
pub struct ChatErrorPayload {
    pub request_id: String,
    /// 错误标题（简短摘要）
    pub error_title: String,
    /// 错误详情（可含换行，前端按文本展示）
    pub error_detail: String,
    /// 此错误是否可重试（前端据此决定是否显示"重试"按钮）
    pub retryable: bool,
}

// =========================================================
// 应用状态事件负载
// =========================================================

/// 应用状态变更事件负载（预留，Phase 5 后续批次启用）。
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
pub struct AppStatePayload {
    /// 状态字符串（来自 AppState::as_str()，snake_case）：
    /// "needs_setup" | "downloading_model" | "indexing" | "ready" | "degraded" | "fatal_error"
    pub state: String,
}

// =========================================================
// 构造辅助函数
// =========================================================

impl ChatDeltaPayload {
    /// 创建聊天增量事件负载。
    pub fn new(request_id: String, content: String) -> Self {
        Self {
            request_id,
            content,
        }
    }
}

impl ChatDonePayload {
    /// 创建聊天完成事件负载。
    pub fn new(request_id: String, backend_id: Option<String>, total_chars: usize) -> Self {
        Self {
            request_id,
            backend_id,
            total_chars,
        }
    }
}

impl ChatErrorPayload {
    /// 创建聊天错误事件负载。
    pub fn new(
        request_id: String,
        error_title: String,
        error_detail: String,
        retryable: bool,
    ) -> Self {
        Self {
            request_id,
            error_title,
            error_detail,
            retryable,
        }
    }
}

#[allow(dead_code)]
impl AppStatePayload {
    /// 创建应用状态变更事件负载。
    pub fn new(state: String) -> Self {
        Self { state }
    }
}

// =========================================================
// 导入进度事件（v1.1）
// =========================================================

/// 导入深度处理进度事件负载。
#[derive(Debug, Clone, Serialize)]
pub struct ImportProgressPayload {
    /// 阶段: "l1" | "l2" | "l3" | "done"
    pub phase: String,
    /// 当前进度（已处理数）
    pub current: usize,
    /// 总数（-1 表示未知）
    pub total: usize,
    /// 人类可读的阶段描述
    pub message: String,
}

impl ImportProgressPayload {
    pub fn new(phase: &str, current: usize, total: usize, message: &str) -> Self {
        Self {
            phase: phase.to_string(),
            current,
            total,
            message: message.to_string(),
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
    fn delta_payload_serialization() {
        let payload = ChatDeltaPayload::new("req-001".to_string(), "你好".to_string());
        let json = serde_json::to_string(&payload).expect("序列化失败");
        assert!(json.contains("req-001"));
        assert!(json.contains("你好"));
    }

    #[test]
    fn done_payload_serialization() {
        let payload = ChatDonePayload::new("req-001".to_string(), Some("deepseek".into()), 42);
        let json = serde_json::to_string(&payload).expect("序列化失败");
        assert!(json.contains("deepseek"));
        assert!(json.contains("42"));
    }

    #[test]
    fn error_payload_serialization() {
        let payload = ChatErrorPayload::new(
            "req-001".into(),
            "连接失败".into(),
            "请检查网络".into(),
            true,
        );
        let json = serde_json::to_string(&payload).expect("序列化失败");
        assert!(json.contains("连接失败"));
        assert!(json.contains("请检查网络"));
        assert!(json.contains("true"));
    }

    #[test]
    fn app_state_payload_serialization() {
        let payload = AppStatePayload::new("Ready".to_string());
        let json = serde_json::to_string(&payload).expect("序列化失败");
        assert!(json.contains("Ready"));
    }
}
