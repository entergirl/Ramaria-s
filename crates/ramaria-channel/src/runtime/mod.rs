//! crates/ramaria-channel/src/runtime/mod.rs - Ramaria 通道运行时模块
//!
//! 设计特点:
//! - 宿主无关：只持有装配参数、事件接收端与状态簿，不含平台协议概念
//! - 状态簿按通道名独立记录；读取返回拷贝，写入后锁外通知宿主（不持锁回调）
//! - 通道名排序稳定：状态列表按名称字典序输出，宿主展示顺序确定
//! - 运行时自身实现宿主事件回调：传输层可直接以本类型为状态上报终点

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::config::ChannelRuntimeConfig;
use crate::events::ChannelEventSink;
use crate::status::ChannelStatus;

pub mod assemble;
pub mod download;
pub mod outbound;
pub mod recall;
pub mod worker;

// =========================================================
// 通道运行时
// =========================================================

/// 通道运行时（状态簿与状态上报）。
///
/// 职责:
/// - 持有运行期装配参数、宿主事件接收端与各通道状态快照；
/// - 接收传输层的状态上报并同步给宿主（落快照 + 回调）。
///
/// 并发约定:
/// - 状态簿为 `RwLock<HashMap>`：写入在锁内完成，宿主回调在锁外执行
///   （回调实现可能投递宿主事件循环，不得持锁调用）。
pub struct ChannelRuntime {
    config: ChannelRuntimeConfig,
    sink: Arc<dyn ChannelEventSink>,
    statuses: RwLock<HashMap<String, ChannelStatus>>,
}

impl ChannelRuntime {
    /// 以装配参数与事件接收端构造运行时（状态簿为空）。
    pub fn new(config: ChannelRuntimeConfig, sink: Arc<dyn ChannelEventSink>) -> Self {
        Self {
            config,
            sink,
            statuses: RwLock::new(HashMap::new()),
        }
    }

    /// 运行期装配参数快照。
    pub fn config(&self) -> &ChannelRuntimeConfig {
        &self.config
    }

    /// 查询单通道当前状态快照（未知通道为 None）。
    pub fn status(&self, channel: &str) -> Option<ChannelStatus> {
        self.lock_statuses().get(channel).cloned()
    }

    /// 全部通道状态列表（按通道名升序，稳定）。
    pub fn statuses(&self) -> Vec<ChannelStatus> {
        let guard = self.lock_statuses();
        let mut list: Vec<ChannelStatus> = guard.values().cloned().collect();
        list.sort_by(|a, b| a.channel.cmp(&b.channel));
        list
    }

    /// 接收一次状态上报：更新快照并通知宿主。
    ///
    /// 参数:
    /// - `status`: 传输层上报的状态快照（按 `channel` 键覆盖同通道旧值）。
    ///
    /// 说明:
    /// - 宿主回调在锁外执行；回调收到的是更新后的快照拷贝。
    pub fn report_status(&self, status: ChannelStatus) {
        let snapshot = {
            let mut guard = self.lock_statuses();
            guard.insert(status.channel.clone(), status.clone());
            status
        };
        self.sink.on_status_changed(&snapshot);
    }

    /// 获取状态簿写锁（中毒时取回内部数据，保持运行时可用）。
    fn lock_statuses(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, ChannelStatus>> {
        match self.statuses.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl ChannelEventSink for ChannelRuntime {
    /// 传输层的状态上报直接落状态簿并转发宿主事件端。
    fn on_status_changed(&self, status: &ChannelStatus) {
        self.report_status(status.clone());
    }
}

#[cfg(test)]
mod tests;
