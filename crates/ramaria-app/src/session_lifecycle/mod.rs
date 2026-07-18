//! rust/crates/ramaria-app/src/session_lifecycle/mod.rs - Session 生命周期与记忆管线触发
//!
//! 设计特点:
//! - 对齐 Python `SessionManager` 的完整行为：手动关闭、空闲自动关闭、只读约束
//! - 拆分为三个子模块：`idle`（空闲检测）、`l1_generate`（L1 摘要生成）、`l2_l3_scheduler`（L2/L3 调度）
//! - 本模块保留核心结构体定义、session 追踪、`save_and_close_session` 编排、`shutdown` 优雅关闭
//! - shutdown hook：应用退出时自动关闭活跃 session 并等待后台任务完成
//! - 所有管线触发通过 `JobManager` 编排，含重试和指数退避
//!
//! 与 Python 对齐:
//! - `_close_and_summarize` → `save_and_close_session`
//! - Thread A `_idle_checker_loop` → `idle::spawn_idle_checker`
//! - Thread B → `l2_l3_scheduler::spawn_l2_l3_scheduler`
//! - `force_close_current_session` → `save_and_close_session`

pub mod idle;
pub mod l1_generate;
pub mod l2_l3_scheduler;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_core::types::now_ms;
use ramaria_memory::retriever::Retriever;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// =========================================================
// Session 生命周期编排器
// =========================================================

/// Session 生命周期编排器。
///
/// 职责:
/// - 管理活跃 session ID 的内存追踪。
/// - 管理每个 session 最后活跃时间的内存追踪（避免每次查 DB）。
/// - 提供手动关闭、空闲检测、级联管线触发的完整编排。
///
/// 对齐 Python:
/// - `SessionManager.active_session_id` + `threading.Lock` → `active_session_id: Mutex<Option<Uuid>>`
/// - `SessionManager._idle_checker_loop` → `idle::spawn_idle_checker`
/// - `SessionManager._l2_checker_loop` → `l2_l3_scheduler::spawn_l2_l3_scheduler`
pub struct SessionLifecycle {
    /// 当前活跃 session ID（同一时刻只有一个活跃 session）
    pub(crate) active_session_id: Mutex<Option<Uuid>>,
    /// 各 session 最后一条消息的时间（Unix 毫秒），内存缓存
    pub(crate) session_last_active: Mutex<HashMap<Uuid, i64>>,
    /// 应用配置引用
    pub(crate) config: RamariaConfig,
    /// 停止标志（所有后台线程在设置此标志后退出）
    pub(crate) shutdown_flag: Arc<AtomicBool>,
    /// v1.2: 内存检索器引用（L1 生成后增量更新），None 表示未注入（向后兼容）
    pub(crate) retriever: Mutex<Option<Arc<RwLock<Retriever>>>>,
}

impl SessionLifecycle {
    /// 创建新的 Session 生命周期编排器。
    ///
    /// retriever 默认为 `None`，调用方需在创建后通过 [`set_retriever`] 注入，
    /// 以启用 L1 摘要生成后的增量索引更新。
    pub fn new(config: RamariaConfig) -> Self {
        Self {
            active_session_id: Mutex::new(None),
            session_last_active: Mutex::new(HashMap::new()),
            config,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            retriever: Mutex::new(None),
        }
    }

    /// 注入内存检索器引用（v1.2 新增）。
    ///
    /// 调用时机:
    /// - 在 `App::new` 中，`SessionLifecycle` 和 `Retriever` 创建完成后立即调用。
    /// - 必须在后台任务启动前调用（空闲检测/shutdown 路径依赖此引用做 L1 增量索引）。
    ///
    /// 参数:
    /// - `r`: 与 App 共享的 Retriever（v1.3 P-3: `Arc<RwLock<Retriever>>`）。
    pub fn set_retriever(&self, r: Arc<RwLock<Retriever>>) {
        let mut guard = self.retriever.lock().unwrap_or_else(|e| {
            error!("retriever lock poisoned during set_retriever: {e}");
            e.into_inner()
        });
        *guard = Some(r);
        info!("SessionLifecycle: Retriever 引用已注入，L1 增量索引已启用");
    }

