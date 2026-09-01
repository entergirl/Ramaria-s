//! crates/ramaria-app/src/session_lifecycle/mod.rs - Session 生命周期与记忆管线触发
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

pub mod example_extract;
pub mod idle;
pub mod l1_generate;
pub mod l2_l3_scheduler;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, StorageBackend};
use ramaria_core::types::now_ms;
use ramaria_memory::retriever::Retriever;
use ramaria_memory::utt::builder::UttBuilder;
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
    /// 空闲自动保存阈值（分钟）——热更新
    ///
    /// 说明:
    /// - 独立于 `config.session.l1_idle_minutes` 的可变副本：设置页修改配置后
    ///   通过 `set_idle_minutes` 即时生效，空闲检测线程每轮 tick 读取最新值，
    ///   无需重启（与既有空闲检测线程联动）。
    /// - 初始值来自 `config.session.l1_idle_minutes`（`new` 时快照）。
    pub(crate) idle_minutes: Arc<AtomicU32>,
    /// 内存检索器引用（L1 生成后增量更新），None 表示未注入（向后兼容）
    pub(crate) retriever: Mutex<Option<Arc<RwLock<Retriever>>>>,
    /// embedding provider 引用（utt 块向量生成），None 表示未配置（块无向量）
    pub(crate) embedding: Mutex<Option<Arc<dyn EmbeddingProvider>>>,
    /// 行为层封存钩子（v1.5 M5 D6）：会话封存时触发行为规则增量更新。
    ///
    /// 职责:
    /// - 与 L1 生成同钩子（Step 2.7），手动/空闲两条封存路径均覆盖。
    /// - 钩子内部自行处理失败（记 warn 不阻塞封存主流程，注册式接入）。
    /// - None = 未注册（行为层关闭或旧构造路径），封存不触发增量更新（等同 v1.4）。
    pub(crate) behavior_hook: Mutex<Option<BehaviorCloseHook>>,
    /// 风格统计封存钩子（v1.7 M2 A3）：会话封存时触发风格统计增量更新。
    ///
    /// 职责:
    /// - 与行为层同钩子位置（Step 2.7 后），手动/空闲两条封存路径均覆盖。
    /// - 钩子内部自行处理失败（记 warn 不阻塞封存主流程，注册式接入）。
    /// - None = 未注册（风格关闭或旧构造路径），封存不触发风格统计（等同 v1.6）。
    pub(crate) style_hook: Mutex<Option<StyleCloseHook>>,
}

/// 行为层封存钩子类型：接收 persona_uid 的异步闭包（内部自行处理失败）。
pub(crate) type BehaviorCloseHook = Arc<
    dyn Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

/// 风格统计封存钩子类型：接收 persona_uid 的异步闭包（内部自行处理失败）。
pub(crate) type StyleCloseHook = Arc<
    dyn Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

impl SessionLifecycle {
    /// 创建新的 Session 生命周期编排器。
    ///
    /// retriever 默认为 `None`，调用方需在创建后通过 [`set_retriever`] 注入，
    /// 以启用 L1 摘要生成后的增量索引更新。
    pub fn new(config: RamariaConfig) -> Self {
        let idle_minutes = config.session.l1_idle_minutes;
        Self {
            active_session_id: Mutex::new(None),
            session_last_active: Mutex::new(HashMap::new()),
            config,
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            idle_minutes: Arc::new(AtomicU32::new(idle_minutes)),
            retriever: Mutex::new(None),
            embedding: Mutex::new(None),
            behavior_hook: Mutex::new(None),
            style_hook: Mutex::new(None),
        }
    }

    /// 注册行为层封存钩子（v1.5 M5 D6）。
    ///
    /// 调用时机:
    /// - 在 `App::new` 中完成 App 依赖组装后立即调用。
    /// - 钩子接收 persona_uid，内部执行行为规则增量更新；
    ///   任何失败由钩子自行记 warn（不阻塞封存主流程）。
    pub fn set_behavior_hook(&self, hook: BehaviorCloseHook) {
        let mut guard = self.behavior_hook.lock().unwrap_or_else(|e| {
            error!("behavior_hook lock poisoned during set_behavior_hook: {e}");
            e.into_inner()
        });
        *guard = Some(hook);
        info!("SessionLifecycle: 行为层封存钩子已注册，封存时触发增量更新");
    }

