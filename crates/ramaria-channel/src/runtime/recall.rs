//! crates/ramaria-channel/src/runtime/recall.rs - Ramaria 通道撤回通知分发模块
//!
//! 设计特点:
//! - 读循环同步回调内只做转投：立即 spawn 异步处理，不阻塞事件拉取
//! - 处理失败只记日志：撤回回应属增强路径，单条失败不影响通道运行

use std::sync::Arc;

use ramaria_service::Engine;
use ramaria_service::types::RecallNoticeRequest;

use crate::events::{RecallNotice, RecallNoticeSink};

// =========================================================
// 撤回通知分发
// =========================================================

/// 撤回通知分发器（把传输层通知转交服务层处理）。
///
/// 职责:
/// - 实现 [`RecallNoticeSink`]，把平台撤回通知转换为服务层请求；
/// - 每次通知在独立异步任务中处理（生成与投递耗时较长，不占读循环）。
pub struct RecallDispatcher {
    engine: Arc<Engine>,
}

impl RecallDispatcher {
    /// 以服务层引擎构造分发器。
    pub fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }
}

impl RecallNoticeSink for RecallDispatcher {
    /// 撤回通知回调（同步、不阻塞；立即 spawn 异步处理）。
    fn on_recall(&self, notice: &RecallNotice) {
        let engine = Arc::clone(&self.engine);
        let request = RecallNoticeRequest {
            channel: notice.channel.to_string(),
            external_ref: notice.external_ref.clone(),
            platform_message_id: notice.platform_message_id.clone(),
            operator_id: notice.operator_id.clone(),
            self_id: notice.self_id.clone(),
            occurred_at: notice.occurred_at,
        };
        // 读循环内同步回调：立即 spawn 异步处理，不阻塞事件拉取
        tokio::spawn(async move {
            match engine.channel_recall(request).await {
                Ok(outcome) => tracing::debug!(outcome = ?outcome, "撤回通知已处理"),
                Err(error) => tracing::warn!(error = %error, "撤回通知处理失败（不阻塞通道）"),
            }
        });
    }
}
