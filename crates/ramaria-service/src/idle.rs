//! crates/ramaria-service/src/idle.rs - 空闲检查用例与宿主循环（tick_idle 的服务层实现）
//!
//! 设计特点:
//! - 全库扫描：遍历 `sessions` 中**全部**活跃会话（含切换人格遗留在库的孤儿会话），
//!   而非仅当前活跃会话——与在线管线空闲检测线程同一口径
//! - 阈值口径：最后消息距今 > 给定阈值触发封存；缺省取 `[session].l1_idle_minutes`
//!   （默认 10 分钟），宿主可传入运行时可变的阈值（热更新）
//! - 抢占幂等：逐个走 `seal`（条件更新抢占），多进程同时扫描不会重复生成 L1
//! - 请求间节流：连续封存时按 `[thresholds].cluster_delay_ms` 间隔（避免触发远端 LLM 限流）
//! - 空会话跳过：无消息的会话不触发 LLM（保持既有语义）
//! - 宿主循环（[`IdleLoop`]）：按 `[session].idle_check_interval_seconds` 周期调用本用例，
//!   供长驻宿主（MCP 等）不用外部驱动即可自动封存超时会话；停止由原子标志收敛，
//!   首次检查延后一个周期（给宿主启动留缓冲），封存耗时超过周期时按延迟补跑而非连续追赶

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::now_ms;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// 宿主循环参数
// =========================================================

/// 空闲检查间隔下限（秒）：防配置误设过小造成热循环（配置缺省 60s，远大于下限）。
pub const MIN_IDLE_CHECK_INTERVAL_SECONDS: u64 = 5;

/// 关停等待上限（秒）：等待在途封存收敛；超时放弃等待（进程即将退出，不强杀任务）。
const IDLE_LOOP_SHUTDOWN_TIMEOUT_SECONDS: u64 = 15;

/// 执行一次空闲检查：补扫失败的 L1 摘要，并对超时会话执行封存。
///
/// 阈值口径:
/// - 使用 `[session].l1_idle_minutes`（默认 10 分钟）；运行时可变的阈值
///   （宿主热更新）走 [`tick_with_threshold`]。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 本次实际封存的会话数量（未抢到 / 未超时 / 空会话不计入）。
pub(crate) async fn tick(engine: &Engine) -> RamariaResult<usize> {
    tick_with_threshold(engine, engine.config().session.l1_idle_minutes).await
}

