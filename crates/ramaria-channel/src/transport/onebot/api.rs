//! crates/ramaria-channel/src/transport/onebot/api.rs - OneBot 出站 API 与应答配对模块
//!
//! 设计特点:
//! - 请求—响应经 `echo` 字段配对：每次出站注册一次性应答通道，超时或断连即失败
//! - 本机已发消息 ID 小表：容量受限、先进先出，供「被回复判定」使用
//! - 请求构建为纯函数：会话标识 → API 动作与参数，非法标识显式报错
//! - 应答分类统一：`retcode == 0` 为成功，其余为协议错误；重试策略由调用方决定
//! - 错误文本中的外部标识经脱敏，不携带完整账号

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard};

use ramaria_core::privacy::mask_id;
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::{ChannelError, ChannelResult};

// =========================================================
// 常量
// =========================================================

/// 本机已发消息 ID 缓存容量（超出淘汰最旧）。
pub const SENT_MESSAGE_CACHE: usize = 500;

/// 出站动作：私聊消息。
pub(crate) const ACTION_SEND_PRIVATE: &str = "send_private_msg";

/// 出站动作：群聊消息。
pub(crate) const ACTION_SEND_GROUP: &str = "send_group_msg";

// =========================================================
// 本机已发消息 ID 缓存
// =========================================================

/// 本机已发消息 ID 缓存（容量受限，先进先出）。
///
/// 职责:
/// - 记录本通道成功发出的平台消息 ID，供「被回复判定」识别「回复本机消息」；
/// - 超出 [`SENT_MESSAGE_CACHE`] 容量时淘汰最旧记录。
///
/// 并发约定:
/// - 内部 `Mutex` 仅做集合读写，不跨 await 持锁。
#[derive(Debug, Default)]
pub struct SentMessageCache {
    inner: Mutex<SentCacheInner>,
}

#[derive(Debug, Default)]
struct SentCacheInner {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl SentMessageCache {
    /// 创建空缓存。
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一条已发出的平台消息 ID（重复记录不重复入队）。
    pub fn insert(&self, message_id: impl Into<String>) {
        let message_id = message_id.into();
        if message_id.is_empty() {
            return;
        }
        let mut guard = lock(&self.inner);
        if !guard.set.insert(message_id.clone()) {
            return;
        }
        guard.order.push_back(message_id);
        while guard.order.len() > SENT_MESSAGE_CACHE {
            let Some(oldest) = guard.order.pop_front() else {
                break;
            };
            guard.set.remove(&oldest);
        }
    }

    /// 判定消息 ID 是否在缓存内。
    pub fn contains(&self, message_id: &str) -> bool {
        lock(&self.inner).set.contains(message_id)
    }
}

// =========================================================
// 应答配对
// =========================================================

/// 待应答表（`echo` → 一次性应答通道）。
///
/// 职责:
/// - 出站调用注册后等待应答；应答到达或连接断开时清理。
#[derive(Debug, Default)]
pub(crate) struct PendingCalls {
    inner: Mutex<HashMap<String, oneshot::Sender<Value>>>,
}

impl PendingCalls {
    /// 注册一次待应答（返回接收端）。
    pub(crate) fn register(&self, echo: String) -> oneshot::Receiver<Value> {
        let (tx, rx) = oneshot::channel();
        lock(&self.inner).insert(echo, tx);
        rx
    }

    /// 应答到达：配对成功返回 true（未找到对应注册返回 false）。
    pub(crate) fn resolve(&self, echo: &str, value: Value) -> bool {
        let sender = lock(&self.inner).remove(echo);
        match sender {
            Some(tx) => tx.send(value).is_ok(),
            None => false,
        }
    }

    /// 移除一条待应答（超时 / 发送失败时清理）。
    pub(crate) fn remove(&self, echo: &str) {
        lock(&self.inner).remove(echo);
    }