    /// 注册风格统计封存钩子（v1.7 M2 A3）。
    ///
    /// 调用时机:
    /// - 在 `App::new` 中完成 App 依赖组装后立即调用。
    /// - 钩子接收 persona_uid，内部执行风格统计增量更新；
    ///   任何失败由钩子自行记 warn（不阻塞封存主流程）。
    pub fn set_style_hook(&self, hook: StyleCloseHook) {
        let mut guard = self.style_hook.lock().unwrap_or_else(|e| {
            error!("style_hook lock poisoned during set_style_hook: {e}");
            e.into_inner()
        });
        *guard = Some(hook);
        info!("SessionLifecycle: 风格统计封存钩子已注册，封存时触发增量更新");
    }

    /// 注入内存检索器引用。
    ///
    /// 调用时机:
    /// - 在 `App::new` 中，`SessionLifecycle` 和 `Retriever` 创建完成后立即调用。
    /// - 必须在后台任务启动前调用（空闲检测/shutdown 路径依赖此引用做 L1 增量索引）。
    ///
    /// 参数:
    /// - `r`: 与 App 共享的 Retriever（`Arc<RwLock<Retriever>>`）。
    pub fn set_retriever(&self, r: Arc<RwLock<Retriever>>) {
        let mut guard = self.retriever.lock().unwrap_or_else(|e| {
            error!("retriever lock poisoned during set_retriever: {e}");
            e.into_inner()
        });
        *guard = Some(r);
        info!("SessionLifecycle: Retriever 引用已注入，L1 增量索引已启用");
    }

    /// 注入 embedding provider 引用（utt 块向量生成）。
    ///
    /// 调用时机:
    /// - 在 `App::new` 中与 [`set_retriever`] 同时调用。
    /// - 未注入时 utt 块照常构建，仅无向量（检索走子串降级）。
    pub fn set_embedding(&self, embedding: Option<Arc<dyn EmbeddingProvider>>) {
        let mut guard = self.embedding.lock().unwrap_or_else(|e| {
            error!("embedding lock poisoned during set_embedding: {e}");
            e.into_inner()
        });
        *guard = embedding;
        info!("SessionLifecycle: embedding 引用已注入，utt 块向量生成已启用");
    }

