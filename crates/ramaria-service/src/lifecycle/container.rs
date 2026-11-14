//! crates/ramaria-service/src/lifecycle/container.rs - Ramaria 会话生命周期容器
//!
//! 设计特点:
//! - 与传输无关的生命周期容器：活跃指针、手动关闭、空闲检测、L2/L3 调度、主动对话调度与关停统一装配
//! - 宿主差异全部由 [`LifecycleOptions`] 表达（长驻宿主 / 仅空闲检查 / 单次执行），后台循环按选项拉起
//! - 停止语义：原子停止位供轮次内判定，`watch` 通道供等待中的循环立即唤醒；
//!   关停等待在途轮次收敛（超时只记日志，不阻塞退出）
//! - 降级纪律：LLM 不可用 / 单条数据失败均不阻塞级联与关停，仅记日志后继续
//! - 并发约定：指针与缓存由 `Mutex` 持有，锁内只做读写与克隆，不跨 `.await` 持锁

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::types::now_ms;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::engine::Engine;
use crate::idle::MIN_IDLE_CHECK_INTERVAL_SECONDS;
use crate::types::SealOutcome;

use super::{LifecycleOptions, idle, l1, l2_l3};

// =========================================================
// 装配常量
// =========================================================

/// L2/L3 定时调度首轮检查的默认延迟（秒）：避开宿主启动阶段。
const L2_L3_DEFAULT_FIRST_DELAY_SECONDS: u64 = 300;

/// 主动对话调度首轮检查的默认延迟（秒）：避开宿主启动阶段。
const PROACTIVE_DEFAULT_FIRST_DELAY_SECONDS: u64 = 60;

/// 主动对话检查间隔下限（秒）：防配置误设过小造成热循环（配置缺省 300s，远大于下限）。
const MIN_PROACTIVE_CHECK_INTERVAL_SECONDS: u64 = 30;

/// 启动期 L1 补扫的延迟（秒）：先让索引构建与首轮对话完成。
const STARTUP_L1_RETRY_DELAY_SECONDS: u64 = 30;

/// 关停等待后台循环收敛的上限（秒）：超过后放弃等待（进程即将退出，不强杀任务）。
const SHUTDOWN_WAIT_TIMEOUT_SECONDS: u64 = 15;

/// 限时关停时封存（含 L1 生成）可占用的上限（秒）：超时后登记 L1 补扫并继续退出。
const SHUTDOWN_SEAL_TIMEOUT_SECONDS: u64 = 3;

/// 限时关停时单个后台循环的等待上限（毫秒）：循环收到唤醒后立即退出，长等待只用于兜底。
const SHUTDOWN_LOOP_WAIT_MILLIS: u64 = 500;

// =========================================================
// 生命周期容器
// =========================================================

