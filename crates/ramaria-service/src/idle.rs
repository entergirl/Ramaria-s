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
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, MockLlm, engine_on_existing_db, engine_with_failing_llm,
        engine_with_l1_reply, seed_persona, seed_session_with_messages,
    };
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use std::time::Instant;

    /// 3 个会话 2 个超时：只封存超时的 2 个，未超时的保持活跃。
    #[tokio::test]
    async fn tick_seals_only_expired_sessions() {
        let (engine, storage, dir) = engine_with_l1_reply("idle", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;

        // 阈值 10 分钟：20 分钟前 → 超时；刚刚 → 未超时
        let stale_base = now_ms() - 20 * 60_000;
        let stale_a = seed_session_with_messages(&storage, "char-0001", 2, stale_base).await;
        let stale_b =
            seed_session_with_messages(&storage, "char-0001", 2, stale_base + 5_000).await;
        let fresh = seed_session_with_messages(&storage, "char-0001", 2, now_ms()).await;

        let sealed = engine.tick_idle().await.expect("空闲检查应成功");
        assert_eq!(sealed, 2, "应封存 2 个超时会话");

        // 超时会话：已关闭 + 生成 L1
        let stale_session = storage
            .get_session(stale_a)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(stale_session.ended_at.is_some(), "超时会话应被关闭");
        assert_eq!(
            storage
                .list_memory_l1(stale_a)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "超时会话应生成 L1"
        );
        assert!(
            storage
                .get_session(stale_b)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在")
                .ended_at
                .is_some(),
            "第二个超时会话也应被关闭"
        );

        // 未超时会话：保持活跃
        let fresh_session = storage
            .get_session(fresh)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(fresh_session.ended_at.is_none(), "未超时会话不应被关闭");

        // 幂等：再跑一次无超时会话 → 0
        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无消息的空会话不触发封存（不调用 LLM）。
    #[tokio::test]
    async fn tick_skips_empty_sessions() {
        let (engine, storage, dir) = engine_with_l1_reply("idle-empty", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话");

        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);
        let stored = storage
            .get_session(session.id)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(stored.ended_at.is_none(), "空会话应保持活跃");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无活跃会话 → 0（空库不报错）。
    #[tokio::test]
    async fn tick_without_sessions_returns_zero() {
        let (engine, _storage, dir) = engine_with_l1_reply("idle-none", L1_JSON_REPLY).await;
        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 阈值参数化：`tick_with_threshold` 按传入阈值判定（0 分钟 → 刚活跃会话也视为超时）。
    #[tokio::test]
    async fn tick_with_threshold_uses_given_threshold() {
        let (engine, storage, dir) = engine_with_l1_reply("idle-threshold", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session = seed_session_with_messages(&storage, "char-0001", 2, now_ms()).await;

        // 缺省口径（10 分钟）：刚活跃 → 不封存
        assert_eq!(tick(&engine).await.expect("空闲检查应成功"), 0);

        // 阈值 0 分钟：立即视为超时 → 封存
        assert_eq!(
            tick_with_threshold(&engine, 0)
                .await
                .expect("空闲检查应成功"),
            1,
            "阈值 0 分钟时刚活跃会话也应封存"
        );
        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_some(), "会话应被关闭");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 封存门禁（D-V21-009）：许可关闭时整轮跳过
    /// （超时会话不封存、不生成 L1、会话保持活跃）。
    #[tokio::test]
    async fn tick_is_noop_when_seal_disabled() {
        let (engine, storage, dir) = engine_with_l1_reply("idle-gated", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

        engine.set_seal_allowed(false);
        assert_eq!(
            engine.tick_idle().await.expect("空闲检查应成功"),
            0,
            "许可关闭时不应封存任何会话"
        );

        let row = storage
            .get_session(session)
            .await
            .expect("查询应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_none(), "许可关闭时会话应保持活跃");
        assert!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "许可关闭时不应生成 L1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L1 摘要补扫（D4）：封存中 L1 失败登记的 pending 任务，在 LLM 恢复后
    /// 由空闲检查自动补跑（MCP 独用无桌面时的消费点）。
    #[tokio::test]
    async fn tick_consumes_pending_l1_retry_after_llm_recovers() {
        let (engine, storage, dir) = engine_with_failing_llm("idle-retry").await;
        seed_persona(&storage, "char-0001").await;
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

        // 第一轮：超时会话被抢占关闭，但 L1 生成失败 → 登记 pending 重试任务
        // （返回 0：封存失败不计入成功数，但会话已被抢占关闭）
        assert_eq!(
            engine.tick_idle().await.expect("空闲检查应成功"),
            0,
            "L1 生成失败不应计入封存成功数"
        );
        let closed = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(closed.ended_at.is_some(), "超时会话应已被抢占关闭");
        let pending = storage
            .list_pending_jobs()
            .await
            .expect("查询 pending 应成功");
        assert!(
            pending
                .iter()
                .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
            "L1 失败应登记 l1_summary_retry pending 任务: {pending:?}"
        );
        assert!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "LLM 不可用时不应生成 L1"
        );

        // LLM 恢复：同库第二台引擎（成功 mock）执行空闲检查 → 先补扫 pending，产出摘要
        let recovered = engine_on_existing_db(
            &dir.join("assistant.db"),
            MockLlm::with_reply(L1_JSON_REPLY),
            RamariaConfig::default(),
        )
        .await;
        assert_eq!(
            recovered.tick_idle().await.expect("空闲检查应成功"),
            0,
            "会话已关闭，本轮无需封存"
        );
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "LLM 恢复后空闲检查应补跑出 L1"
        );
        let remaining = storage
            .list_pending_jobs()
            .await
            .expect("查询 pending 应成功");
        assert!(
            !remaining
                .iter()
                .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
            "补跑成功后任务不应再停留 pending: {remaining:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 补扫只消费补偿登记类型（`l1_summary_retry`）：在途生成任务（`l1_summary`）
    /// 处于 pending 窗口时不得被误取（误取会重复生成同一会话的 L1）。
    #[tokio::test]
    async fn tick_retry_ignores_inflight_l1_generation_jobs() {
        let (engine, storage, dir) = engine_with_l1_reply("idle-retry-type", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
        // 模拟"已在途生成"现场：会话已关闭 + 一条旧类型（l1_summary）pending 任务
        storage
            .close_session(session)
            .await
            .expect("关闭会话应成功");
        let payload = serde_json::json!({ "session_id": session.to_string() }).to_string();
        storage
            .create_background_job("l1_summary", Some(&payload))
            .await
            .expect("登记任务应成功");

        assert_eq!(engine.tick_idle().await.expect("空闲检查应成功"), 0);
        assert!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "在途生成任务类型（l1_summary）不应被补扫消费"
        );
        let pending = storage
            .list_pending_jobs()
            .await
            .expect("查询 pending 应成功");
        assert!(
            pending
                .iter()
                .any(|(_, job_type, _)| job_type == "l1_summary"),
            "旧类型任务应保持原状态（由真正的执行方收敛）: {pending:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 补扫并发去重：两台引擎同时补扫同一 pending 补偿任务 →
    /// 原子抢占保证只有一方执行，最终恰好一份 L1。
    #[tokio::test]
    async fn concurrent_retry_from_two_engines_generates_once() {
        // 第一轮用恒失败 LLM：L1 失败 → 登记补偿任务（会话已被抢占关闭）
        let (engine_a, storage, dir) = engine_with_failing_llm("idle-retry-race").await;
        seed_persona(&storage, "char-0001").await;
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
        assert_eq!(engine_a.tick_idle().await.expect("空闲检查应成功"), 0);

        // LLM 恢复：同一库上两台引擎并发补扫
        let db_path = dir.join("assistant.db");
        let engine_b = engine_on_existing_db(
            &db_path,
            MockLlm::with_reply(L1_JSON_REPLY),
            RamariaConfig::default(),
        )
        .await;
        let engine_c = engine_on_existing_db(
            &db_path,
            MockLlm::with_reply(L1_JSON_REPLY),
            RamariaConfig::default(),
        )
        .await;
        let (result_b, result_c) = tokio::join!(engine_b.tick_idle(), engine_c.tick_idle());
        result_b.expect("空闲检查应成功");
        result_c.expect("空闲检查应成功");

        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "并发补扫不得产生重复 L1（原子抢占去重）"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // =========================================================
    // 宿主循环（M4：进程内空闲检测）
    // =========================================================

    /// 选项夹取：配置小于下限时按 [`MIN_IDLE_CHECK_INTERVAL_SECONDS`] 处理（防热循环）。
    #[test]
    fn idle_loop_options_clamp_configured_interval() {
        let mut config = RamariaConfig::default();
        assert_eq!(
            IdleLoopOptions::from_config(&config).interval_seconds(),
            config.session.idle_check_interval_seconds as u64,
            "配置缺省（60s）应原样生效"
        );

        config.session.idle_check_interval_seconds = 0;
        assert_eq!(
            IdleLoopOptions::from_config(&config).interval_seconds(),
            MIN_IDLE_CHECK_INTERVAL_SECONDS,
            "0 秒应被夹取到下限（tokio 定时器不接受零周期）"
        );

        // 显式构造不做夹取（测试与特殊宿主自行保证间隔合法）
        assert_eq!(IdleLoopOptions::new(1).interval_seconds(), 1);
    }

    /// 循环自动封存：拉起后按间隔扫描，超时会话被封闭并生成 L1；关停后不再运行。
    #[tokio::test]
    async fn idle_loop_seals_timed_out_session_then_stops() {
        let (engine, storage, dir) = engine_with_l1_reply("idle-loop", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        // 20 分钟前最后发言 → 超过 10 分钟空闲阈值
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

        // 间隔 1 秒（首次检查延后一个周期，不会在拉起瞬间就触发 LLM）
        let engine = Arc::new(engine);
        let mut idle_loop = engine.spawn_idle_loop_with(IdleLoopOptions::new(1));
        assert!(idle_loop.is_running(), "拉起后循环应处于运行状态");

        // 轮询等待自动封存完成（最多 6 秒）：不依赖固定 sleep，避免慢机偶发失败；
        // 同时等待"会话已关闭 + L1 已落库"——抢占关闭与 L1 生成落库之间存在窗口，
        // 只看关闭会在窗口内误判为"未生成 L1"。
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            let session_row = storage
                .get_session(session)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在");
            let l1_count = storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len();
            if session_row.ended_at.is_some() && l1_count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // 跳出后按最终状态断言（超时同样走到这里，给出准确的失败原因）
        let session_row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(
            session_row.ended_at.is_some(),
            "空闲检查循环应在间隔内自动封存超时会话"
        );
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "自动封存应生成 L1 摘要"
        );

        // 优雅关停：置停止位并等待在途轮次结束
        idle_loop.shutdown().await;
        assert!(!idle_loop.is_running(), "关停后循环不应再运行");
        // 重复关停为空操作（不阻塞、不报错）
        idle_loop.shutdown().await;

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 多宿主并发（M4-004 的服务层等价物）：同一库上两台引擎同时扫描 → 只生成一份 L1。
    #[tokio::test]
    async fn concurrent_tick_from_two_engines_seals_once() {
        let (engine_a, storage, dir) = engine_with_l1_reply("idle-concurrent", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session =
            seed_session_with_messages(&storage, "char-0001", 4, now_ms() - 20 * 60_000).await;

        // 第二台引擎：同一库文件、独立连接池与内存状态（模拟"桌面 + MCP"并存）
        let engine_b = engine_on_existing_db(
            &dir.join("assistant.db"),
            MockLlm::with_reply(L1_JSON_REPLY),
            RamariaConfig::default(),
        )
        .await;

        // 并发扫描：条件更新抢占保证只有一方进入封存链路
        let (result_a, result_b) = tokio::join!(engine_a.tick_idle(), engine_b.tick_idle());
        let sealed_a = result_a.expect("引擎 A 空闲检查应成功");
        let sealed_b = result_b.expect("引擎 B 空闲检查应成功");
        assert_eq!(
            sealed_a + sealed_b,
            1,
            "同一超时会话只允许一方抢到封存（抢占幂等）"
        );
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "并发扫描不得产生重复 L1 摘要"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 循环与前台用例并存（并发手测的宿主层等价物）：空闲循环运行期间前台 `seal`
    /// 照常工作，两者共同收敛全部超时会话，且每个会话恰好一份 L1（抢占幂等）。
    #[tokio::test]
    async fn idle_loop_and_foreground_seal_do_not_duplicate_l1() {
        let (engine, storage, dir) = engine_with_l1_reply("idle-foreground", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        // 两个超时会话：一个由前台抢占，另一个交给循环
        let session_front =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
        let session_loop =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;

        let engine = Arc::new(engine);
        let mut idle_loop = engine.spawn_idle_loop_with(IdleLoopOptions::new(1));

        // 前台立即抢封存（与循环的首轮扫描并发）：抢到与否都是合法结果，
        // 正确性由下方"每个会话恰好一份 L1"断言承担
        let _ = engine.seal(session_front).await.expect("前台封存不应报错");

        // 等待两个会话都被关闭且各有一份 L1（前台 + 循环共同收敛，上限 6 秒）
        //
        // 说明: 抢占（ended_at 置位）先于摘要写入完成，仅在"已关闭"时断言 L1 条数
        // 会命中该时间窗导致偶发抖动；等待条件因此同时要求 L1 落库。
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let front_closed = storage
                .get_session(session_front)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在")
                .ended_at
                .is_some();
            let loop_closed = storage
                .get_session(session_loop)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在")
                .ended_at
                .is_some();
            let front_l1 = storage
                .list_memory_l1(session_front)
                .await
                .expect("读取 L1 应成功")
                .len();
            let loop_l1 = storage
                .list_memory_l1(session_loop)
                .await
                .expect("读取 L1 应成功")
                .len();
            if front_closed && loop_closed && front_l1 == 1 && loop_l1 == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "循环与前台应在上限内关闭全部超时会话并各产出一份 L1（front_l1={front_l1}, loop_l1={loop_l1}）"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // 并发封存不产生重复摘要：每个会话恰好一份 L1
        for session in [session_front, session_loop] {
            assert_eq!(
                storage
                    .list_memory_l1(session)
                    .await
                    .expect("读取 L1 应成功")
                    .len(),
                1,
                "会话 {session} 应恰好一份 L1（抢占幂等）"
            );
        }

        idle_loop.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