    /// 返回 shutdown_flag 的 Arc 引用，供外部线程检查。
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown_flag)
    }

    /// 热更新空闲自动保存阈值（分钟）。
    ///
    /// 说明:
    /// - 设置页保存 `session.l1_idle_minutes` 后由配置命令调用，
    ///   空闲检测线程下一轮 tick 即使用新阈值（无需重启）。
    /// - 调用方负责已落盘（config 双写）；本方法只做运行时联动。
    ///
    /// 参数:
    /// - `minutes`: 新阈值（分钟）。任意 u32 均接受（配置层已做范围校验）。
    pub fn set_idle_minutes(&self, minutes: u32) {
        let old = self.idle_minutes.swap(minutes, Ordering::Relaxed);
        info!(
            old_minutes = old,
            new_minutes = minutes,
            "空闲自动保存阈值已热更新"
        );
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
    pub(super) fn last_active(&self, session_id: Uuid) -> Option<i64> {
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

    /// 从 DB 读取当前活跃 session 的 `persona_uid`。
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
    /// - `persona_uid`: 当前对话人格的 UID（仅兜底，真相源为 DB session）。
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

        // 核心流程（关闭 + L1 + utt + examples + L2 检查）
        self.close_session_pipeline(storage, llm, active_sid, persona_uid)
            .await?;

        // Step 4: 清除活跃 session
        self.set_active_session_id(None);
        self.forget_session(active_sid);

        info!(%active_sid, "session 已关闭");
        Ok(())
    }

    /// 关闭指定 session 的完整管线（供空闲检测复用）。
    ///
    /// 与 [`save_and_close_session`] 的区别：本函数**不操作 active 指针**，
    /// 只对传入的 session_id 执行关闭 + L1 + utt + examples + L2 检查。
    /// 这样空闲检测线程可遍历关闭**所有**活跃会话（含切换人格后遗留的
    /// 孤儿会话），而不仅限于 active 指针指向的当前会话。
    ///
    /// 调用方职责:
    /// - 若关闭的 session 恰好是 active 指针指向的会话，调用方需自行
    ///   清理指针（`set_active_session_id(None)` + `forget_session`）。
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider（供 L1 summarizer 使用）。
    /// - `session_id`: 目标 session（必须未关闭，否则 close_session_safe 幂等跳过）。
    /// - `persona_uid`: 归属兜底参数（真相源为 DB `sessions.persona_uid`）。
    pub(super) async fn close_session_pipeline(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        session_id: Uuid,
        persona_uid: Option<&str>,
    ) -> RamariaResult<()> {
        // 归属统一以 DB `sessions.persona_uid` 为真相源。
        // 手动保存（前端传内存态）与空闲保存（DB 读）来源不一致导致
        // L1/utt/examples 归属不稳定；此处统一从 session 读取，
        // 调用方传入的 persona_uid 仅作兜底（session 查询失败时使用）。
        let persona_uid: Option<String> = match storage.get_session(session_id).await {
            Ok(Some(s)) => s.persona_uid.or_else(|| persona_uid.map(|p| p.to_string())),
            Ok(None) => {
                warn!(%session_id, "保存时 session 不存在，persona_uid 回退调用方参数");
                persona_uid.map(|p| p.to_string())
            }
            Err(e) => {
                warn!(%session_id, %e, "保存时读取 session 失败，persona_uid 回退调用方参数");
                persona_uid.map(|p| p.to_string())
            }
        };

        // Step 1: 关闭 session（设置 ended_at）
        // 对齐 Python `close_session(sid)`
        l1_generate::close_session_safe(storage, session_id).await?;

        // Step 2: 生成 L1 摘要（传入当前对话人格）
        // 对齐 Python `summarizer.summarize_session(session_id)`
        // 正常对话流程使用默认前缀（"用户：""助手："）
        match self
            .generate_l1_summary(storage, llm, session_id, persona_uid.as_deref(), None, None)
            .await
        {
            Ok(l1) => {
                info!(
                    %session_id,
                    l1_id = %l1.id,
                    summary_len = l1.summary.chars().count(),
                    "L1 摘要生成成功"
                );

                // 增量更新 Retriever 内存索引
                // 必须在 L2 级联检查前执行，确保后续 L2/L3 也能检索到新 L1
                self.index_l1_into_retriever(&l1).await;

                // Step 2.5: utt 话语块增量构建（v1.4，与 L1 同钩子）
                // 失败降级记 warn 不阻塞封存（下次封存自动补齐）
                self.build_utt_for_session(storage, session_id).await;

                // Step 2.6: examples 回复对抽取入库（v1.4，与 L1 同钩子）
                // 失败降级记 warn 不阻塞封存（下次封存自动补齐）
                self.extract_examples_for_session(storage, session_id).await;

                // Step 2.7: 行为规则增量更新（v1.5 M5 D6，与 L1 同钩子）
                // 注册式接入：钩子内部失败记 warn 不阻塞封存（等同 v1.4 行为）
                // 仅在 L1 生成成功路径触发（有真实对话内容才值得更新行为模型）
                // 注意：hook guard 须在 await 前释放（避免 std MutexGuard 跨 await）
                let hook = self
                    .behavior_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(persona) = persona_uid.as_deref()
                    && let Some(hook) = hook
                {
                    hook(persona).await;
                }

                // Step 2.8: 风格统计增量更新（v1.7 M2 A3，与 L1 同钩子）
                // 注册式接入：钩子内部失败记 warn 不阻塞封存（等同 v1.6 行为）
                // 仅在 L1 生成成功路径触发（有真实对话内容才值得统计风格）
                let style_hook = self
                    .style_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(persona) = persona_uid.as_deref()
                    && let Some(style_hook) = style_hook
                {
                    style_hook(persona).await;
                }

                // Step 3: 检查 L2 触发条件（路径 A：即时触发）
                // 对齐 Python summarizer 末尾的 `merger.check_and_merge`
                self.check_l2_trigger(storage, llm).await;
            }
            Err(e) => {
                tracing::error!(
                    %session_id,
                    error = %e,
                    "❌ L1 摘要生成失败！session 已关闭但未生成摘要。LLM 服务可能不可用。"
                );
                // 创建 pending BackgroundJob，供后续 regenerate_l1 重试
                // 对齐决策：L1 失败不阻塞 session 关闭，但需记录可重试任务
                let payload = serde_json::json!({
                    "session_id": session_id.to_string(),
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
                            %session_id,
                            job_id,
                            "已创建 pending L1 重试任务，稍后可调用 regenerate_l1 或后台自动重试"
                        );
                    }
                    Err(job_err) => {
                        tracing::error!(
                            %session_id,
                            %job_err,
                            "❌ 创建 L1 重试任务也失败了！需手动使用 generate_l1 命令重试。"
                        );
                    }
                }
            }
        }

        info!(%session_id, "session 已关闭（管线完成）");
        Ok(())
    }

    /// utt 话语块增量构建（封存钩子，v1.4）。
    ///
    /// 职责:
    /// - 会话封存后立即把本会话消息切分为话语块并入库（含向量生成）。
    /// - 幂等：重复执行只重切最后一个已入库块及其后的新增消息。
    ///
    /// 降级（不阻塞封存）:
    /// - `utt.enabled=false` → 跳过（行为回退 v1.3）。
    /// - 会话读取/构建失败 → warn 日志，下次封存自动补齐。
    /// - embedding 不可用/失败 → 块照常入库（无向量，检索走子串降级）。
    ///
    /// 安全约束:
    /// - 日志只记录计数与 ID，不记录原文内容（原文是最高敏感层）。
    pub(super) async fn build_utt_for_session(
        &self,
        storage: &dyn StorageBackend,
        session_id: Uuid,
    ) {
        if !self.config.utt.enabled {
            debug!(%session_id, "utt 配置关闭，跳过话语块构建（等同 v1.3）");
            return;
        }

        let session = match storage.get_session(session_id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                warn!(%session_id, "封存会话不存在，跳过 utt 构建");
                return;
            }
            Err(e) => {
                warn!(%session_id, %e, "读取会话失败，跳过 utt 构建");
                return;
            }
        };

        // 先 clone Arc 出锁再 await，避免 MutexGuard 跨 .await
        let embedder = self
            .embedding
            .lock()
            .unwrap_or_else(|e| {
                warn!("embedding lock poisoned during utt build: {e}");
                e.into_inner()
            })
            .clone();

        let builder = UttBuilder::from_config(&self.config.utt);
        match builder
            .build_session(storage, &session, embedder.as_deref())
            .await
        {
            Ok(stats) => {
                info!(
                    %session_id,
                    created = stats.chunks_created,
                    skipped = stats.chunks_skipped,
                    removed = stats.chunks_removed,
                    embedding_ok = stats.embedding_ok,
                    embedding_failed = stats.embedding_failed,
                    "utt 话语块增量构建完成"
                );
            }
            Err(e) => {
                warn!(
                    %session_id,
                    %e,
                    "utt 话语块构建失败（不阻塞封存，下次封存自动补齐）"
                );
            }
        }
    }

    /// examples 回复对抽取入库（封存钩子，v1.4）。
    ///
    /// 职责:
    /// - 会话封存后抽取"对方消息 → persona 回复"相邻对（决策见 docs/dev-1.4/v1.4-decisions.md）入库为候选池。
    /// - 入库前按 (partner, reply) 查重：重复回复对不重复入库（幂等）。
    ///
    /// 降级（不阻塞封存）:
    /// - `examples.enabled=false` → 跳过（行为回退 v1.3）。
    /// - 会话读取/抽取/入库失败 → warn 日志，下次封存自动补齐。
    /// - 抽取结果为空（无有效回复对）→ 正常返回，记 debug。
    ///
    /// 安全约束:
    /// - 日志只记录计数，不记录对话内容。
    pub(super) async fn extract_examples_for_session(
        &self,
        storage: &dyn StorageBackend,
        session_id: Uuid,
    ) {
        if !self.config.examples.enabled {
            debug!(%session_id, "examples 配置关闭，跳过回复对抽取（等同 v1.3）");
            return;
        }

        let session = match storage.get_session(session_id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                warn!(%session_id, "封存会话不存在，跳过 examples 抽取");
                return;
            }
            Err(e) => {
                warn!(%session_id, %e, "读取会话失败，跳过 examples 抽取");
                return;
            }
        };
        let Some(persona_uid) = session.persona_uid.as_deref() else {
            // 存量 NULL 会话防御——从消息首条 assistant 发言推断
            // 目标 persona；仍无法推断（纯用户会话）才跳过。
            let messages = match storage.list_messages(session_id).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(%session_id, %e, "读取会话消息失败，跳过 examples 抽取");
                    return;
                }
            };
            let Some(inferred) = ramaria_memory::utt::infer_target_persona_from_messages(&messages)
            else {
                debug!(%session_id, "会话无绑定 persona 且无法从消息推断，跳过 examples 抽取");
                return;
            };
            warn!(
                %session_id,
                persona_uid = %inferred,
                "会话 persona_uid 为 NULL，已从消息推断目标 persona（存量兼容）"
            );

            let pairs = example_extract::extract_pairs(&messages, &inferred);
            if pairs.is_empty() {
                debug!(%session_id, "本会话无有效回复对，跳过入库");
                return;
            }
            save_example_pairs(storage, session_id, &inferred, pairs).await;
            return;
        };

        let messages = match storage.list_messages(session_id).await {
            Ok(m) => m,
            Err(e) => {
                warn!(%session_id, %e, "读取会话消息失败，跳过 examples 抽取");
                return;
            }
        };

        let pairs = example_extract::extract_pairs(&messages, persona_uid);
        if pairs.is_empty() {
            debug!(%session_id, "本会话无有效回复对，跳过入库");
            return;
        }
        save_example_pairs(storage, session_id, persona_uid, pairs).await;
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
        // 从活跃 session 读取 persona_uid（不再传 None）
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
// examples 回复对入库（extract_examples_for_session 共用）
// =========================================================

