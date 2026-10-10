//! crates/ramaria-channel/src/transport/telegram/api.rs - Telegram 出站 API 与应答分类模块
//!
//! 设计特点:
//! - 请求构建为纯函数：会话标识 → 聊天 ID、长文本 → 分片、方法 → 请求体
//! - 分片按 UTF-16 单元计数（与平台上限口径一致），不得切在代理对中间；
//!   超限优先在换行后断开，无换行则硬切
//! - 应答分类统一：`ok:true` 为成功取 `result`；401 / 403 为认证类（权限拒绝不可重试），
//!   其余错误码为协议错误；JSON 解析失败按 HTTP 状态兜底
//! - 隐私：令牌位于请求路径，错误文案与日志不含令牌与完整地址
//!   （请求错误一律折叠为固定分类文案，不透出底层错误渲染）

use std::time::Duration;

use ramaria_core::privacy::mask_id;
use serde_json::{Map, Value, json};

use super::BotIdentity;
use crate::{ChannelError, ChannelResult};

// =========================================================
// 常量
// =========================================================

/// 单条消息文本上限（UTF-16 单元数；平台口径）。
pub(crate) const MESSAGE_LIMIT: usize = 4096;

/// `getFile` 应答（文件路径 + 可选体积）。
pub(crate) struct GetFileInfo {
    pub(crate) file_path: String,
    pub(crate) file_size: Option<u64>,
}

// =========================================================
// 会话标识解析
// =========================================================

/// 会话外部标识 → 聊天 ID。
///
/// 规则:
/// - `c2c:{id}` → 私聊聊天 ID（正整数）；
/// - `group:{id}` → 群聊聊天 ID（负数原样）；
/// - 空标识 / 未知前缀 / 非数字 → 协议错误。
///
/// 参数:
/// - `external_ref`: 会话外部标识（两侧空白容忍）。
pub(crate) fn parse_chat_id(external_ref: &str) -> ChannelResult<i64> {
    let trimmed = external_ref.trim();
    if trimmed.is_empty() {
        return Err(ChannelError::Protocol("会话标识为空".to_string()));
    }
    if let Some(value) = trimmed.strip_prefix("c2c:") {
        let value = value.trim();
        return value.parse().map_err(|_| {
            ChannelError::Protocol(format!(
                "非法会话标识（聊天 ID 非数字）: {}",
                mask_id(value)
            ))
        });
    }
    if let Some(value) = trimmed.strip_prefix("group:") {
        let value = value.trim();
        return value.parse().map_err(|_| {
            ChannelError::Protocol(format!("非法会话标识（群 ID 非数字）: {}", mask_id(value)))
        });
    }
    Err(ChannelError::Protocol(format!(
        "未知会话标识格式: {}",
        mask_id(trimmed)
    )))
}

// =========================================================
// 文本分片
// =========================================================

/// 按 UTF-16 单元上限切分文本（每片非空；空文本返回空列表）。
///
/// 规则:
/// - 累计 `char::len_utf16`（非 BMP 字符计 2 单元），不切在代理对中间；
/// - 超限时优先在最近的换行之后断开（前片包含换行）；无换行则硬切；
/// - 单字符超过上限时该字符独占一片（保证不撕裂字符）。
///
/// 参数:
/// - `text`: 原始文本。
/// - `limit`: 单片 UTF-16 单元上限（0 按 1 处理）。
pub(crate) fn split_message(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut units = 0usize;
        let mut cut = rest.len();
        let mut last_newline_end: Option<usize> = None;
        for (index, ch) in rest.char_indices() {
            if units + ch.len_utf16() > limit {
                cut = match last_newline_end {
                    Some(newline_end) => newline_end,
                    None if index > 0 => index,
                    // 首个字符即超限：该字符独占一片（不撕裂字符，防死循环）
                    None => index + ch.len_utf8(),
                };
                break;
            }
            units += ch.len_utf16();
            if ch == '\n' {
                last_newline_end = Some(index + ch.len_utf8());
            }
        }
        chunks.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    chunks
}

// =========================================================
// 请求构建（纯函数）
// =========================================================

/// 构建 `getUpdates` 请求体（首轮不带 `offset`；只订阅 message 类更新）。
pub(crate) fn build_get_updates_body(offset: Option<i64>, poll_timeout_secs: u64) -> Value {
    let mut body = Map::new();
    if let Some(offset) = offset {
        body.insert("offset".to_string(), json!(offset));
    }
    body.insert("timeout".to_string(), json!(poll_timeout_secs));
    body.insert("allowed_updates".to_string(), json!(["message"]));
    Value::Object(body)
}

/// 构建 `sendMessage` 请求体（文本消息）。
pub(crate) fn build_send_message_body(chat_id: i64, text: &str) -> Value {
    json!({ "chat_id": chat_id, "text": text })
}

/// 构建 `getFile` 请求体（入参为文件标识）。
pub(crate) fn build_get_file_body(file_id: &str) -> Value {
    json!({ "file_id": file_id })
}

/// 方法调用地址（`{base}/bot{token}/{method}`）。
pub(crate) fn api_url(base_url: &str, token: &str, method: &str) -> String {
    format!("{}/bot{}/{}", base_url.trim_end_matches('/'), token, method)
}

/// 媒体下载地址（`{base}/file/bot{token}/{file_path}`）。
pub(crate) fn download_url(base_url: &str, token: &str, file_path: &str) -> String {
    format!(
        "{}/file/bot{}/{}",
        base_url.trim_end_matches('/'),
        token,
        file_path.trim_start_matches('/')
    )
}

// =========================================================
// 应答分类
// =========================================================

