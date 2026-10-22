//! crates/ramaria-service/src/proactive/schedule/cycle.rs - Ramaria 主动调度后台循环
//!
//! 设计特点:
//! - 首轮延迟后按间隔循环，单轮失败只记错误不终止
//! - 首轮延迟与轮次等待按分片感知停止位，停止后立即退出

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{debug, error, info};

use ramaria_core::types::now_ms;

use crate::engine::Engine;

use super::SHUTDOWN_POLL_CHUNK_SECONDS;
use super::round::run_tick;
use super::types::TopicPicker;

// =========================================================
// 后台调度循环
// =========================================================

/// 启动主动对话后台调度任务。
///
/// 逻辑:
/// - 首轮延迟 `first_delay_seconds` 秒执行（避开宿主启动阶段）；
/// - 之后每 `interval_seconds` 秒执行一轮；单轮失败记 error 且不终止
///   （下一轮重试）；
/// - 停止位置位后退出；首轮延迟与轮次等待均按 60 秒分片感知停止位。
///
/// 参数:
/// - `engine`: 服务层引擎（`Arc` 共享）。
/// - `shutdown`: 宿主停止位（true = 循环应在下一轮退出）。
/// - `first_delay_seconds`: 首轮检查前的延迟秒数。
/// - `interval_seconds`: 两轮检查之间的间隔秒数。
/// - `picker`: 选题提供者（宿主装配）。
pub(crate) fn spawn(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
    first_delay_seconds: u64,
    interval_seconds: u64,
    picker: Arc<dyn TopicPicker>,
) -> tokio::task::JoinHandle<()> {
    info!(
        first_delay_seconds,
        interval_seconds, "主动对话调度任务启动"
    );

    tokio::spawn(async move {
        // 首次延迟：避免在宿主启动阶段触发；等待期间收到停止信号直接退出
        if !interruptible_sleep(first_delay_seconds, &shutdown).await {
            info!("主动对话调度任务在首轮延迟期间收到停止信号，退出");
            return;
        }

        loop {
            if shutdown.load(Ordering::Relaxed) {
                info!("主动对话调度任务收到停止信号，退出");
                return;
            }

            match run_tick(&engine, now_ms(), picker.as_ref()).await {
                Ok(summary) => {
                    if summary.attempts > 0 || summary.generated > 0 {
                        info!(
                            personas = summary.personas,
                            attempts = summary.attempts,
                            generated = summary.generated,
                            delivered = summary.delivered,
                            "主动对话调度：本轮完成"
                        );
                    } else {
                        debug!(personas = summary.personas, "主动对话调度：本轮无候选");
                    }
                }
                Err(e) => {
                    error!(error = %e, "主动对话调度：本轮失败（下轮重试）");
                }
            }

            // 等待下一轮；0 间隔热转由装配层夹取兜底
            interruptible_sleep(interval_seconds, &shutdown).await;
        }
    })
}

/// 可中断等待：按 60 秒分片睡眠，每片感知停止位。
///
/// 返回:
/// - `true`: 睡满指定秒数；`false`: 等待期间收到停止信号（调用方应立即退出）。
async fn interruptible_sleep(seconds: u64, shutdown: &AtomicBool) -> bool {
    let mut remaining = seconds;
    while remaining > 0 {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        let chunk = remaining.min(SHUTDOWN_POLL_CHUNK_SECONDS);
        tokio::time::sleep(Duration::from_secs(chunk)).await;
        remaining = remaining.saturating_sub(chunk);
    }
    !shutdown.load(Ordering::Relaxed)
}
