//! crates/ramaria-desktop/src/proactive.rs - Ramaria 主动消息投递接收端
//!
//! 设计特点:
//! - 实现服务层 ProactiveSink（注册制）：投递 = emit 事件 + 系统通知
//! - 事件发射失败按降级处理（消息已落库应用内可见，warn 后继续通知）
//! - 通知点击：聚焦主窗口并重播事件（activated=true）驱动前端定位会话
//! - 投递完成后异步刷新托盘未读徽标（消息已落库，未读口径即时）
//! - 隐私：日志只记会话 / 人格 / 来源等元数据，不记消息内容

use ramaria_core::error::RamariaResult;
use ramaria_service::{ProactiveMessage, ProactiveSink};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::events::{EVENT_PROACTIVE_MESSAGE, ProactiveMessagePayload};
use crate::notification::send_proactive_notification;

// =========================================================
// 投递接收端
// =========================================================

/// 主动消息投递接收端（Tauri 宿主实现）。
///
/// 语义:
/// - 投递 = 先发应用内事件，再发系统通知；
/// - 事件发射失败按降级处理（消息已落库，应用内数据完整）；
/// - 通知失败静默降级（仅记日志，不影响投递结果）。
pub(crate) struct TauriProactiveSink<R: Runtime> {
    app_handle: AppHandle<R>,
}

impl<R: Runtime> TauriProactiveSink<R> {
    /// 创建接收端（注册到引擎后由调度线程调用）。
    pub(crate) fn new(app_handle: AppHandle<R>) -> Self {
        Self { app_handle }
    }
}

impl<R: Runtime> ProactiveSink for TauriProactiveSink<R> {
    fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()> {
        let payload = build_payload(message);

        // 应用内事件：失败按降级处理（消息已落库，warn 后继续通知）
        if let Err(e) = self
            .app_handle
            .emit(EVENT_PROACTIVE_MESSAGE, payload.clone())
        {
            tracing::warn!(error = %e, "主动消息事件发射失败（降级继续通知）");
        }

        // 系统通知：点击聚焦主窗口并重播事件（activated=true，驱动前端定位会话）
        let app_handle = self.app_handle.clone();
        let click_payload = payload.activated();
        send_proactive_notification(&self.app_handle, &message.content, move || {
            focus_main_window(&app_handle);
            if let Err(e) = app_handle.emit(EVENT_PROACTIVE_MESSAGE, click_payload.clone()) {
                tracing::warn!(error = %e, "主动消息点击重播事件失败");
            }
        });

        tracing::info!(
            session_id = %message.session_id,
            persona = %message.persona,
            source = %message.source,
            "主动消息已投递"
        );

        // 未读状态变化：刷新托盘徽标（消息已落库，未读口径即时生效）
        crate::tray::spawn_tray_refresh(self.app_handle.clone());
        Ok(())
    }
}

// =========================================================
// 辅助函数
// =========================================================

/// 由投递负载构造事件负载（字段映射的唯一入口，便于单测锁定）。
fn build_payload(message: &ProactiveMessage) -> ProactiveMessagePayload {
    ProactiveMessagePayload::new(
        message.session_id.to_string(),
        message.message_id.to_string(),
        message.content.clone(),
        message.persona.clone(),
        message.source.clone(),
        message.created_at,
    )
}

/// 聚焦主窗口（通知点击回调中调用，须快速返回）。
///
/// 说明:
/// - 窗口不存在 / 显示或聚焦失败仅记 warn（点击回调中无重试语义）。
fn focus_main_window<R: Runtime>(app_handle: &AppHandle<R>) {
    let Some(window) = app_handle.get_webview_window("main") else {
        tracing::warn!("通知点击：主窗口不存在，无法聚焦");
        return;
    };
    if let Err(e) = window.show() {
        tracing::warn!(error = %e, "通知点击：主窗口显示失败");
    }
    if let Err(e) = window.set_focus() {
        tracing::warn!(error = %e, "通知点击：主窗口聚焦失败");
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// 字段映射：投递负载 → 事件负载逐项一致，activated 缺省。
    #[test]
    fn build_payload_maps_fields() {
        let message = ProactiveMessage {
            message_id: Uuid::new_v4(),
            content: "主动消息内容".to_string(),
            session_id: Uuid::new_v4(),
            persona: "char-0001".to_string(),
            source: "event".to_string(),
            created_at: 1_700_000_000_000,
        };

        let payload = build_payload(&message);
        assert_eq!(payload.session_id, message.session_id.to_string());
        assert_eq!(payload.message_id, message.message_id.to_string());
        assert_eq!(payload.content, "主动消息内容");
        assert_eq!(payload.persona, "char-0001");
        assert_eq!(payload.source, "event");
        assert_eq!(payload.created_at, 1_700_000_000_000);
        assert!(payload.activated.is_none(), "常规投递不带点击标记");
    }
}
