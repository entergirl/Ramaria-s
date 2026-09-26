//! crates/ramaria-service/src/lifecycle/mod.rs - 会话生命周期能力（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）
//!
//! 设计特点:
//! - 与传输无关的生命周期容器：活跃指针、手动关闭、空闲检测、L2/L3 调度与关停统一装配
//! - 宿主差异全部由 [`LifecycleOptions`] 表达（长驻宿主 / 仅空闲检查 / 单次执行），后台循环按选项拉起
//! - `l1`：L1 摘要生成与重试（手动重生成、封存失败遗留任务的补扫消费点）
//! - `l2_l3`：L2 事件提取触发（含无主 L1 归属）与 L3 性格推断级联
//! - `idle`：空闲检查线程（阈值热更新、全库扫描、活跃指针一致性、状态刷新）
//! - 停止语义：共享原子停止位传入各循环，关停等待在途轮次收敛（超时只记日志，不阻塞退出）
//! - 降级纪律：LLM 不可用 / 单条数据失败均不阻塞级联与关停，仅记日志后继续

pub mod idle;
pub mod l1;
pub mod l2_l3;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::types::now_ms;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::engine::Engine;
use crate::idle::MIN_IDLE_CHECK_INTERVAL_SECONDS;
use crate::types::SealOutcome;

// =========================================================
// 装配常量
// =========================================================

/// L2/L3 定时调度首轮检查的默认延迟（秒）：避开宿主启动阶段。
const L2_L3_DEFAULT_FIRST_DELAY_SECONDS: u64 = 300;

/// 启动期 L1 补扫的延迟（秒）：先让索引构建与首轮对话完成。
const STARTUP_L1_RETRY_DELAY_SECONDS: u64 = 30;

/// 关停等待后台循环收敛的上限（秒）：超过后放弃等待（进程即将退出，不强杀任务）。
const SHUTDOWN_WAIT_TIMEOUT_SECONDS: u64 = 15;

// =========================================================
// 装配选项
// =========================================================

/// 生命周期装配选项（宿主差异全部由此表达）。
///
/// 字段约定:
/// - `idle`: 是否拉起空闲检查循环（超时会话自动封存）；
/// - `l2_l3`: 是否拉起 L2/L3 定时调度循环；
/// - `startup_l1_retry`: 是否执行启动期一次 L1 补扫（延迟后先检查停止位再执行）；
/// - `idle_interval_seconds`: 空闲检查间隔覆盖值；`None` 取
///   `[session].idle_check_interval_seconds`（并夹取到 [`MIN_IDLE_CHECK_INTERVAL_SECONDS`]）；
///   显式值不做下限夹取（调用方保证不小于 1 秒，测试与特殊宿主使用）；
/// - `l2_l3_interval_seconds`: L2/L3 检查间隔覆盖值；`None` 取
///   `[session].l2_check_interval_seconds`；
/// - `l2_l3_first_delay_seconds`: L2/L3 首轮检查延迟覆盖值；`None` 取默认 300 秒。
#[derive(Debug, Clone)]
pub struct LifecycleOptions {
    pub idle: bool,
    pub l2_l3: bool,
    pub startup_l1_retry: bool,
    pub idle_interval_seconds: Option<u64>,
    pub l2_l3_interval_seconds: Option<u64>,
    pub l2_l3_first_delay_seconds: Option<u64>,
}

impl LifecycleOptions {
    /// 桌面宿主：空闲检查 + L2/L3 调度 + 启动期 L1 补扫全开（长驻宿主，行为最全）。
    pub fn desktop() -> Self {
        Self {
            idle: true,
            l2_l3: true,
            startup_l1_retry: true,
            idle_interval_seconds: None,
            l2_l3_interval_seconds: None,
            l2_l3_first_delay_seconds: None,
        }
    }

    /// MCP 宿主：仅空闲检查（不拉起 L2/L3 调度，也不做启动期补扫）。
    pub fn mcp() -> Self {
        Self {
            idle: true,
            l2_l3: false,
            startup_l1_retry: false,
            idle_interval_seconds: None,
            l2_l3_interval_seconds: None,
            l2_l3_first_delay_seconds: None,
        }
    }

    /// 单次执行宿主：不拉起任何后台循环。
    pub fn none() -> Self {
        Self {
            idle: false,
            l2_l3: false,
            startup_l1_retry: false,
            idle_interval_seconds: None,
            l2_l3_interval_seconds: None,
            l2_l3_first_delay_seconds: None,
        }
    }

