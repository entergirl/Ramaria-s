//! crates/ramaria-channel/src/transport/telegram/manager.rs - Telegram 后台管理模块
//!
//! 设计特点:
//! - 管理任务单点：身份取得（首次已带则跳过）→ 长轮询循环 → 空批延迟 →
//!   网络类失败指数退避；关停信号随时可打断退避与轮询
//! - `offset` 确认制：一批处理（投递或忽略）完成后按最大 `update_id + 1` 推进，
//!   被忽略的更新同样推进（防死循环）
//! - 状态恢复：仅在「错误态之后重新成功」时回报 `Online`（避免重复上报噪声）
//! - 鉴权失败（401 / 403）终止轮询；网络类失败持续自愈
//! - 错误文本仅简短原因（不含令牌与完整地址）

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::oneshot;

use super::api;
use super::{BotIdentity, Inner, TelegramTransportOptions, emit_event, lock, update_status};
use crate::ChannelError;
use crate::config::TelegramRuntimeConfig;
use crate::events::ChannelEventSink;
use crate::status::ConnectionState;

// =========================================================
// 管理任务上下文
// =========================================================

/// 管理任务上下文（配置 / 参数 / 宿主回调 / 共享状态 / HTTP 客户端）。
pub(crate) struct ManagerContext {
    pub(crate) config: TelegramRuntimeConfig,
    pub(crate) options: TelegramTransportOptions,
    pub(crate) sink: Arc<dyn ChannelEventSink>,
    pub(crate) inner: Arc<std::sync::Mutex<Inner>>,
    pub(crate) client: reqwest::Client,
}

// =========================================================
// 管理任务
// =========================================================

/// 后台管理任务：取得身份后维持长轮询直至关停。
///
/// 流程:
/// - 首次已带身份 → 直接进入轮询；
/// - 否则按退避重试 `getMe`（成功后保存身份并回报 `Online`）；
/// - 轮询失败（非鉴权类）→ 状态 `Error` → 退避翻倍继续；
/// - 鉴权失败 → 状态 `Error` → 任务退出（凭据问题重试无意义）；
/// - 收到关停信号 → 任务退出（状态由 `shutdown` 置 `Disconnected`）。
pub(crate) async fn manage(
    context: ManagerContext,
    mut shutdown_rx: oneshot::Receiver<()>,
    initial: Option<BotIdentity>,
) {
    let Some(bot) = ensure_identity(&context, &mut shutdown_rx, initial).await else {
        return;
    };
    poll_loop(&context, &mut shutdown_rx, bot).await;
}

/// 取得 Bot 身份：已有身份直接返回；否则退避重试 `getMe`（None = 关停或鉴权终止）。
async fn ensure_identity(
    context: &ManagerContext,
    shutdown_rx: &mut oneshot::Receiver<()>,
    initial: Option<BotIdentity>,
) -> Option<BotIdentity> {
    if let Some(bot) = initial {
        return Some(bot);
    }
    let mut backoff = context.options.backoff_min;
    loop {
        tokio::select! {
            _ = &mut *shutdown_rx => return None,
            _ = tokio::time::sleep(backoff) => {}
        }
        update_status(
            &context.inner,
            context.sink.as_ref(),
            ConnectionState::Connecting,
            None,
        );
        match api::get_me(
            &context.client,
            &context.config.bot_token,
            &context.options.api_base_url,
            context.options.request_timeout,
        )
        .await
        {
            Ok(identity) => {
                {
                    let mut guard = lock(&context.inner);
                    guard.bot = Some(identity.clone());
                }
                update_status(
                    &context.inner,
                    context.sink.as_ref(),
                    ConnectionState::Online,
                    None,
                );
                return Some(identity);
            }
            Err(ChannelError::Auth(reason)) => {
                tracing::warn!(channel = "telegram", error = %reason, "Telegram 鉴权失败，停止重连");
                update_status(
                    &context.inner,
                    context.sink.as_ref(),
                    ConnectionState::Error,
                    Some(reason),
                );
                return None;
            }
            Err(error) => {
                tracing::warn!(channel = "telegram", error = %error, "Telegram 身份获取失败，继续退避");
                update_status(
                    &context.inner,
                    context.sink.as_ref(),
                    ConnectionState::Error,
                    Some(error.to_string()),
                );
                backoff = next_backoff(backoff, &context.options);
            }
        }
    }
}

/// 长轮询循环：每轮拉取并处理一批更新，直至关停或鉴权失败。
async fn poll_loop(
    context: &ManagerContext,
    shutdown_rx: &mut oneshot::Receiver<()>,
    bot: BotIdentity,
) {
    let mut offset: Option<i64> = None;
    let mut backoff = context.options.backoff_min;
    let mut errored = false;
    loop {
        let request = api::get_updates(
            &context.client,
            &context.config.bot_token,
            &context.options.api_base_url,
            offset,
            context.options.poll_timeout_secs,
            context.options.request_timeout,
        );
        let result = tokio::select! {
            _ = &mut *shutdown_rx => return,
            result = request => result,
        };
        match result {
            Ok(updates) => {
                backoff = context.options.backoff_min;
                if errored {
                    update_status(
                        &context.inner,
                        context.sink.as_ref(),
                        ConnectionState::Online,
                        None,
                    );
                    errored = false;
                }
                if let Some(advanced) = deliver_batch(context, &bot, &updates) {
                    offset = Some(advanced);
                }
                if updates.is_empty() {
                    tokio::select! {
                        _ = &mut *shutdown_rx => return,
                        _ = tokio::time::sleep(context.options.empty_batch_delay) => {}
                    }
                }
            }
            Err(ChannelError::Auth(reason)) => {
                tracing::warn!(channel = "telegram", error = %reason, "Telegram 鉴权失败，停止轮询");
                update_status(
                    &context.inner,
                    context.sink.as_ref(),
                    ConnectionState::Error,
                    Some(reason),
                );
                return;
            }
            Err(error) => {
                tracing::warn!(channel = "telegram", error = %error, "Telegram 轮询失败，准备退避重试");
                update_status(
                    &context.inner,
                    context.sink.as_ref(),
                    ConnectionState::Error,
                    Some(error.to_string()),
                );
                errored = true;
                tokio::select! {
                    _ = &mut *shutdown_rx => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = next_backoff(backoff, &context.options);
            }
        }
    }
}

/// 处理一批更新：逐条解析投递；返回推进后的 `offset`（批内无 `update_id` 时为 None）。
///
/// 说明:
/// - 被忽略的更新（服务消息 / 非消息类更新）同样计入推进，防重复拉取。
fn deliver_batch(context: &ManagerContext, bot: &BotIdentity, updates: &[Value]) -> Option<i64> {
    let mut max_update_id: Option<i64> = None;
    for update in updates {
        if let Some(update_id) = update.get("update_id").and_then(Value::as_i64) {
            max_update_id = Some(max_update_id.map_or(update_id, |max| max.max(update_id)));
        }
        if let Some(event) = super::event::parse_update(update, bot) {
            emit_event(&context.inner, event);
        }
    }
    max_update_id.map(|max| max.saturating_add(1))
}

/// 退避序列推进：翻倍且不超过上界。
fn next_backoff(current: Duration, options: &TelegramTransportOptions) -> Duration {
    let doubled = current.saturating_mul(2);
    if doubled > options.backoff_max {
        options.backoff_max
    } else {
        doubled
    }
}
