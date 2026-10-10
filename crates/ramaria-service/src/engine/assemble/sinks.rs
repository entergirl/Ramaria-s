//! crates/ramaria-service/src/engine/assemble/sinks.rs - Ramaria 主动消息投递接收端门面
//!
//! 设计特点:
//! - 注册制：宿主启动时注入实现（本地 / 通道各一），未注册时调度静默丢弃（降级不阻塞）
//! - 两个槽位并列：本地接收端（应用内通知）与通道接收端（社交通道发送）互不替代
//! - 门面只做槽位读写；投递路由与门禁由调度层实现

use std::sync::Arc;

use ramaria_core::lock::{read_recover, write_recover};

use crate::proactive::ProactiveSink;

use super::super::Engine;

// =========================================================
// 投递接收端门面
// =========================================================

impl Engine {
    /// 注册主动消息投递接收端（覆盖已有注册）。
    ///
    /// 说明:
    /// - 宿主启动时注入（桌面宿主实现系统通知与应用内转发）；
    /// - 未注册时调度静默丢弃（降级不阻塞）。
    pub fn set_proactive_sink(&self, sink: Arc<dyn ProactiveSink>) {
        tracing::info!("主动消息投递接收端已注册");
        let mut guard = write_recover(&self.proactive_sink, "engine.proactive_sink");
        *guard = Some(sink);
    }

    /// 当前注册的主动消息投递接收端（未注册 → None）。
    pub fn proactive_sink(&self) -> Option<Arc<dyn ProactiveSink>> {
        read_recover(&self.proactive_sink, "engine.proactive_sink").clone()
    }

    /// 注册通道路主动消息投递接收端（覆盖已有注册）。
    ///
    /// 说明:
    /// - 宿主启动时注入（通道侧实现按通道查找传输并发送）；
    /// - 未注册时通道投递按门禁丢弃（降级不阻塞）。
    pub fn set_channel_proactive_sink(&self, sink: Arc<dyn ProactiveSink>) {
        tracing::info!("通道路主动消息投递接收端已注册");
        let mut guard = write_recover(
            &self.channel_proactive_sink,
            "engine.channel_proactive_sink",
        );
        *guard = Some(sink);
    }

    /// 当前注册的通道路主动消息投递接收端（未注册 → None）。
    pub fn channel_proactive_sink(&self) -> Option<Arc<dyn ProactiveSink>> {
        read_recover(
            &self.channel_proactive_sink,
            "engine.channel_proactive_sink",
        )
        .clone()
    }
}
