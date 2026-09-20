//! crates/ramaria-mcp/src/result.rs - 工具结果与错误构造
//!
//! 设计特点:
//! - 成功结果：JSON 文本 + 结构化内容（模型既能直接读文本，也能按字段解析）
//! - 失败结果：结果内 `isError = true` + 可操作描述（不使用协议级错误，模型能看到原因）
//! - 隐私：错误描述只含静态提示、人格 uid 与错误分类信息，不含对话原文
//! - 序列化失败不 panic：降级为纯文本错误结果并记 error 日志

use rmcp::model::CallToolResult;
use serde::Serialize;

/// 构造成功结果（结构化内容 + 同内容的 JSON 文本）。
///
/// 参数:
/// - `value`: 已实现 `Serialize` 的返回值（服务层视图类型）。
///
/// 返回:
/// - `CallToolResult`：`content` 为紧凑 JSON 文本，`structured_content` 为同一 JSON 值。
pub(crate) fn success<T: Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(json) => CallToolResult::structured(json),
        Err(e) => {
            // 自身返回值序列化失败属实现缺陷：记 error 并返回可读错误，不让调用方看到 panic
            tracing::error!(error = %e, "工具返回值序列化失败");
            tool_error(format!("结果序列化失败：{e}"))
        }
    }
}

/// 构造成功结果（直接给定 JSON 值，供需要在服务层视图上追加说明字段的场景）。
pub(crate) fn success_value(value: serde_json::Value) -> CallToolResult {
    CallToolResult::structured(value)
}

/// 构造工具级错误结果（`isError = true`，内容对模型可读可操作）。
///
/// 说明:
/// - 消息应是"下一步怎么做"的口径（例如提示去桌面开关、补参数），不含对话原文；
/// - 记 debug 日志便于排查（不记 error：可预期的用户/模型侧错误不算服务故障）。
pub(crate) fn tool_error(message: impl Into<String>) -> CallToolResult {
    let message = message.into();
    tracing::debug!(message = %message, "工具返回可操作错误");
    CallToolResult::structured_error(serde_json::json!({ "error": message }))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 成功结果：文本与结构化内容一致，且显式标注为非错误。
    #[test]
    fn success_packs_structured_and_text() {
        let result = success(&serde_json::json!({ "context": "记忆", "items": [] }));
        // SDK 的成功构造器显式写 isError=false（客户端据此区分错误结果）
        assert_eq!(result.is_error, Some(false), "成功结果应标注 isError=false");
        assert_eq!(result.content.len(), 1, "文本内容应有一块");
        assert!(result.structured_content.is_some(), "应有结构化内容");
        let text = result.content[0]
            .as_text()
            .map(|t| t.text.clone())
            .expect("应为文本块");
        assert!(text.contains("\"context\""), "文本应为 JSON：{text}");
    }

    /// 错误结果：isError=true 且消息可读。
    #[test]
    fn tool_error_marks_is_error() {
        let result = tool_error("MCP 接入未开启：请在桌面设置中打开");
        assert_eq!(result.is_error, Some(true), "错误结果必须带 isError");
        let value = result
            .structured_content
            .clone()
            .expect("错误也应有结构化内容");
        assert_eq!(
            value.get("error").and_then(|v| v.as_str()),
            Some("MCP 接入未开启：请在桌面设置中打开")
        );
    }
}