    /// 清空全部待应答（连接断开时调用；接收端收到关闭信号）。
    pub(crate) fn clear(&self) {
        lock(&self.inner).clear();
    }
}

// =========================================================
// 请求构建（纯函数）
// =========================================================

/// 会话外部标识 → 出站动作与基础参数。
///
/// 规则:
/// - `c2c:{uin}` → `send_private_msg`，参数 `user_id`（数字账号）；
/// - `group:{gid}` → `send_group_msg`，参数 `group_id`（数字群号）；
/// - 空标识 / 未知前缀 / 非数字账号 → 协议错误。
///
/// 参数:
/// - `external_ref`: 会话外部标识（两侧空白容忍）。
///
/// 返回:
/// - 动作名与基础参数表（不含 `message` 段与 `echo`）。
pub(crate) fn action_and_params(
    external_ref: &str,
) -> ChannelResult<(&'static str, Map<String, Value>)> {
    let trimmed = external_ref.trim();
    if trimmed.is_empty() {
        return Err(ChannelError::Protocol("会话标识为空".to_string()));
    }
    if let Some(uin) = trimmed.strip_prefix("c2c:") {
        let uin = uin.trim();
        let account: i64 = uin.parse().map_err(|_| {
            ChannelError::Protocol(format!("非法会话标识（账号非数字）: {}", mask_id(uin)))
        })?;
        let mut params = Map::new();
        params.insert("user_id".to_string(), json!(account));
        return Ok((ACTION_SEND_PRIVATE, params));
    }
    if let Some(gid) = trimmed.strip_prefix("group:") {
        let gid = gid.trim();
        let group: i64 = gid.parse().map_err(|_| {
            ChannelError::Protocol(format!("非法会话标识（群号非数字）: {}", mask_id(gid)))
        })?;
        let mut params = Map::new();
        params.insert("group_id".to_string(), json!(group));
        return Ok((ACTION_SEND_GROUP, params));
    }
    Err(ChannelError::Protocol(format!(
        "未知会话标识格式: {}",
        mask_id(trimmed)
    )))
}

/// 构建一条文本出站请求体（含 `message` 段与 `echo`）。
pub(crate) fn build_text_request(
    external_ref: &str,
    text: &str,
    echo: &str,
) -> ChannelResult<Value> {
    let (action, mut params) = action_and_params(external_ref)?;
    params.insert(
        "message".to_string(),
        json!([{ "type": "text", "data": { "text": text } }]),
    );
    Ok(json!({
        "action": action,
        "params": params,
        "echo": echo,
    }))
}

/// 归一 JSON 取值为字符串：数字转十进制串、字符串原样；其余为 None。
pub(crate) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

// =========================================================
// 应答分类
// =========================================================

/// 分类一条出站应答，成功时返回平台消息 ID（平台未提供则为 None）。
///
/// 规则:
/// - `retcode == 0` 为成功（`status` 非 `ok` 时宽容按成功处理）；
/// - `data.message_id` 存在时归一为字符串返回，供已发消息 ID 缓存记录；
/// - `retcode` 缺失或非 0 → 协议错误（含 retcode 与服务端文案）。
pub(crate) fn classify_response(value: &Value) -> ChannelResult<Option<String>> {
    let Some(retcode) = value.get("retcode").and_then(Value::as_i64) else {
        return Err(ChannelError::Protocol(
            "OneBot 应答缺少 retcode 字段".to_string(),
        ));
    };
    if retcode != 0 {
        let wording = value.get("wording").and_then(Value::as_str).unwrap_or("");
        let message = value.get("message").and_then(Value::as_str).unwrap_or("");
        return Err(ChannelError::Protocol(format!(
            "OneBot 调用失败: retcode={retcode} {wording}{message}"
        )));
    }
    let message_id = value
        .get("data")
        .and_then(|data| data.get("message_id"))
        .and_then(value_to_string);
    Ok(message_id)
}

// =========================================================
// 内部辅助
// =========================================================

/// 获取互斥锁（中毒时取回内部数据，保持通道可用）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
