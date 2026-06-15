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

// =========================================================
// 导入进度事件（v1.1）
// =========================================================

/// 导入深度处理进度事件负载。
///
/// 职责:
/// - 携带导入后 L1/L2/L3 管线处理的实时进度和最终统计。
/// - 在 `done` 阶段携带 `l1_success`/`l1_failed` 计数，
///   前端据此决定是否展示 L1 失败警告和"深度处理"引导入口。
///
/// 字段约定:
/// - `phase`: "l1" | "l2" | "l3" | "done"
/// - `current` / `total`: 进度计数；done 阶段为最终统计
/// - `l1_success` / `l1_failed`: 仅 done 阶段有意义，其余阶段为 None
/// - `l2_triggered` / `l3_triggered`: 仅 done 阶段有意义，标记深度模式级联是否已触发
#[derive(Debug, Clone, Serialize)]
pub struct ImportProgressPayload {
    /// 阶段: "l1" | "l2" | "l3" | "done"
    pub phase: String,
    /// 当前进度（已处理数）
    pub current: usize,
    /// 总数（0 表示未知）
    pub total: usize,
    /// 人类可读的阶段描述
    pub message: String,
    /// L1 摘要生成成功数（仅 done 阶段有意义）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l1_success: Option<usize>,
    /// L1 摘要生成失败数（仅 done 阶段有意义）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l1_failed: Option<usize>,
    /// 深度模式：L2 是否已触发（仅 done 阶段有意义）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l2_triggered: Option<bool>,
    /// 深度模式：L3 是否已触发（仅 done 阶段有意义）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l3_triggered: Option<bool>,
}

impl ImportProgressPayload {
    /// 创建基础进度事件负载（无统计字段）。
    pub fn new(phase: &str, current: usize, total: usize, message: &str) -> Self {
        Self {
            phase: phase.to_string(),
            current,
            total,
            message: message.to_string(),
            l1_success: None,
            l1_failed: None,
            l2_triggered: None,
            l3_triggered: None,
        }
    }

    /// 创建带完整统计的 done 阶段事件负载。
    ///
    /// 参数:
    /// - `l1_success`: L1 摘要生成成功数
    /// - `l1_failed`: L1 摘要生成失败数
    /// - `l2_triggered`: 深度模式下 L2 是否已触发
    /// - `l3_triggered`: 深度模式下 L3 是否已触发
    /// - `total_sessions`: 导入的 session 总数
    /// - `message`: 人类可读的完成消息
    pub fn done_with_stats(
        l1_success: usize,
        l1_failed: usize,
        l2_triggered: bool,
        l3_triggered: bool,
        total_sessions: usize,
        message: &str,
    ) -> Self {
        Self {
            phase: "done".to_string(),
            current: l1_success + l1_failed,
            total: total_sessions,
            message: message.to_string(),
            l1_success: Some(l1_success),
            l1_failed: Some(l1_failed),
            l2_triggered: Some(l2_triggered),
            l3_triggered: Some(l3_triggered),
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
}
