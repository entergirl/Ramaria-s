//! crates/ramaria-service/src/lifecycle/idle.rs - 生命周期空闲检查线程
//!
//! 设计特点:
//! - 空闲阈值来自 [`Lifecycle`]（可热更新）：每轮读取最新值，阈值变更无需重建循环
//! - 与宿主的活跃指针一致性 / 状态刷新均在 [`Lifecycle::tick_idle`] 内完成，本模块只做循环编排
//! - 首次检查延后一个间隔（给宿主启动留缓冲）；封存耗时超过间隔时按延迟补跑而非连续追赶
//! - 单轮失败（库 / LLM 不可用）只记日志，下一轮继续重试，不终止循环
//! - 停止由 [`Lifecycle`] 的共享停止位收敛；日志只记计数与 ID，不记原文

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tracing::{debug, error, info};

use crate::lifecycle::Lifecycle;

/// 拉起空闲检查线程（返回任务句柄；停止由 [`Lifecycle`] 的停止位控制）。
///
/// 参数:
/// - `lifecycle`: 生命周期容器（每轮调用其 `tick_idle`，与手动触发同一份实现；
///   容器持有引擎，循环存活期间引擎不被提前释放）。
/// - `interval_seconds`: 两轮检查之间的间隔（秒；小于 1 按 1 秒处理）。
pub(crate) fn spawn(
    lifecycle: Arc<Lifecycle>,
    interval_seconds: u64,
) -> tokio::task::JoinHandle<()> {
    let shutdown = lifecycle.shutdown_flag();
    tokio::spawn(async move {
        // tokio 定时器要求非零周期：即使调用方传入 0 也退化为 1 秒
        let interval_seconds = interval_seconds.max(1);
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds));
        // 首个 tick 立即返回：消费掉，使第一次扫描发生在启动后一个间隔
        ticker.tick().await;
        // 封存耗时（LLM）可能超过间隔：延迟补跑，不连续追赶（避免堆积任务与限流）
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        info!(
            interval_seconds,
            idle_minutes = lifecycle.idle_minutes(),
            "会话生命周期空闲检查线程已启动"
        );

        loop {
            ticker.tick().await;
            if shutdown.load(Ordering::Acquire) {
                info!("空闲检查线程收到停止信号，退出");
                return;
            }
            match lifecycle.tick_idle().await {
                Ok(0) => debug!("空闲检查：本轮无需封存"),
                Ok(sealed) => info!(sealed, "空闲检查：本轮已封存 {sealed} 个超时会话"),
                // 单轮失败不终止循环：库或 LLM 恢复后下一轮自然成功
                Err(e) => error!(error = %e, "空闲检查失败，将在下一轮重试"),
            }
        }
    })
}