    /// 覆盖空闲检查间隔（秒；显式值不做下限夹取）。
    pub fn with_idle_interval(mut self, seconds: u64) -> Self {
        self.idle_interval_seconds = Some(seconds);
        self
    }

    /// 覆盖 L2/L3 检查间隔（秒）。
    pub fn with_l2_l3_interval(mut self, seconds: u64) -> Self {
        self.l2_l3_interval_seconds = Some(seconds);
        self
    }

    /// 覆盖 L2/L3 首轮检查延迟（秒）。
    pub fn with_l2_l3_first_delay(mut self, seconds: u64) -> Self {
        self.l2_l3_first_delay_seconds = Some(seconds);
        self
    }
}

impl Default for LifecycleOptions {
    /// 缺省按桌面宿主口径（长驻宿主，行为最全）。
    fn default() -> Self {
        Self::desktop()
    }
}

// =========================================================
// 生命周期容器
// =========================================================

/// 会话生命周期容器：活跃指针、手动关闭、空闲检测、L2/L3 调度与关停。
///
/// 职责:
/// - 活跃会话指针与各会话最后活跃时间的内存缓存（宿主每条消息落库后调用 `touch_session`）；
/// - 手动关闭活跃会话（抢占式封存，同一会话只生成一份摘要）；
/// - 按 [`LifecycleOptions`] 拉起空闲检查线程、L2/L3 调度与启动期 L1 补扫；
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
    /// 空闲阈值（分钟，热更新；空闲检查每轮读取最新值）。
    idle_minutes: Arc<AtomicU32>,
    /// 空闲检查循环句柄（None = 未拉起，或关停时已取走等待）。
    idle_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// L2/L3 调度循环句柄（None = 未拉起，或关停时已取走等待）。
    l2_l3_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
        let lifecycle = Arc::new(Self {
            idle_minutes: Arc::new(AtomicU32::new(engine.config().session.l1_idle_minutes)),
            engine: Arc::clone(&engine),
            active_session_id: Mutex::new(None),
            session_last_active: Mutex::new(HashMap::new()),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            idle_handle: Mutex::new(None),
            l2_l3_handle: Mutex::new(None),
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
                first_delay_seconds,
                interval_seconds,
            );
            *lock_recover(&lifecycle.l2_l3_handle, "lifecycle.l2_l3_handle") = Some(handle);
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
    /// 1. 置 `shutdown_flag`（后台循环在下一轮感知后退出）；
    /// 2. 关闭活跃会话（失败只记 error，不向上抛——退出流程不因单点失败中断）；
    /// 3. 取走两个循环句柄，各自按 15 秒上限等待（超时只 warn，不强杀任务）。
    ///
    /// 说明:
    /// - 可重复调用（第二次为空操作）；
    /// - 不等待启动期 L1 补扫任务（该任务自行感知停止位退出）。
    pub async fn shutdown(&self) {
        info!("会话生命周期关停开始");
        self.shutdown_flag.store(true, Ordering::Release);

        match self.close_active_session().await {
            Ok(Some(outcome)) => info!(
                sealed = outcome.sealed,
                l1_count = outcome.l1_count,
                "shutdown: 活跃会话已关闭"
            ),
            Ok(None) => debug!("shutdown: 无活跃会话（或已由其他路径完成）"),
            Err(e) => error!(error = %e, "shutdown: 关闭活跃会话失败（继续退出）"),
        }

        let idle_handle = {
            let mut guard = lock_recover(&self.idle_handle, "lifecycle.idle_handle");
            guard.take()
        };
        if let Some(handle) = idle_handle {
            let timeout = Duration::from_secs(SHUTDOWN_WAIT_TIMEOUT_SECONDS);
            match tokio::time::timeout(timeout, handle).await {
                Ok(Ok(())) => debug!("空闲检查循环已退出"),
                Ok(Err(e)) => warn!(error = %e, "空闲检查循环异常结束"),
                Err(_) => warn!(
                    timeout_seconds = SHUTDOWN_WAIT_TIMEOUT_SECONDS,
                    "空闲检查循环未在超时内退出（可能正在封存），放弃等待"
                ),
            }
        }

        let l2_l3_handle = {
            let mut guard = lock_recover(&self.l2_l3_handle, "lifecycle.l2_l3_handle");
            guard.take()
        };
        if let Some(handle) = l2_l3_handle {
            let timeout = Duration::from_secs(SHUTDOWN_WAIT_TIMEOUT_SECONDS);
            match tokio::time::timeout(timeout, handle).await {
                Ok(Ok(())) => debug!("L2/L3 定时检查循环已退出"),
                Ok(Err(e)) => warn!(error = %e, "L2/L3 定时检查循环异常结束"),
                Err(_) => warn!(
                    timeout_seconds = SHUTDOWN_WAIT_TIMEOUT_SECONDS,
                    "L2/L3 定时检查循环未在超时内退出，放弃等待"
                ),
            }
        }

        info!("会话生命周期关停完成");
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

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        DeterministicEmbedding, L1_JSON_REPLY, MockLlm, engine_with_db, engine_with_l1_reply,
        engine_with_llm_config_and_embedding, seed_persona, seed_session_with_messages,
    };
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
    use ramaria_core::types::{AppState, BackendConfig, MemoryL1};
    use std::time::Instant;

    /// 测试用装配选项：空闲与 L2/L3 均以 1 秒轮次运行（关停等待收敛在 1 秒量级），
    /// 首轮延迟 1 秒，其余按桌面口径。
    fn test_options() -> LifecycleOptions {
        LifecycleOptions::desktop()
            .with_idle_interval(1)
            .with_l2_l3_interval(1)
            .with_l2_l3_first_delay(1)
    }

    /// 活跃指针与最后活跃缓存：set / get / forget / clear_active_if。
    #[tokio::test]
    async fn active_pointer_and_last_active_cache() {
        let (engine, _storage, dir) = engine_with_db("life-pointer-basic").await;
        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(LifecycleOptions::none());

        let sid = Uuid::new_v4();
        assert!(lifecycle.active_session_id().is_none(), "初始无活跃指针");
        lifecycle.set_active_session_id(Some(sid));
        assert_eq!(lifecycle.active_session_id(), Some(sid));
        lifecycle.set_active_session_id(None);
        assert!(lifecycle.active_session_id().is_none());

        // touch / last_active / forget
        lifecycle.touch_session(sid);
        assert!(
            lifecycle.last_active(sid).is_some_and(|t| t > 0),
            "touch 后应能读到活跃时间"
        );
        lifecycle.forget_session(sid);
        assert!(lifecycle.last_active(sid).is_none(), "forget 后缓存应清空");

        // clear_active_if：仅指针指向该会话时清空
        let other = Uuid::new_v4();
        lifecycle.set_active_session_id(Some(sid));
        lifecycle.touch_session(sid);
        lifecycle.clear_active_if(other);
        assert_eq!(
            lifecycle.active_session_id(),
            Some(sid),
            "指针未指向该会话时不应清空指针"
        );
        lifecycle.clear_active_if(sid);
        assert!(
            lifecycle.active_session_id().is_none(),
            "指针指向该会话时应清空"
        );
        assert!(lifecycle.last_active(sid).is_none(), "缓存应一并清理");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// close_active_session：无活跃 → Ok(None)；有活跃 → 关闭 + 1 条 L1 + 指针与缓存清空；
    /// 再次调用 → Ok(None)。
    #[tokio::test]
    async fn close_active_session_seals_and_clears_pointer() {
        let (engine, storage, dir) = engine_with_l1_reply("life-close", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;
        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(LifecycleOptions::none());

        // 无活跃会话 → Ok(None)
        assert!(
            lifecycle
                .close_active_session()
                .await
                .expect("无活跃会话应正常返回")
                .is_none()
        );

        lifecycle.set_active_session_id(Some(session));
        lifecycle.touch_session(session);
        let outcome = lifecycle
            .close_active_session()
            .await
            .expect("关闭应成功")
            .expect("应有封存结果");
        assert_eq!(outcome.session_id, session);
        assert!(outcome.sealed, "首次封存应抢到关闭权");
        assert_eq!(outcome.l1_count, 1, "短会话应生成单条 L1");

        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_some(), "会话应被关闭");
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "应恰好一条 L1"
        );
        assert!(lifecycle.active_session_id().is_none(), "指针应被清空");
        assert!(lifecycle.last_active(session).is_none(), "缓存应被清空");

        // 再次调用：无活跃会话 → Ok(None)
        assert!(
            lifecycle
                .close_active_session()
                .await
                .expect("重复关闭应正常返回")
                .is_none()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// close_active_session 且 LLM 恒失败：会话仍被关闭、指针清空、返回 Ok(None)、
    /// 库中登记 l1_summary_retry 补偿任务。
    #[tokio::test]
    async fn close_active_session_keeps_ok_when_l1_fails() {
        let (engine, storage, dir) =
            crate::test_support::engine_with_failing_llm("life-close-fail").await;
        seed_persona(&storage, "char-0001").await;
        let session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;
        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
        lifecycle.set_active_session_id(Some(session));
        lifecycle.touch_session(session);

        // 摘要生成失败但会话已收尾 → 复查后按"已关闭"处理，不向上抛
        let result = lifecycle
            .close_active_session()
            .await
            .expect("会话已关闭时应返回 Ok(None)");
        assert!(result.is_none(), "摘要失败但会话已关闭 → 返回 None");

        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_some(), "摘要失败不改变会话已关闭的事实");
        assert!(lifecycle.active_session_id().is_none(), "指针应被清空");
        assert!(lifecycle.last_active(session).is_none(), "缓存应被清空");
        let pending = storage.list_pending_jobs().await.expect("查询任务应成功");
        assert!(
            pending
                .iter()
                .any(|(_, job_type, _)| job_type == "l1_summary_retry"),
            "摘要失败应登记补偿任务: {pending:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空闲线程：超时会话被自动封存（关闭 + L1）→ shutdown 后循环不再运行，可重复调用。
    #[tokio::test]
    async fn idle_thread_seals_timed_out_session_then_shutdown_stops() {
        let (engine, storage, dir) = engine_with_l1_reply("life-idle-thread", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
        let engine = Arc::new(engine);

        let lifecycle = engine.start_lifecycle(test_options());
        assert!(
            lifecycle.idle_loop_running(),
            "拉起后空闲循环应处于运行状态"
        );

        // 轮询等待自动封存（上限 6 秒）：同时要求 L1 落库，避免命中"已关闭未写摘要"的窗口
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let row = storage
                .get_session(session)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在");
            let l1_count = storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len();
            if row.ended_at.is_some() && l1_count == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "空闲线程应在限时内封存超时会话（closed={}, l1={l1_count}）",
                row.ended_at.is_some()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        lifecycle.shutdown().await;
        assert!(!lifecycle.idle_loop_running(), "关停后空闲循环不应再运行");
        // 重复关停为空操作（不阻塞、不报错）
        lifecycle.shutdown().await;
        assert!(!lifecycle.idle_loop_running(), "重复关停后仍不应运行");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 阈值热更新：默认阈值下 5 分钟前活跃的会话不被封存；热更新到 1 分钟后在轮询上限内被封存。
    #[tokio::test]
    async fn idle_threshold_hot_update_takes_effect() {
        let (engine, storage, dir) = engine_with_l1_reply("life-hot", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        // 5 分钟前活跃：默认阈值 10 分钟下未超时
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 5 * 60_000).await;
        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(test_options());

        // 默认阈值：手动跑一轮（与后台线程同一实现）→ 不封存
        assert_eq!(
            lifecycle.tick_idle().await.expect("空闲检查应成功"),
            0,
            "默认 10 分钟阈值下 5 分钟活跃不应封存"
        );
        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_none(), "默认阈值下会话应保持活跃");

        // 热更新到 1 分钟 → 下一轮即生效（后台线程在轮询上限内完成封存）
        lifecycle.set_idle_minutes(1);
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let row = storage
                .get_session(session)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在");
            if row.ended_at.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "热更新后空闲线程应在限时内封存超时会话"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        lifecycle.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 活跃指针一致性：指针指向的会话被空闲线程封存 → 下一轮后指针为 None。
    #[tokio::test]
    async fn active_pointer_cleared_after_idle_seal() {
        let (engine, storage, dir) = engine_with_l1_reply("life-pointer-seal", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session =
            seed_session_with_messages(&storage, "char-0001", 2, now_ms() - 20 * 60_000).await;
        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(test_options());
        lifecycle.set_active_session_id(Some(session));
        lifecycle.touch_session(session);

        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if lifecycle.active_session_id().is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "空闲封存后活跃指针应被一致性清理"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            lifecycle.last_active(session).is_none(),
            "指针清理时缓存应一并清理"
        );
        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_some(), "超时会话应已被空闲线程封存");

        lifecycle.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状态机与事实对齐：空闲轮次后按配置完整度推进到 Ready / Indexing。
    #[tokio::test]
    async fn idle_tick_aligns_state_with_facts() {
        // 引擎 1：配置完整 + 索引已建 + 嵌入可用 → 空闲轮次后推进到 Ready
        let (engine, storage, dir) = engine_with_llm_config_and_embedding(
            "life-state-ready",
            MockLlm::with_reply(L1_JSON_REPLY),
            RamariaConfig::default(),
            Some(Arc::new(DeterministicEmbedding::new())),
        )
        .await;
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");
        storage
            .set_index_version(1)
            .await
            .expect("写入索引版本应成功");
        let engine = Arc::new(engine);
        assert_eq!(
            engine.current_state(),
            AppState::NeedsSetup,
            "装配初值应为 NeedsSetup"
        );

        let lifecycle = engine.start_lifecycle(test_options());
        let deadline = Instant::now() + Duration::from_secs(6);
        while engine.current_state() != AppState::Ready {
            assert!(
                Instant::now() < deadline,
                "空闲轮次应在限时内把状态推进到 Ready（当前 {:?}）",
                engine.current_state()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        lifecycle.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);

        // 引擎 2：同配置但索引未建（index_version=0）→ 判定为 Indexing
        let (engine, storage, dir) = engine_with_llm_config_and_embedding(
            "life-state-indexing",
            MockLlm::with_reply(L1_JSON_REPLY),
            RamariaConfig::default(),
            Some(Arc::new(DeterministicEmbedding::new())),
        )
        .await;
        storage
            .save_backend_config(&BackendConfig::lm_studio_default())
            .await
            .expect("保存后端配置应成功");
        storage
            .set_index_version(0)
            .await
            .expect("写入索引版本应成功");
        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
        assert_eq!(lifecycle.tick_idle().await.expect("空闲检查应成功"), 0);
        assert_eq!(
            engine.current_state(),
            AppState::Indexing,
            "索引待构建应判定为 Indexing"
        );
        lifecycle.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// MCP 装配：仅空闲循环；不开 L2/L3 时无主 L1 在空闲轮次后仍保持无主。
    /// none() 选项不拉起任何循环。
    #[tokio::test]
    async fn mcp_options_start_idle_only_and_skip_l2() {
        let mut config = RamariaConfig::default();
        // 只要发生 L2 检查，无主 L1 就会被归属回填（阈值 1）
        config.thresholds.l2_trigger_count = 1;
        config.thresholds.cluster_delay_ms = 0;
        let (engine, storage, dir) = crate::test_support::engine_with_llm_and_config(
            "life-mcp",
            MockLlm::with_reply(L1_JSON_REPLY),
            config,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        // 已关闭会话 + 一条无主 L1（无主 L1 属此类由导入产生）
        let session = storage
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        storage
            .close_session(session.id)
            .await
            .expect("关闭会话应成功");
        let l1 = MemoryL1::new(session.id, "导入会话摘要内容".to_string(), None);
        storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(LifecycleOptions::mcp().with_idle_interval(1));
        assert!(lifecycle.idle_loop_running(), "mcp 选项应拉起空闲循环");
        assert!(!lifecycle.l2_l3_running(), "mcp 选项不应拉起 L2/L3 调度");

        // 跑一轮空闲检查（与后台线程同一实现）：无主 L1 保持无主
        assert_eq!(lifecycle.tick_idle().await.expect("空闲检查应成功"), 0);
        let unbound = storage
            .list_unabsorbed_l1_unbound()
            .await
            .expect("查询无主 L1 应成功");
        assert_eq!(unbound.len(), 1, "不开 L2/L3 时无主 L1 不应被归属");
        assert!(unbound[0].persona_uid.is_none(), "无主 L1 归属不应被回填");

        lifecycle.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);

        // none() 选项：不拉起任何后台循环
        let (engine, _storage, dir) = engine_with_db("life-none").await;
        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
        assert!(!lifecycle.idle_loop_running(), "none 选项不应拉起空闲循环");
        assert!(!lifecycle.l2_l3_running(), "none 选项不应拉起 L2/L3 调度");
        lifecycle.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// shutdown 落库：关闭活跃会话（生成 L1）、置停止位、两个循环不再运行。
    #[tokio::test]
    async fn shutdown_closes_active_session_and_stops_loops() {
        let (engine, storage, dir) = engine_with_l1_reply("life-shutdown", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        // 刚活跃的会话：空闲轮次不会封存它，由 shutdown 的关闭路径收尾
        let session = seed_session_with_messages(&storage, "char-0001", 4, now_ms()).await;
        let engine = Arc::new(engine);

        let lifecycle = engine.start_lifecycle(test_options());
        lifecycle.set_active_session_id(Some(session));
        lifecycle.touch_session(session);
        assert!(lifecycle.idle_loop_running(), "拉起后空闲循环应运行");
        assert!(lifecycle.l2_l3_running(), "拉起后 L2/L3 循环应运行");

        lifecycle.shutdown().await;

        assert!(
            lifecycle.shutdown_flag().load(Ordering::Acquire),
            "停止位应已置位"
        );
        assert!(!lifecycle.idle_loop_running(), "关停后空闲循环不应再运行");
        assert!(!lifecycle.l2_l3_running(), "关停后 L2/L3 循环不应再运行");
        assert!(lifecycle.active_session_id().is_none(), "指针应被清空");

        let row = storage
            .get_session(session)
            .await
            .expect("查询会话应成功")
            .expect("会话应存在");
        assert!(row.ended_at.is_some(), "shutdown 应关闭活跃会话");
        assert_eq!(
            storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "shutdown 应生成 L1 摘要"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 封存结果对照：手动关闭（Lifecycle）与引擎封存（Engine::seal）产出等价摘要。
    #[tokio::test]
    async fn lifecycle_close_matches_engine_seal() {
        let (engine, storage, dir) = engine_with_l1_reply("life-compare", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_a = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;
        let session_b = seed_session_with_messages(&storage, "char-0001", 4, 1_000).await;

        let engine = Arc::new(engine);
        let lifecycle = engine.start_lifecycle(LifecycleOptions::none());
        lifecycle.set_active_session_id(Some(session_a));

        let outcome_a = lifecycle
            .close_active_session()
            .await
            .expect("手动关闭应成功")
            .expect("应有封存结果");
        assert!(outcome_a.sealed, "会话 A 应由本次调用封存");
        let outcome_b = engine.seal(session_b).await.expect("引擎封存应成功");
        assert!(outcome_b.sealed, "会话 B 应由本次调用封存");

        // 两个会话都关闭、各恰好 1 条 L1
        for sid in [session_a, session_b] {
            let row = storage
                .get_session(sid)
                .await
                .expect("查询会话应成功")
                .expect("会话应存在");
            assert!(row.ended_at.is_some(), "会话 {sid} 应被关闭");
            assert_eq!(
                storage
                    .list_memory_l1(sid)
                    .await
                    .expect("读取 L1 应成功")
                    .len(),
                1,
                "会话 {sid} 应恰好一条 L1"
            );
        }

        // 关键字段逐项一致（同一 mock LLM 回复 → 摘要素材一致）
        let l1_a = storage
            .list_memory_l1(session_a)
            .await
            .expect("读取 L1 应成功")
            .pop()
            .expect("会话 A 应有 L1");
        let l1_b = storage
            .list_memory_l1(session_b)
            .await
            .expect("读取 L1 应成功")
            .pop()
            .expect("会话 B 应有 L1");
        assert_eq!(l1_a.summary, l1_b.summary, "summary 应一致");
        assert_eq!(l1_a.keywords, l1_b.keywords, "keywords 应一致");
        assert_eq!(l1_a.persona_uid, l1_b.persona_uid, "persona_uid 应一致");
        assert_eq!(l1_a.valence, l1_b.valence, "valence 应一致");
        assert_eq!(l1_a.salience, l1_b.salience, "salience 应一致");
        assert_eq!(l1_a.absorbed, l1_b.absorbed, "absorbed 应一致");

        // serde 归一化对照：两条摘要的序列化字段集合与内容一致
        let value_a = serde_json::to_value(&l1_a).expect("序列化应成功");
        let value_b = serde_json::to_value(&l1_b).expect("序列化应成功");
        assert_eq!(value_a["summary"], value_b["summary"]);
        assert_eq!(value_a["keywords"], value_b["keywords"]);
        assert_eq!(value_a["persona_uid"], value_b["persona_uid"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
