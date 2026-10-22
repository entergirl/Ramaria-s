//! crates/ramaria-channel/src/runtime/worker/options.rs - Ramaria 通道编排参数
//!
//! 设计特点:
//! - 参数集合只覆盖编排节奏：节流 / 分段发送 / 发送重试 / 图片理解等待
//! - 默认值取保守节奏，避免触发平台风控
//! - 纯数据类型，不含运行时状态

use std::time::Duration;

/// 编排参数（节流 / 分段发送 / 发送重试 / 图片理解等待）。
///
/// 字段约定:
/// - `min_send_interval`: 同会话两次出站的最小间隔（节流）。
/// - `segment_interval`: 分段发送的段间最小间隔（同一条回复的相邻段之间；
///   注意平台风控节奏）。
/// - `segment_max`: 单条回复的最大分条数（超出部分并入末段）。
/// - `send_retry_delay`: 可重试发送失败后的重试等待。
/// - `send_max_attempts`: 发送总尝试上限（1 次初始 + 重试；仅可重试错误消耗）。
/// - `image_understand_timeout`: 应响应事件的图片理解等待上限（超时按占位符
///   继续回复，后台理解任务不取消）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOptions {
    pub min_send_interval: Duration,
    pub segment_interval: Duration,
    pub segment_max: usize,
    pub send_retry_delay: Duration,
    pub send_max_attempts: u32,
    pub image_understand_timeout: Duration,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            min_send_interval: Duration::from_secs(1),
            segment_interval: Duration::from_millis(800),
            segment_max: 5,
            send_retry_delay: Duration::from_secs(1),
            send_max_attempts: 2,
            image_understand_timeout: Duration::from_secs(20),
        }
    }
}