/// 分类一条 Bot API 应答，成功时返回 `result` 字段。
///
/// 规则:
/// - `ok:true` → 成功（`result` 缺失时为 [`Value::Null`]）；
/// - `ok:false`：`error_code` 为 401 / 403 → 认证类错误（凭据无效 /
///   权限被拒，重试无意义，不可重试）；
/// - `ok:false` 其余错误码（400 / 429 等）→ 协议错误（可由调用方退避重试）；
/// - 缺少 `ok` 字段 → 协议错误。
pub(crate) fn parse_api_response(value: &Value) -> ChannelResult<Value> {
    let Some(ok) = value.get("ok").and_then(Value::as_bool) else {
        return Err(ChannelError::Protocol(
            "Telegram 应答缺少 ok 字段".to_string(),
        ));
    };
    if ok {
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
    let code = value
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    if code == 401 || code == 403 {
        return Err(ChannelError::Auth(format!(
            "Telegram 拒绝请求: error_code={code} {description}"
        )));
    }
    Err(ChannelError::Protocol(format!(
        "Telegram 调用失败: error_code={code} {description}"
    )))
}

/// 从 `sendMessage` 的 `result` 中提取平台消息 ID（缺失时为 None）。
pub(crate) fn extract_message_id(result: &Value) -> Option<String> {
    result.get("message_id").and_then(value_to_string)
}

/// 归一 JSON 取值为字符串：数字转十进制串、字符串原样；其余为 None。
pub(crate) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 归一 JSON 取值为 u64：数字原样，字符串尝试解析；其余为 None。
pub(crate) fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

// =========================================================
// HTTP 执行
// =========================================================

/// 请求错误分类（文案不含地址与凭据，不透出底层错误渲染）。
pub(crate) fn classify_request_error(error: reqwest::Error) -> ChannelError {
    let reason = if error.is_timeout() {
        "请求超时"
    } else if error.is_connect() {
        "连接失败"
    } else {
        "网络错误"
    };
    if error.is_timeout() {
        ChannelError::Timeout(format!("Telegram {reason}"))
    } else {
        ChannelError::Protocol(format!("Telegram {reason}"))
    }
}

/// 执行一次 Bot API POST 调用并分类应答。
///
/// 语义:
/// - 请求 / 读取失败按网络类分类（超时 → `Timeout`，其余 → `Protocol`）；
/// - JSON 解析失败按 HTTP 状态兜底：401 / 403 → `Auth`，其余 → `Protocol`。
pub(crate) async fn post_json(
    client: &reqwest::Client,
    token: &str,
    base_url: &str,
    method: &str,
    body: &Value,
    timeout: Duration,
) -> ChannelResult<Value> {
    let response = client
        .post(api_url(base_url, token, method))
        .timeout(timeout)
        .json(body)
        .send()
        .await
        .map_err(classify_request_error)?;
    let http_status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|_| ChannelError::Protocol("Telegram 应答读取失败".to_string()))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => parse_api_response(&value),
        Err(_) => {
            if http_status == 401 || http_status == 403 {
                Err(ChannelError::Auth(format!(
                    "Telegram 请求被拒绝（HTTP {http_status}）"
                )))
            } else {
                Err(ChannelError::Protocol(format!(
                    "Telegram 应答解析失败（HTTP {http_status}）"
                )))
            }
        }
    }
}

/// 调用 `getMe` 取得 Bot 身份。
pub(crate) async fn get_me(
    client: &reqwest::Client,
    token: &str,
    base_url: &str,
    timeout: Duration,
) -> ChannelResult<BotIdentity> {
    let result = post_json(client, token, base_url, "getMe", &json!({}), timeout).await?;
    let id = result
        .get("id")
        .and_then(value_to_string)
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() {
        return Err(ChannelError::Protocol(
            "Telegram getMe 应答缺少身份标识".to_string(),
        ));
    }
    let username = result
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(BotIdentity { id, username })
}

/// 调用 `getUpdates` 拉取一批更新（`offset` 为 None 时首轮拉取）。
pub(crate) async fn get_updates(
    client: &reqwest::Client,
    token: &str,
    base_url: &str,
    offset: Option<i64>,
    poll_timeout_secs: u64,
    timeout: Duration,
) -> ChannelResult<Vec<Value>> {
    let body = build_get_updates_body(offset, poll_timeout_secs);
    let result = post_json(client, token, base_url, "getUpdates", &body, timeout).await?;
    match result.as_array() {
        Some(items) => Ok(items.clone()),
        None => Err(ChannelError::Protocol(
            "Telegram getUpdates 应答格式异常".to_string(),
        )),
    }
}

/// 调用 `sendMessage` 发送一段文本，返回平台消息 ID（应答缺失时为 None）。
pub(crate) async fn send_message(
    client: &reqwest::Client,
    token: &str,
    base_url: &str,
    chat_id: i64,
    text: &str,
    timeout: Duration,
) -> ChannelResult<Option<String>> {
    let body = build_send_message_body(chat_id, text);
    let result = post_json(client, token, base_url, "sendMessage", &body, timeout).await?;
    Ok(extract_message_id(&result))
}

/// 调用 `getFile` 取得文件路径与可选体积。
pub(crate) async fn get_file(
    client: &reqwest::Client,
    token: &str,
    base_url: &str,
    file_id: &str,
    timeout: Duration,
) -> ChannelResult<GetFileInfo> {
    let body = build_get_file_body(file_id);
    let result = post_json(client, token, base_url, "getFile", &body, timeout).await?;
    let file_path = result
        .get("file_path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if file_path.is_empty() {
        return Err(ChannelError::Protocol(
            "Telegram getFile 应答缺少文件路径".to_string(),
        ));
    }
    let file_size = result.get("file_size").and_then(value_as_u64);
    Ok(GetFileInfo {
        file_path,
        file_size,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