/// 执行一次空闲检查（空闲阈值由调用方给定）。
///
/// 流程:
/// 0. L1 摘要补扫：消费封存失败遗留的 pending `l1_summary_retry` 任务（先于本轮封存执行，
///    避免本轮新登记的任务在同一轮被立即重试一次——LLM 不可用时白耗一次调用）；
/// 1. 列出全部活跃会话；
/// 2. 逐个读取最后消息时间（无消息 → 跳过）；
/// 3. 超过 `idle_minutes` → 走 `seal` 用例（内部抢占，防止重复摘要）；
/// 4. 每封存一个会话后按 `[thresholds].cluster_delay_ms` 节流。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `idle_minutes`: 空闲阈值（分钟）；由调用方给定，支持运行时热更新。
///
/// 返回:
/// - 本次实际封存的会话数量（未抢到 / 未超时 / 空会话不计入）。
pub(crate) async fn tick_with_threshold(
    engine: &Engine,
    idle_minutes: u32,
) -> RamariaResult<usize> {
    // 封存许可（服务层门禁）：关闭时不扫描、不封存、不做摘要补扫
    // （MCP 宿主在 allow_seal=false 时已不拉起循环，此处为服务层兜底，
    //   同时覆盖直接调用 `tick_idle` 的入口）
    if !engine.seal_allowed() {
        tracing::debug!("空闲检查：封存已禁用（服务层门禁），本轮跳过");
        return Ok(0);
    }

    let storage = engine.storage_ref().as_ref();
    let config = engine.config();
    let threshold_ms = idle_minutes as i64 * 60_000;

    // ---- 0. L1 摘要补扫：消费封存失败遗留的 pending 任务，先于本轮封存执行
    //      （避免本轮新登记的任务被立即重试一次——LLM 不可用时白耗一次调用） ----
    let retry_stats = crate::lifecycle::l1::retry_pending_l1_jobs_with_stats(engine).await;
    if retry_stats.completed > 0 {
        tracing::info!(
            scanned = retry_stats.scanned,
            attempted = retry_stats.attempted,
            completed = retry_stats.completed,
            "空闲检查：L1 摘要补扫完成"
        );
    }

    let sessions = storage.list_active_sessions().await?;
    if sessions.is_empty() {
        tracing::debug!("空闲检查：无活跃会话");
        return Ok(0);
    }

    let mut sealed = 0usize;
    for session in &sessions {
        // 单会话读取失败不中断整轮扫描（其余会话照常封存，失败者下轮重试）
        let last_active = match last_message_time(storage, session.id).await {
            Ok(time) => time,
            Err(e) => {
                tracing::warn!(
                    session_id = %session.id,
                    error = %e,
                    "空闲检查：读取最后消息时间失败，跳过该会话"
                );
                continue;
            }
        };
        let Some(last_active) = last_active else {
            tracing::debug!(session_id = %session.id, "空闲检查：会话无消息，跳过");
            continue;
        };
        let idle_ms = now_ms().saturating_sub(last_active);
        if idle_ms < threshold_ms {
            tracing::debug!(
                session_id = %session.id,
                idle_minutes = %format!("{:.1}", idle_ms as f64 / 60_000.0),
                "空闲检查：会话仍在活跃，未触发封存"
            );
            continue;
        }

        tracing::info!(
            session_id = %session.id,
            idle_minutes = %format!("{:.1}", idle_ms as f64 / 60_000.0),
            threshold_minutes = idle_minutes,
            "空闲检查：会话超时，执行封存"
        );

        match crate::seal::run(engine, session.id).await {
            Ok(outcome) => {
                if outcome.sealed {
                    sealed += 1;
                    // 连续封存节流（共享 LLM 速率保护；间隔沿用 [thresholds].cluster_delay_ms）
                    ramaria_memory::llm_gate::inter_llm_delay(
                        config.thresholds.cluster_delay_ms,
                        "L1 空闲批量封存",
                    )
                    .await;
                }
            }
            // 单个会话封存失败不阻塞其余会话（LLM 不可用时下轮继续尝试）
            Err(e) => {
                tracing::error!(session_id = %session.id, error = %e, "空闲检查：封存失败，跳过该会话");
            }
        }
    }

    tracing::info!(
        checked = sessions.len(),
        sealed,
        "空闲检查完成（本轮封存 {sealed} 个会话）"
    );
    Ok(sealed)
}

/// 读取会话最后消息时间。
///
/// 降级:
/// - `get_last_message_time` 未覆写（Unsupported）→ 回退全量加载消息取最大值；
/// - 无消息 → `Ok(None)`。
async fn last_message_time(
    storage: &dyn StorageBackend,
    session_id: Uuid,
) -> RamariaResult<Option<i64>> {
    match storage.get_last_message_time(session_id).await {
        Ok(time) => Ok(time),
        Err(RamariaError::Unsupported { .. }) => {
            let messages = storage.list_messages(session_id).await?;
            Ok(messages.iter().map(|m| m.created_at).max())
        }
        Err(e) => Err(e),
    }
}

// =========================================================
// 宿主循环（进程内空闲检查）
// =========================================================

/// 空闲检查循环选项。
///
/// 字段约定:
/// - `interval_seconds`: 两次扫描之间的间隔（秒），最小 1 秒（tokio 定时器不接受 0）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleLoopOptions {
    interval_seconds: u64,
}

impl IdleLoopOptions {
    /// 按生效配置构造（`[session].idle_check_interval_seconds`，夹取到下限）。
    ///
    /// 参数:
    /// - `config`: 生效配置（服务层只读快照）。
    pub fn from_config(config: &RamariaConfig) -> Self {
        let configured = config.session.idle_check_interval_seconds as u64;
        let interval_seconds = configured.max(MIN_IDLE_CHECK_INTERVAL_SECONDS);
        if interval_seconds != configured {
            tracing::warn!(
                configured,
                used = interval_seconds,
                "空闲检查间隔配置过小，已按下限夹取（避免热循环）"
            );
        }
        Self { interval_seconds }
    }

    /// 显式指定间隔（测试与特殊宿主使用；不做下限夹取，调用方自行保证不小于 1 秒）。
    pub fn new(interval_seconds: u64) -> Self {
        Self { interval_seconds }
    }