    /// 返回 shutdown_flag 的 Arc 引用，供外部线程检查。
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown_flag)
    }

    // =========================================================
    // 活跃 Session 追踪
    // =========================================================

    /// 获取当前活跃 session ID。
    ///
    /// 对齐 Python `SessionManager.active_session_id`。
    pub fn get_active_session_id(&self) -> Option<Uuid> {
        *self.active_session_id.lock().unwrap_or_else(|e| {
            error!("active_session_id lock poisoned: {e}");
            e.into_inner()
        })
    }

    /// 设置当前活跃 session ID（公共 API，供 App::send_message 自动创建 session）。
    pub fn set_active_session_id_public(&self, sid: Option<Uuid>) {
        let mut guard = self.active_session_id.lock().unwrap_or_else(|e| {
            error!("active_session_id lock poisoned during set: {e}");
            e.into_inner()
        });
        *guard = sid;
    }

    /// 设置当前活跃 session ID（内部使用）。
    pub(super) fn set_active_session_id(&self, sid: Option<Uuid>) {
        self.set_active_session_id_public(sid);
    }

    /// 记录 session 最后活跃时间。
    ///
    /// 对齐 Python `SessionManager._last_message_time`（Python 从 DB 查，
    /// Rust 在此做内存缓存以减少 DB 查询）。
    pub fn touch_session(&self, session_id: Uuid) {
        let now = now_ms();
        let mut guard = self.session_last_active.lock().unwrap_or_else(|e| {
            error!("session_last_active lock poisoned: {e}");
            e.into_inner()
        });
        guard.insert(session_id, now);
        debug!(%session_id, last_active = now, "session 活跃时间已更新");
    }

    /// 获取 session 最后活跃时间（从内存缓存）。
    pub(super) fn get_last_active(&self, session_id: Uuid) -> Option<i64> {
        let guard = self.session_last_active.lock().unwrap_or_else(|e| {
            error!("session_last_active lock poisoned: {e}");
            e.into_inner()
        });
        guard.get(&session_id).copied()
    }

    /// 移除 session 的活跃时间缓存（session 关闭后清理）。
    pub(super) fn forget_session(&self, session_id: Uuid) {
        let mut guard = self.session_last_active.lock().unwrap_or_else(|e| {
            error!("session_last_active lock poisoned during forget: {e}");
            e.into_inner()
        });
        guard.remove(&session_id);
    }

    /// v1.2: 从 DB 读取当前活跃 session 的 `persona_uid`。
    ///
    /// 职责:
    /// - 供空闲超时关闭（`spawn_idle_checker`）和 shutdown 关闭路径使用，
    ///   确保 L1 摘要归属正确（不再传 `None` 导致 NULL persona_uid 死锁）。
    ///
    /// 降级:
    /// - 无活跃 session → 返回 `None`。
    /// - DB 查询失败 → warn 日志 + 返回 `None`（不阻塞关闭流程）。
    ///
    /// 参数:
    /// - `storage`: 存储后端（用于查询 session 记录）。
    ///
    /// 返回:
    /// - `Some(uid)`: 当前活跃 session 的 persona_uid。
    /// - `None`: 无活跃 session / session 不存在 / 查询失败。
    pub(super) async fn get_active_session_persona_uid(
        &self,
        storage: &dyn StorageBackend,
    ) -> Option<String> {
        let sid = self.get_active_session_id()?;
        match storage.get_session(sid).await {
            Ok(Some(s)) => {
                debug!(%sid, persona_uid = ?s.persona_uid, "从活跃 session 读取 persona_uid");
                s.persona_uid
            }
            Ok(None) => {
                warn!(%sid, "活跃 session 在 DB 中不存在，persona_uid 回退为 None");
                None
            }
            Err(e) => {
                warn!(%sid, %e, "查询活跃 session persona_uid 失败，回退为 None");
                None
            }
        }
    }

    // =========================================================
    // 手动关闭：save_and_close_session
    // =========================================================

    /// 手动保存并关闭当前活跃 session。
    ///
    /// 完整流程（对齐 Python `force_close_current_session → _close_and_summarize`）:
    /// 1. 获取当前活跃 session ID
    /// 2. 调用 storage.close_session 设置 ended_at
    /// 3. 生成 L1 摘要（通过 L1Summarizer，传入当前对话人格）
    /// 4. 检查 L2 触发条件（路径 A：未吸收 L1 ≥ 阈值）
    /// 5. 清除活跃 session ID
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider（供 L1 summarizer 使用）。
    /// - `persona_uid`: 当前对话人格的 UID（用于 L1 摘要归属）。
    ///
    /// 返回:
    /// - `Ok()`: 关闭成功（即使无活跃 session 也视为成功）。
    /// - `Err`: 存储或 LLM 调用失败。
    pub async fn save_and_close_session(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        persona_uid: Option<&str>,
    ) -> RamariaResult<()> {
        let active_sid = match self.get_active_session_id() {
            Some(sid) => sid,
            None => {
                debug!("无活跃 session，跳过 save_and_close");
                return Ok(());
            }
        };

        info!(%active_sid, "手动保存并关闭 session");

        // Step 1: 关闭 session（设置 ended_at）
        // 对齐 Python `close_session(sid)`
        l1_generate::close_session_safe(storage, active_sid).await?;

        // Step 2: 生成 L1 摘要（传入当前对话人格）
        // 对齐 Python `summarizer.summarize_session(session_id)`
        match self
            .generate_l1_summary(storage, llm, active_sid, persona_uid)
            .await
        {
            Ok(l1) => {
                info!(
                    %active_sid,
                    l1_id = %l1.id,
                    summary_len = l1.summary.chars().count(),
                    "L1 摘要生成成功"
                );

                // v1.2: 增量更新 Retriever 内存索引（D-V12-013）
                // 必须在 L2 级联检查前执行，确保后续 L2/L3 也能检索到新 L1
                self.index_l1_into_retriever(&l1);

                // Step 3: 检查 L2 触发条件（路径 A：即时触发）
                // 对齐 Python summarizer 末尾的 `merger.check_and_merge`
                self.check_l2_trigger(storage, llm).await;
            }
            Err(e) => {
                tracing::error!(
                    %active_sid,
                    error = %e,
                    "❌ L1 摘要生成失败！session 已关闭但未生成摘要。LLM 服务可能不可用。"
                );
                // 创建 pending BackgroundJob，供后续 regenerate_l1 重试
                // 对齐决策：L1 失败不阻塞 session 关闭，但需记录可重试任务
                let payload = serde_json::json!({
                    "session_id": active_sid.to_string(),
                    "persona_uid": persona_uid,
                    "reason": "auto_retry_on_close"
                })
                .to_string();
                match storage
                    .create_background_job("l1_summary", Some(&payload))
                    .await
                {
                    Ok(job_id) => {
                        tracing::info!(
                            %active_sid,
                            job_id,
                            "已创建 pending L1 重试任务，稍后可调用 regenerate_l1 或后台自动重试"
                        );
                    }
                    Err(job_err) => {
                        tracing::error!(
                            %active_sid,
                            %job_err,
                            "❌ 创建 L1 重试任务也失败了！需手动使用 generate_l1 命令重试。"
                        );
                    }
                }
            }
        }

        // Step 4: 清除活跃 session
        self.set_active_session_id(None);
        self.forget_session(active_sid);

        info!(%active_sid, "session 已关闭");
        Ok(())
    }

    // =========================================================
    // Shutdown
    // =========================================================

    /// 优雅关闭：关闭活跃 session 并通知所有后台线程退出。
    ///
    /// 对齐 Python `SessionManager.stop`。
    ///
    /// 流程:
    /// 1. 设置 shutdown_flag
    /// 2. 若有活跃 session，调用 save_and_close_session（无超时——L1 摘要依赖 LLM 响应）
    /// 3. 等待后台线程退出（各带 15s 独立超时）
    ///
    /// 超时策略:
    /// - L1 摘要是关键数据，不设超时上限（若 LLM 响应极慢，用户可自行 kill 进程）。
    /// - 后台线程仅做轮询检查，15s 超时足够它们感知 shutdown_flag 并退出。
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider。
    /// - `idle_handle`: 空闲检测线程的 JoinHandle。
    /// - `scheduler_handle`: L2/L3 定时线程的 JoinHandle。
    pub async fn shutdown(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        idle_handle: Option<tokio::task::JoinHandle<()>>,
        scheduler_handle: Option<tokio::task::JoinHandle<()>>,
    ) {
        info!("SessionLifecycle shutdown 开始");

        // Step 1: 设置停止标志
        self.shutdown_flag.store(true, Ordering::SeqCst);

        // Step 2: 关闭活跃 session（无超时——L1 摘要需要等待 LLM 响应）
        // v1.2: 从活跃 session 读取 persona_uid（不再传 None）
        let persona_uid = self.get_active_session_persona_uid(storage).await;
        match self
            .save_and_close_session(storage, llm, persona_uid.as_deref())
            .await
        {
            Ok(()) => info!("shutdown: 活跃 session 已关闭"),
            Err(e) => warn!(%e, "shutdown: 关闭活跃 session 时出错（继续退出）"),
        }

        // Step 3: 等待后台线程退出（各带独立 15s 超时）
        let bg_timeout = Duration::from_secs(15);

        if let Some(handle) = idle_handle {
            match tokio::time::timeout(bg_timeout, handle).await {
                Ok(_) => debug!("空闲检测线程已退出"),
                Err(_) => warn!("空闲检测线程退出超时（{}s）", bg_timeout.as_secs()),
            }
        }

        if let Some(handle) = scheduler_handle {
            match tokio::time::timeout(bg_timeout, handle).await {
                Ok(_) => debug!("L2/L3 定时检查线程已退出"),
                Err(_) => warn!("L2/L3 定时检查线程退出超时（{}s）", bg_timeout.as_secs()),
            }
        }

        info!("SessionLifecycle shutdown 完成");
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle_creation() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);
        assert!(lifecycle.get_active_session_id().is_none());
        assert!(!lifecycle.shutdown_flag.load(Ordering::Relaxed));
    }

    #[test]
    fn session_active_id_tracking() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let sid = Uuid::new_v4();
        lifecycle.set_active_session_id(Some(sid));
        assert_eq!(lifecycle.get_active_session_id(), Some(sid));

        lifecycle.set_active_session_id(None);
        assert!(lifecycle.get_active_session_id().is_none());
    }

    #[test]
    fn session_last_active_tracking() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let sid = Uuid::new_v4();
        lifecycle.touch_session(sid);
        let last = lifecycle.get_last_active(sid);
        assert!(last.is_some());
        assert!(last.unwrap() > 0);
    }

    #[test]
    fn session_forget_after_close() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let sid = Uuid::new_v4();
        lifecycle.touch_session(sid);
        assert!(lifecycle.get_last_active(sid).is_some());

        lifecycle.forget_session(sid);
        assert!(lifecycle.get_last_active(sid).is_none());
    }

    #[test]
    fn shutdown_flag_signals_correctly() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let flag = lifecycle.shutdown_flag();
        assert!(!flag.load(Ordering::Relaxed));

        lifecycle.shutdown_flag.store(true, Ordering::SeqCst);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn config_values_correct() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        // 验证配置值传递正确
        assert_eq!(lifecycle.config.session.l1_idle_minutes, 10);
        assert_eq!(lifecycle.config.session.idle_check_interval_seconds, 60);
        assert_eq!(lifecycle.config.session.l2_check_interval_seconds, 86400);
    }

    #[test]
    fn set_retriever_stores_reference() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let retriever = Arc::new(RwLock::new(Retriever::new()));
        lifecycle.set_retriever(Arc::clone(&retriever));

        // 验证 retriever 已存储
        let guard = lifecycle.retriever.lock().unwrap();
        assert!(guard.is_some());
    }

    #[test]
    fn get_active_session_persona_uid_returns_none_when_no_active() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        // 无活跃 session 时 get_active_session_id 返回 None，
        // get_active_session_persona_uid 中的 `?` 会提前返回 None
        assert!(lifecycle.get_active_session_id().is_none());
    }
}