/// 会话生命周期容器：活跃指针、手动关闭、空闲检测、L2/L3 调度、主动对话调度与关停。
///
/// 职责:
/// - 活跃会话指针与各会话最后活跃时间的内存缓存（宿主每条消息落库后调用 `touch_session`）；
/// - 手动关闭活跃会话（抢占式封存，同一会话只生成一份摘要）；
/// - 按 [`LifecycleOptions`] 拉起空闲检查线程、L2/L3 调度、主动对话调度与启动期 L1 补扫；
/// - 优雅关停：置停止位 → 关闭活跃会话（落库）→ 等待在途轮次结束。
///
/// 并发约定:
/// - 指针与缓存由 `Mutex` 持有，锁内只做读写与克隆，不跨 `.await` 持锁；
/// - `shutdown_flag` / `idle_minutes` 由后台任务共享（原子读写，阈值热更新即时生效）。
pub struct Lifecycle {
    /// 服务层引擎（本容器持有引擎，引擎不反向持有本容器，避免引用环）。
    engine: Arc<Engine>,
    /// 当前活跃会话指针（同一时刻至多一个）。
    active_session_id: Mutex<Option<Uuid>>,
    /// 各会话最后活跃时间的内存缓存（Unix 毫秒），供宿主记录与读取。
    session_last_active: Mutex<HashMap<Uuid, i64>>,
    /// 停止位（后台循环共享）。
    shutdown_flag: Arc<AtomicBool>,
    /// 停止广播（后台循环在等待中订阅；与停止位同批置位）。
    shutdown_tx: watch::Sender<bool>,
    /// 空闲阈值（分钟，热更新；空闲检查每轮读取最新值）。
    idle_minutes: Arc<AtomicU32>,
    /// 空闲检查循环句柄（None = 未拉起，或关停时已取走等待）。
    idle_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// L2/L3 调度循环句柄（None = 未拉起，或关停时已取走等待）。
    l2_l3_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 主动对话调度循环句柄（None = 未拉起，或关停时已取走等待）。
    proactive_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Lifecycle {
    /// 拉起生命周期（按选项拉起后台循环；同一容器实例内每个循环只拉起一次）。
    ///
    /// 参数:
    /// - `engine`: 服务层引擎（与宿主共享同一实例）。
    /// - `options`: 装配选项（宿主差异由 [`LifecycleOptions`] 表达）。
    ///
    /// 返回:
    /// - 生命周期容器句柄；宿主退出时调用 [`Lifecycle::shutdown`] 优雅关停。
    pub fn start(engine: Arc<Engine>, options: LifecycleOptions) -> Arc<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        let lifecycle = Arc::new(Self {
            idle_minutes: Arc::new(AtomicU32::new(engine.config().session.l1_idle_minutes)),
            engine: Arc::clone(&engine),
            active_session_id: Mutex::new(None),
            session_last_active: Mutex::new(HashMap::new()),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            idle_handle: Mutex::new(None),
            l2_l3_handle: Mutex::new(None),
            proactive_handle: Mutex::new(None),
        });

        // ---- 空闲检查循环（间隔：覆盖值或配置值夹取下限） ----
        if options.idle {
            let interval_seconds = options.idle_interval_seconds.unwrap_or_else(|| {
                clamp_idle_interval(engine.config().session.idle_check_interval_seconds as u64)
            });
            let handle = idle::spawn(Arc::clone(&lifecycle), interval_seconds);
            *lock_recover(&lifecycle.idle_handle, "lifecycle.idle_handle") = Some(handle);
        }

        // ---- L2/L3 定时调度循环 ----
        if options.l2_l3 {
            let interval_seconds = options
                .l2_l3_interval_seconds
                .unwrap_or(engine.config().session.l2_check_interval_seconds as u64);
            let first_delay_seconds = options
                .l2_l3_first_delay_seconds
                .unwrap_or(L2_L3_DEFAULT_FIRST_DELAY_SECONDS);
            let handle = l2_l3::spawn_scheduler(
                Arc::clone(&engine),
                Arc::clone(&lifecycle.shutdown_flag),
                lifecycle.shutdown_watch(),
                first_delay_seconds,
                interval_seconds,
            );
            *lock_recover(&lifecycle.l2_l3_handle, "lifecycle.l2_l3_handle") = Some(handle);
        }

        // ---- 主动对话调度循环（仅桌面宿主装配；总开关由每轮 tick 读取配置判定）----
        if options.proactive {
            let interval_seconds = options.proactive_interval_seconds.unwrap_or_else(|| {
                clamp_proactive_interval(engine.config().proactive.check_interval_seconds as u64)
            });
            let first_delay_seconds = options
                .proactive_first_delay_seconds
                .unwrap_or(PROACTIVE_DEFAULT_FIRST_DELAY_SECONDS);
            let picker: Arc<dyn crate::proactive::TopicPicker> =
                Arc::new(crate::proactive::PickerTopicProvider);
            let handle = crate::proactive::spawn(
                Arc::clone(&engine),
                Arc::clone(&lifecycle.shutdown_flag),
                lifecycle.shutdown_watch(),
                first_delay_seconds,
                interval_seconds,
                picker,
            );
            *lock_recover(&lifecycle.proactive_handle, "lifecycle.proactive_handle") = Some(handle);
        }

        // ---- 启动期 L1 补扫（独立任务，不纳入句柄；由停止位提前退出） ----
        if options.startup_l1_retry {
            let retry_engine = Arc::clone(&engine);
            let shutdown = Arc::clone(&lifecycle.shutdown_flag);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(STARTUP_L1_RETRY_DELAY_SECONDS)).await;
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                let retried = l1::retry_pending_l1_jobs(&retry_engine).await;
                if retried > 0 {
                    info!(retried, "启动期 L1 补扫完成");
                }
            });
        }

        info!(
            idle = options.idle,
            l2_l3 = options.l2_l3,
            proactive = options.proactive,
            startup_l1_retry = options.startup_l1_retry,
            "会话生命周期已拉起"
        );
        lifecycle
    }

    /// 引擎引用。
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// 停止位（后台循环与宿主共享）。
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown_flag)
    }

    /// 停止广播订阅（后台循环在等待中据此立即唤醒）。
    pub(crate) fn shutdown_watch(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    /// 当前空闲阈值（分钟）。
    pub fn idle_minutes(&self) -> u32 {
        self.idle_minutes.load(Ordering::Relaxed)
    }

    /// 热更新空闲阈值（分钟）：后续空闲检查轮次立即生效，不需要重建循环。
    ///
    /// 参数:
    /// - `minutes`: 新阈值（分钟）。任意 `u32` 均接受（配置层已做范围校验）。
    pub fn set_idle_minutes(&self, minutes: u32) {
        let old = self.idle_minutes.swap(minutes, Ordering::Relaxed);
        info!(
            old_minutes = old,
            new_minutes = minutes,
            "空闲自动保存阈值已热更新"
        );
    }

    // =========================================================
    // 活跃会话追踪
    // =========================================================

    /// 当前活跃会话 ID（无则 None）。
    pub fn active_session_id(&self) -> Option<Uuid> {
        *lock_recover(&self.active_session_id, "lifecycle.active_session_id")
    }

    /// 设置当前活跃会话 ID。
    pub fn set_active_session_id(&self, session_id: Option<Uuid>) {
        let mut guard = lock_recover(&self.active_session_id, "lifecycle.active_session_id");
        *guard = session_id;
    }

    /// 记录会话最后活跃时间（内存缓存，避免频繁读库）。
    ///
    /// 用法:
    /// - 宿主每条消息落库后调用。
    pub fn touch_session(&self, session_id: Uuid) {
        let now = now_ms();
        let mut guard = lock_recover(&self.session_last_active, "lifecycle.session_last_active");
        guard.insert(session_id, now);
        debug!(%session_id, last_active = now, "会话活跃时间已更新");
    }

    /// 读取会话最后活跃时间（内存缓存；未记录则 None）。
    pub fn last_active(&self, session_id: Uuid) -> Option<i64> {
        let guard = lock_recover(&self.session_last_active, "lifecycle.session_last_active");
        guard.get(&session_id).copied()
    }

    /// 移除会话的活跃时间缓存（会话关闭后清理）。
    pub fn forget_session(&self, session_id: Uuid) {
        let mut guard = lock_recover(&self.session_last_active, "lifecycle.session_last_active");
        guard.remove(&session_id);
    }

    /// 会话被删除后清理指针与缓存（指针指向该会话时置空）。
    pub fn clear_active_if(&self, session_id: Uuid) {
        {
            let mut guard = lock_recover(&self.active_session_id, "lifecycle.active_session_id");
            if *guard == Some(session_id) {
                *guard = None;
            }
        }
        self.forget_session(session_id);
    }

    // =========================================================
    // 循环状态
    // =========================================================

    /// 空闲检查循环是否在运行（未收停止信号且任务未结束）。
    pub fn idle_loop_running(&self) -> bool {
        let handle = lock_recover(&self.idle_handle, "lifecycle.idle_handle");
        handle.as_ref().is_some_and(|h| !h.is_finished())
            && !self.shutdown_flag.load(Ordering::Acquire)
    }

    /// L2/L3 调度循环是否在运行（未收停止信号且任务未结束）。
    pub fn l2_l3_running(&self) -> bool {
        let handle = lock_recover(&self.l2_l3_handle, "lifecycle.l2_l3_handle");
        handle.as_ref().is_some_and(|h| !h.is_finished())
            && !self.shutdown_flag.load(Ordering::Acquire)
    }

    /// 主动对话调度循环是否在运行（未收停止信号且任务未结束）。
    pub fn proactive_running(&self) -> bool {
        let handle = lock_recover(&self.proactive_handle, "lifecycle.proactive_handle");
        handle.as_ref().is_some_and(|h| !h.is_finished())
            && !self.shutdown_flag.load(Ordering::Acquire)
    }

    // =========================================================
    // 空闲检查与手动关闭
    // =========================================================

    /// 执行一次空闲检查（与后台空闲线程同一份实现）。
    ///
    /// 流程:
    /// 1. 按当前阈值扫描全部活跃会话，超时者走抢占式封存（见 `crate::idle::tick_with_threshold`）；
    /// 2. 活跃指针一致性：指针指向的会话已关闭或不存在 → 清空指针与缓存；
    /// 3. 刷新应用状态与事实对齐，失败只 warn（不影响本轮封存结果）。
    ///
    /// 返回:
    /// - 本轮实际封存的会话数量。
    pub async fn tick_idle(&self) -> RamariaResult<usize> {
        let sealed = crate::idle::tick_with_threshold(&self.engine, self.idle_minutes()).await?;
        self.clear_pointer_if_closed().await;
        if let Err(e) = self.engine.refresh_setup_state().await {
            warn!(error = %e, "空闲检查：刷新应用状态失败（不影响本轮封存结果）");
        }
        Ok(sealed)
    }

    /// 活跃指针一致性：指针指向的会话已在库中关闭或不存在 → 清空指针与缓存。
    ///
    /// 说明:
    /// - 查询失败时保守保留指针（仅 warn），不影响调用方返回值。
    async fn clear_pointer_if_closed(&self) {
        let Some(session_id) = self.active_session_id() else {
            return;
        };
        match self.engine.storage_ref().get_session(session_id).await {
            Ok(Some(session)) if session.ended_at.is_some() => {
                self.set_active_session_id(None);
                self.forget_session(session_id);
                debug!(%session_id, "空闲检查：活跃会话已关闭，指针已清理");
            }
            Ok(None) => {
                self.set_active_session_id(None);
                self.forget_session(session_id);
                debug!(%session_id, "空闲检查：活跃会话已不存在，指针已清理");
            }
            Ok(Some(_)) => {}
            Err(e) => {
                warn!(%session_id, error = %e, "空闲检查：复查活跃会话状态失败（保留指针）");
            }
        }
    }

    /// 手动关闭当前活跃会话（走抢占式封存）。
    ///
    /// 语义:
    /// - 无活跃指针 → `Ok(None)`；
    /// - 抢占成功 → 清指针与缓存 → `Ok(Some(outcome))`；
    /// - 封存报错但复查发现会话已关闭（摘要生成失败、补偿任务已登记）→
    ///   清指针与缓存 → `Ok(None)`（"摘要失败"不改变"会话已关闭"这一事实）；
    /// - 会话仍活跃或状态未知 → 保留指针 → `Err`。
    ///
    /// 返回:
    /// - `None`: 无活跃会话 / 会话已关闭但摘要生成失败；
    /// - `Some(outcome)`: 本次调用完成封存（`sealed=false` 表示会话已被其他路径封存）。
    pub async fn close_active_session(&self) -> RamariaResult<Option<SealOutcome>> {
        let Some(session_id) = self.active_session_id() else {
            debug!("无活跃会话，跳过手动关闭");
            return Ok(None);
        };
        match self.engine.seal(session_id).await {
            Ok(outcome) => {
                self.set_active_session_id(None);
                self.forget_session(session_id);
                Ok(Some(outcome))
            }
            Err(e) => match self.engine.storage_ref().get_session(session_id).await {
                Ok(Some(session)) if session.ended_at.is_some() => {
                    warn!(
                        %session_id,
                        error = %e,
                        "会话已关闭但摘要生成失败（补偿任务已登记），清理指针"
                    );
                    self.set_active_session_id(None);
                    self.forget_session(session_id);
                    Ok(None)
                }
                _ => {
                    warn!(
                        %session_id,
                        error = %e,
                        "关闭活跃会话失败（会话仍活跃或状态未知），保留指针"
                    );
                    Err(e)
                }
            },
        }
    }

    // =========================================================
    // 级联检查与补扫（手动触发）
    // =========================================================

    /// 手动触发 L2 事件提取检查（全 persona 扫描 + L3 级联），与后台调度共用同一份实现。
    pub async fn check_l2_trigger(&self) {
        l2_l3::check_l2_trigger(&self.engine, Some(&self.shutdown_flag)).await;
    }

    /// 手动触发指定 persona 的 L3 性格推断检查。
    pub async fn check_l3_trigger(&self, persona_uid: &str) {
        l2_l3::check_l3_trigger(&self.engine, Some(&self.shutdown_flag), persona_uid).await;
    }

    /// 补扫封存失败遗留的 L1 摘要任务，返回本轮成功补跑的任务数。
    pub async fn retry_pending_l1_jobs(&self) -> usize {
        l1::retry_pending_l1_jobs(&self.engine).await
    }

    // =========================================================
    // 优雅关停
    // =========================================================

    /// 优雅关停：置停止位 → 关闭活跃会话（落库）→ 等待在途轮次结束。
    ///
    /// 顺序:
    /// 1. 置停止位并广播（等待中的后台循环立即唤醒退出）；
    /// 2. 关闭活跃会话（失败只记 error，不向上抛——退出流程不因单点失败中断）；
    /// 3. 取走三个循环句柄，各自按 15 秒上限等待（超时只 warn，不强杀任务）。
    ///
    /// 说明:
    /// - 可重复调用（第二次为空操作）；
    /// - 不等待启动期 L1 补扫任务（该任务自行感知停止位退出）；
    /// - 需要总时长可控时用 [`Self::shutdown_with_timeout`]。
    pub async fn shutdown(&self) {
        let _ = self.run_shutdown(None).await;
    }

    /// 限时优雅关停：封存与循环等待共享 `total` 时限，超时只记 warn。
    ///
    /// 预算:
    /// - 封存最多 `SHUTDOWN_SEAL_TIMEOUT_SECONDS` 秒：超时时会话若已关闭，摘要按
    ///   "已关闭但无 L1"登记补扫（下次启动或空闲轮次消费），不丢记忆；
    /// - 每个后台循环最多 `SHUTDOWN_LOOP_WAIT_MILLIS` 毫秒：收到广播后通常立即退出，
    ///   该上限只兜住正在封存的轮次。
    ///
    /// 参数:
    /// - `total`: 从调用开始计算的总等待上限。
    ///
    /// 返回:
    /// - 无返回值；正常收敛与超时都不向上抛错。
    ///
    /// 说明:
    /// - 可重复调用；
    /// - 超时后放弃等待在途工作（进程即将退出），本次调用不保证会话已落库。
    pub async fn shutdown_with_timeout(&self, total: Duration) {
        let deadline = Instant::now() + total;
        if !self.run_shutdown(Some(deadline)).await {
            warn!(
                timeout_millis = total.as_millis(),
                "生命周期关停未在时限内完成，放弃等待"
            );
        }
    }

    /// 关停主体：`deadline` 为空表示不设总时限（封存不限时、循环各自等待 15 秒）。
    ///
    /// 返回:
    /// - `true`: 封存与全部循环等待均在时限内收敛；
    /// - `false`: 有环节被放弃等待（调用方按需记录总时限告警）。
    async fn run_shutdown(&self, deadline: Option<Instant>) -> bool {
        info!("会话生命周期关停开始");
        self.shutdown_flag.store(true, Ordering::Release);
        self.shutdown_tx.send_replace(true);

        let mut converged = true;

        // ---- 关闭活跃会话（落库）----
        match deadline.map(|d| {
            d.saturating_duration_since(Instant::now())
                .min(Duration::from_secs(SHUTDOWN_SEAL_TIMEOUT_SECONDS))
        }) {
            None => match self.close_active_session().await {
                Ok(Some(outcome)) => info!(
                    sealed = outcome.sealed,
                    l1_count = outcome.l1_count,
                    "shutdown: 活跃会话已关闭"
                ),
                Ok(None) => debug!("shutdown: 无活跃会话（或已由其他路径完成）"),
                Err(e) => error!(error = %e, "shutdown: 关闭活跃会话失败（继续退出）"),
            },
            Some(budget) if budget.is_zero() => {
                warn!("shutdown: 总时限已耗尽，跳过活跃会话封存");
                converged = false;
            }
            Some(budget) => {
                match tokio::time::timeout(budget, self.close_active_session()).await {
                    Ok(Ok(Some(outcome))) => info!(
                        sealed = outcome.sealed,
                        l1_count = outcome.l1_count,
                        "shutdown: 活跃会话已关闭"
                    ),
                    Ok(Ok(None)) => debug!("shutdown: 无活跃会话（或已由其他路径完成）"),
                    Ok(Err(e)) => error!(error = %e, "shutdown: 关闭活跃会话失败（继续退出）"),
                    Err(_) => {
                        warn!(
                            timeout_millis = budget.as_millis(),
                            "shutdown: 封存未在时限内完成，登记 L1 补扫后继续退出"
                        );
                        // 会话可能已被封存第 1 步关闭但摘要缺失；补扫按"已有 L1 则跳过"幂等消费。
                        if let Some(session_id) = self.active_session_id() {
                            crate::seal::register_retry_if_closed(&self.engine, session_id).await;
                        }
                        converged = false;
                    }
                }
            }
        }

        // ---- 等待三个后台循环退出（共享截止时间；无时限时各自 15 秒）----
        let handles = [
            (
                "空闲检查循环",
                lock_recover(&self.idle_handle, "lifecycle.idle_handle").take(),
            ),
            (
                "L2/L3 定时检查循环",
                lock_recover(&self.l2_l3_handle, "lifecycle.l2_l3_handle").take(),
            ),
            (
                "主动对话调度循环",
                lock_recover(&self.proactive_handle, "lifecycle.proactive_handle").take(),
            ),
        ];
        for (label, handle) in handles {
            let Some(handle) = handle else {
                continue;
            };
            let cap = match deadline {
                Some(_) => Duration::from_millis(SHUTDOWN_LOOP_WAIT_MILLIS),
                None => Duration::from_secs(SHUTDOWN_WAIT_TIMEOUT_SECONDS),
            };
            let budget = match deadline {
                Some(d) => {
                    let remaining = d.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        warn!(loop_name = label, "shutdown: 总时限已耗尽，放弃等待该循环");
                        converged = false;
                        continue;
                    }
                    remaining.min(cap)
                }
                None => cap,
            };
            match tokio::time::timeout(budget, handle).await {
                Ok(Ok(())) => debug!(loop_name = label, "后台循环已退出"),
                Ok(Err(e)) => warn!(loop_name = label, error = %e, "后台循环异常结束"),
                Err(_) => {
                    warn!(
                        loop_name = label,
                        timeout_millis = budget.as_millis(),
                        "后台循环未在时限内退出，放弃等待"
                    );
                    converged = false;
                }
            }
        }

        info!("会话生命周期关停完成");
        converged
    }
}

/// 夹取配置侧的空闲检查间隔到下限（防配置误设过小造成热循环）。
///
/// 说明:
/// - 与 `crate::idle::IdleLoopOptions::from_config` 同口径：仅对配置值夹取并 warn；
///   显式覆盖值视为调用方自保证，不做下限夹取。
fn clamp_idle_interval(configured: u64) -> u64 {
    let interval = configured.max(MIN_IDLE_CHECK_INTERVAL_SECONDS);
    if interval != configured {
        warn!(
            configured,
            used = interval,
            "空闲检查间隔配置过小，已按下限夹取（避免热循环）"
        );
    }
    interval
}

/// 夹取配置侧的主动对话检查间隔到下限（防配置误设过小造成热循环）。
///
/// 说明:
/// - 与空闲检查同口径：仅对配置值夹取并 warn；显式覆盖值视为调用方自保证，不做下限夹取。
fn clamp_proactive_interval(configured: u64) -> u64 {
    let interval = configured.max(MIN_PROACTIVE_CHECK_INTERVAL_SECONDS);
    if interval != configured {
        warn!(
            configured,
            used = interval,
            "主动对话检查间隔配置过小，已按下限夹取（避免热循环）"
        );
    }
    interval
}