/// 把抽取的回复对查重后入库（幂等），并记录统计日志。
///
/// 职责:
/// - 按 (persona_uid, partner, reply) 查重，重复回复对不重复入库。
/// - 新入库示例进入候选池（selected=false），注入侧按评分轮换选择。
/// - 单条失败仅 warn 不中断其余入库（不阻塞封存）。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `session_id`: 来源会话。
/// - `persona_uid`: 归属人格（已解析，可能来自消息推断）。
/// - `pairs`: 抽取的回复对（非空，由调用方保证）。
async fn save_example_pairs(
    storage: &dyn StorageBackend,
    session_id: Uuid,
    persona_uid: &str,
    pairs: Vec<example_extract::ExtractedPair>,
) {
    let mut saved = 0usize;
    let mut skipped = 0usize;
    for pair in pairs {
        // 幂等查重：已存在相同回复对 → 跳过
        match storage
            .find_example_by_pair(persona_uid, &pair.partner, &pair.reply)
            .await
        {
            Ok(Some(_)) => {
                skipped += 1;
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                warn!(%session_id, %e, "examples 查重失败，跳过该回复对");
                continue;
            }
        }

        let mut example = ramaria_core::types::PersonaExample::new(
            persona_uid.to_string(),
            pair.partner,
            pair.reply,
        );
        example.session_id = Some(session_id);
        example.context = pair.context;
        example.tags = if pair.tags.is_empty() {
            None
        } else {
            Some(pair.tags)
        };

        match storage.save_example(&example).await {
            Ok(id) => {
                saved += 1;
                info!(example_id = id, %session_id, persona_uid, "example 已入库");
            }
            Err(e) => {
                warn!(%session_id, %e, "example 入库失败（不阻塞封存）");
            }
        }
    }

    info!(
        %session_id,
        persona_uid,
        saved,
        skipped,
        "examples 回复对抽取入库完成"
    );
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
        let last = lifecycle.last_active(sid);
        assert!(last.is_some());
        assert!(last.unwrap() > 0);
    }

    #[test]
    fn session_forget_after_close() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let sid = Uuid::new_v4();
        lifecycle.touch_session(sid);
        assert!(lifecycle.last_active(sid).is_some());

        lifecycle.forget_session(sid);
        assert!(lifecycle.last_active(sid).is_none());
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

    /// v1.4 M5：set_idle_minutes 热更新——初始值来自 config，
    /// 热更新后新值即时生效（空闲检测线程每轮 tick 读取最新值，无需重启）。
    #[test]
    fn set_idle_minutes_hot_updates_threshold() {
        // 默认配置阈值 10 分钟
        let config = RamariaConfig::default();
        assert_eq!(config.session.l1_idle_minutes, 10);
        let lifecycle = SessionLifecycle::new(config);
        assert_eq!(lifecycle.idle_minutes.load(Ordering::Relaxed), 10);

        // 热更新到 25 分钟 → 下一轮 tick 使用新阈值
        lifecycle.set_idle_minutes(25);
        assert_eq!(lifecycle.idle_minutes.load(Ordering::Relaxed), 25);

        // 再次热更新（滑动块/自定义输入连续保存）
        lifecycle.set_idle_minutes(5);
        assert_eq!(lifecycle.idle_minutes.load(Ordering::Relaxed), 5);
    }

    /// v1.4 M5：App::set_idle_minutes 转发到 lifecycle（桌面端命令入口）。
    #[tokio::test]
    async fn app_set_idle_minutes_forwards_to_lifecycle() {
        use ramaria_core::traits::StorageBackend;
        use ramaria_core::types::AppState;
        use ramaria_llm::keychain::Keychain;
        use std::sync::Arc;

        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let llm = Arc::new(crate::stages::test_utils::MockLlm::local());
        let config = RamariaConfig::default();
        let keychain = Arc::new(Keychain::new());
        let app = crate::App::new_without_embedding(
            storage.clone() as Arc<dyn StorageBackend>,
            llm.clone() as Arc<dyn ramaria_core::traits::LlmProvider>,
            config,
            keychain,
        );
        app.set_state(AppState::Ready);

        app.set_idle_minutes(30);
        assert_eq!(app.lifecycle.idle_minutes.load(Ordering::Relaxed), 30);
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

    // =========================================================
    // examples 回复对抽取入库（v1.4）
    // =========================================================

    fn make_pair_messages(session_id: Uuid, target: &str) -> Vec<ramaria_core::types::Message> {
        use ramaria_core::types::{Message, MessageRole, MessageSource};
        let mut msgs = Vec::new();
        for i in 0..2 {
            msgs.push(Message::new(
                session_id,
                MessageRole::User,
                format!("用户问题第{i}条内容"),
                MessageSource::Local,
            ));
            msgs.push(
                Message::new(
                    session_id,
                    MessageRole::Assistant,
                    format!("角色回复第{i}条内容"),
                    MessageSource::Local,
                )
                .with_persona_uid(Some(target.to_string())),
            );
        }
        msgs
    }

    #[tokio::test]
    async fn extract_examples_populates_pool() {
        // 封存后：候选池增长（抽取 2 对入库）
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(Some("char-0001")).await.unwrap();
        storage.add_messages(session.id, make_pair_messages(session.id, "char-0001"));
        let lifecycle = SessionLifecycle::new(RamariaConfig::default());

        lifecycle
            .extract_examples_for_session(storage.as_ref(), session.id)
            .await;

        let pool = storage.list_all_examples("char-0001").await.unwrap();
        assert_eq!(pool.len(), 2, "两对回复全部入库");
        assert!(
            pool.iter()
                .all(|e| e.persona_uid == "char-0001" && e.session_id == Some(session.id)),
            "归属与来源正确"
        );
    }

    #[tokio::test]
    async fn extract_examples_idempotent() {
        // 重复执行：相同回复对不重复入库（幂等）
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(Some("char-0001")).await.unwrap();
        storage.add_messages(session.id, make_pair_messages(session.id, "char-0001"));
        let lifecycle = SessionLifecycle::new(RamariaConfig::default());

        lifecycle
            .extract_examples_for_session(storage.as_ref(), session.id)
            .await;
        lifecycle
            .extract_examples_for_session(storage.as_ref(), session.id)
            .await;

        let pool = storage.list_all_examples("char-0001").await.unwrap();
        assert_eq!(pool.len(), 2, "重复执行不产生重复入库");
    }

    #[tokio::test]
    async fn extract_examples_disabled_skips() {
        // 开关关闭 → 行为回退 v1.3（不抽取）
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(Some("char-0001")).await.unwrap();
        storage.add_messages(session.id, make_pair_messages(session.id, "char-0001"));
        let mut config = RamariaConfig::default();
        config.examples.enabled = false;
        let lifecycle = SessionLifecycle::new(config);

        lifecycle
            .extract_examples_for_session(storage.as_ref(), session.id)
            .await;

        assert!(
            storage
                .list_all_examples("char-0001")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn extract_examples_no_persona_skips() {
        // 会话未绑定 persona 且无法从消息推断（纯用户会话）→ 不抽取
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(None).await.unwrap();
        let user_only = vec![ramaria_core::types::Message::new(
            session.id,
            ramaria_core::types::MessageRole::User,
            "只有用户发言，没有 persona 回复".to_string(),
            ramaria_core::types::MessageSource::Local,
        )];
        storage.add_messages(session.id, user_only);
        let lifecycle = SessionLifecycle::new(RamariaConfig::default());

        lifecycle
            .extract_examples_for_session(storage.as_ref(), session.id)
            .await;

        assert!(
            storage
                .list_all_examples("char-0001")
                .await
                .unwrap()
                .is_empty()
        );
    }

    // NULL 会话（存量缺陷）从消息首条 assistant 发言推断
    // 目标 persona 后正常抽取入库，不再整会话跳过
    #[tokio::test]
    async fn extract_examples_null_persona_infers_target() {
        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let session = storage.create_session(None).await.unwrap();
        storage.add_messages(session.id, make_pair_messages(session.id, "char-0001"));
        let lifecycle = SessionLifecycle::new(RamariaConfig::default());

        lifecycle
            .extract_examples_for_session(storage.as_ref(), session.id)
            .await;

        let pool = storage.list_all_examples("char-0001").await.unwrap();
        assert_eq!(pool.len(), 2, "NULL 会话经推断后应抽取两对回复");
        assert!(
            pool.iter().all(|e| e.persona_uid == "char-0001"),
            "推断归属 char-0001"
        );
    }

    // =========================================================
    // close_session_pipeline 可关闭非 active 的孤儿会话
    // =========================================================

    /// 空闲检测遍历**全部**活跃会话（含切换人格遗留的孤儿会话）时，
    /// 复用 `close_session_pipeline` 关闭指定会话：
    /// - 孤儿会话正确关闭（ended_at 已置）；
    /// - active 指针指向的会话不受影响（指针与 ended_at 均不变）。
    ///
    /// 注：L1 归属（DB 真相源）已由 tests/session_lifecycle_tests.rs
    /// 的集成测试覆盖（test_utils 的 MockStorage 不落 L1）。
    #[tokio::test]
    async fn close_pipeline_closes_orphan_without_touching_active() {
        use ramaria_core::types::{Message, MessageRole, MessageSource};

        let storage = Arc::new(crate::stages::test_utils::MockStorage::new());
        let llm = Arc::new(crate::stages::test_utils::MockLlm::local());
        let lifecycle = SessionLifecycle::new(RamariaConfig::default());

        // 两个活跃会话：active（当前对话指针）+ orphan（切换人格后遗留）
        let active = storage.create_session(Some("char-0001")).await.unwrap();
        let orphan = storage.create_session(Some("char-0002")).await.unwrap();
        lifecycle.set_active_session_id(Some(active.id));

        // 孤儿会话注入消息（L1 摘要需要输入）
        storage.add_messages(
            orphan.id,
            vec![Message::new(
                orphan.id,
                MessageRole::User,
                "你好".to_string(),
                MessageSource::Local,
            )],
        );

        // 核心：直接关闭孤儿会话（不经 active 指针）
        lifecycle
            .close_session_pipeline(storage.as_ref(), llm.as_ref(), orphan.id, None)
            .await
            .expect("关闭孤儿会话不应失败");

        // 孤儿会话已关闭（ended_at 已置）
        let closed = storage
            .get_session(orphan.id)
            .await
            .unwrap()
            .expect("孤儿会话应存在");
        assert!(closed.ended_at.is_some(), "孤儿会话应被关闭");

        // active 指针不受影响；active 会话保持活跃
        assert_eq!(
            lifecycle.get_active_session_id(),
            Some(active.id),
            "active 指针不应被孤儿会话关闭影响"
        );
        let active_now = storage
            .get_session(active.id)
            .await
            .unwrap()
            .expect("active 会话应存在");
        assert!(active_now.ended_at.is_none(), "active 会话应保持活跃");
    }
}