    /// 生效间隔（秒）。
    pub fn interval_seconds(&self) -> u64 {
        self.interval_seconds
    }
}

/// 空闲检查循环句柄。
///
/// 职责:
/// - 持有后台任务与停止标志：宿主退出时调用 [`IdleLoop::shutdown`] 优雅关停；
/// - drop 时仅置停止位（任务在下一轮 tick 自行退出），不阻塞调用方。
///
/// 并发约定:
/// - 循环与前台用例共享同一 `Engine`：会话封存走抢占式条件更新，多进程 / 多线程
///   同时封存同一会话时只有一方生成 L1（幂等）。
pub struct IdleLoop {
    /// 停止标志（true = 循环应在下一轮退出）。
    stop: Arc<AtomicBool>,
    /// 后台任务句柄（`shutdown` 时取走并等待）。
    handle: Option<JoinHandle<()>>,
}

impl IdleLoop {
    /// 拉起空闲检查循环。
    ///
    /// 流程:
    /// 1. 等待一个间隔（跳过定时器的首次立即触发，给宿主启动与首次调用留缓冲）；
    /// 2. 每轮先检查停止标志，再执行一次 [`Engine::tick_idle`]；
    /// 3. 单轮失败（库不可用 / LLM 不可用）只记日志，下一轮继续重试，不终止循环。
    ///
    /// 参数:
    /// - `engine`: 服务层引擎（`Arc` 共享，循环与前台用例并发使用）。
    /// - `options`: 循环选项（间隔）。
    pub fn spawn(engine: Arc<Engine>, options: IdleLoopOptions) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        // tokio 定时器要求非零周期：即使调用方传入 0 也退化为 1 秒
        let interval_seconds = options.interval_seconds.max(1);

        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds));
            // 首个 tick 立即返回：消费掉，使第一次扫描发生在启动后一个间隔
            ticker.tick().await;
            // 封存耗时（LLM）可能超过间隔：延迟补跑，不连续追赶（避免堆积任务与限流）
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            tracing::info!(
                interval_seconds,
                idle_minutes = engine.config().session.l1_idle_minutes,
                "空闲检查循环已启动（宿主退出时优雅关停）"
            );

            loop {
                ticker.tick().await;
                if stop_flag.load(Ordering::Acquire) {
                    tracing::info!("空闲检查循环收到停止信号，退出");
                    return;
                }
                match tick(&engine).await {
                    Ok(0) => tracing::debug!("空闲检查：本轮无需封存"),
                    Ok(sealed) => {
                        tracing::info!(sealed, "空闲检查：本轮已封存 {sealed} 个超时会话")
                    }
                    // 单轮失败不终止循环：库或 LLM 恢复后下一轮自然成功
                    Err(e) => {
                        tracing::error!(error = %e, "空闲检查失败，将在下一轮重试");
                    }
                }
            }
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// 循环是否仍在运行（未收到停止信号且任务未结束）。
    pub fn is_running(&self) -> bool {
        match self.handle.as_ref() {
            Some(handle) => !handle.is_finished() && !self.stop.load(Ordering::Acquire),
            None => false,
        }
    }

    /// 优雅关停：置停止位并等待在途的一轮扫描结束。
    ///
    /// 说明:
    /// - 等待上限 [`IDLE_LOOP_SHUTDOWN_TIMEOUT_SECONDS`] 秒：在途封存可能正在调用 LLM，
    ///   超时后放弃等待（任务随运行时退出结束），仅记 warn 不影响退出流程；
    /// - 可重复调用（第二次为空操作）。
    pub async fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        let Some(handle) = self.handle.take() else {
            return;
        };
        let timeout = Duration::from_secs(IDLE_LOOP_SHUTDOWN_TIMEOUT_SECONDS);
        match tokio::time::timeout(timeout, handle).await {
            Ok(Ok(())) => tracing::info!("空闲检查循环已关停"),
            Ok(Err(e)) => tracing::warn!(error = %e, "空闲检查循环异常结束"),
            Err(_) => tracing::warn!(
                timeout_seconds = IDLE_LOOP_SHUTDOWN_TIMEOUT_SECONDS,
                "空闲检查循环未在超时内退出（可能正在封存），放弃等待"
            ),
        }
    }
}

impl Drop for IdleLoop {
    /// drop 只置停止位：不阻塞（宿主可能在同步语境中释放句柄），任务于下一轮自行退出。
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
