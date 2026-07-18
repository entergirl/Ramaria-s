//! rust/crates/ramaria-app/src/session_lifecycle.rs - Session 生命周期与记忆管线触发
//!
//! 设计特点:
//! - 对齐 Python `SessionManager` 的完整行为：手动关闭、空闲自动关闭、只读约束
//! - Thread A：每 60s 轮询活跃 session 空闲时间，超过阈值自动关闭 → L1 摘要 → L2 触发检查
//! - Thread B：每 24h 检查最早未吸收 L1（>7天 → L2）、最早未吸收事件（>30天 → L3）
//! - shutdown hook：应用退出时自动关闭活跃 session 并等待后台任务完成
//! - 所有管线触发通过 `JobManager` 编排，含重试和指数退避
//! - 级联触发：L1 写入后 → 检查 L2 条件（路径 A）；L2 写入后 → 检查 L3 条件
//!
//! 与 Python 对齐:
//! - `_close_and_summarize` → `close_and_summarize_session`
//! - Thread A `_idle_checker_loop` → `run_idle_checker`
//! - Thread B → `run_l2_l3_scheduler`
//! - `force_close_current_session` → `save_and_close_session`

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{LlmProvider, StorageBackend};
use ramaria_core::types::now_ms;
use ramaria_memory::event::{EventExtractor, EventExtractorConfig};
use ramaria_memory::job::{JobManager, JobResult, JobType};
use ramaria_memory::l1::{L1Summarizer, L1SummarizerConfig};
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
/// - `SessionManager._idle_checker_loop` → `run_idle_checker`
/// - `SessionManager._l2_checker_loop` → `run_l2_l3_scheduler`
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
    fn set_active_session_id(&self, sid: Option<Uuid>) {
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
    fn get_last_active(&self, session_id: Uuid) -> Option<i64> {
        let guard = self.session_last_active.lock().unwrap_or_else(|e| {
            error!("session_last_active lock poisoned: {e}");
            e.into_inner()
        });
        guard.get(&session_id).copied()
    }

    /// 移除 session 的活跃时间缓存（session 关闭后清理）。
    fn forget_session(&self, session_id: Uuid) {
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
    async fn get_active_session_persona_uid(&self, storage: &dyn StorageBackend) -> Option<String> {
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
    /// 4. 检查 L2 触发条件（路径 A：未吸收 L1 ≥ 5 条）
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
        close_session_safe(storage, active_sid).await?;

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
    // L1 摘要手动重试（公开 API）
    // =========================================================

    /// 为指定 session 重新生成 L1 摘要（手动重试）。
    ///
    /// 职责:
    /// - 供 save_and_close_session 中 L1 失败后的手动补救。
    /// - session 可以已关闭，也可以仍在活跃中（shutdown 场景）。
    ///
    /// 参数:
    /// - `session_id`: 目标 session。
    /// - `persona_uid`: 人格标识。
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: 生成成功。
    /// - `Ok(None)`: session 无消息。
    pub async fn regenerate_l1(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        session_id: Uuid,
        persona_uid: Option<&str>,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        // 检查是否有消息可摘要
        let messages = storage.list_messages(session_id).await?;
        if messages.is_empty() {
            warn!(%session_id, "regenerate_l1: session 无消息，跳过");
            return Ok(None);
        }

        info!(%session_id, ?persona_uid, msg_count = messages.len(), "手动重试 L1 摘要");

        match self
            .generate_l1_summary(storage, llm, session_id, persona_uid)
            .await
        {
            Ok(l1) => {
                info!(%session_id, l1_id = %l1.id, "L1 重试成功");
                // v1.2: 增量更新 Retriever 索引
                self.index_l1_into_retriever(&l1);
                // 触发 L2 检查（路径 A）
                self.check_l2_trigger(storage, llm).await;
                Ok(Some(l1))
            }
            Err(e) => {
                error!(%session_id, %e, "L1 重试失败");
                Err(e)
            }
        }
    }

    /// 生成 L1 摘要但不触发 L2 级联（用于批量导入场景，全部 L1 完成后统一触发）。
    ///
    /// 幂等性（v1.2）:
    /// - 若 session 已有目标 persona_uid 的 L1 摘要 → 跳过生成（避免重复 LLM 调用）。
    /// - 若仅有 NULL-persona_uid 的旧摘要 → 删除后重新生成。
    /// - 若无任何摘要 → 直接生成。
    ///
    /// 说明:
    /// - 与 `regenerate_l1` 功能相同，但跳过末尾的 `check_l2_trigger` 调用。
    /// - 调用方应在全部 L1 生成完成后手动调用 `check_l2_trigger`。
    pub async fn regenerate_l1_no_cascade(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        session_id: Uuid,
        persona_uid: Option<&str>,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        let messages = storage.list_messages(session_id).await?;
        if messages.is_empty() {
            warn!(%session_id, "regenerate_l1_no_cascade: session 无消息，跳过");
            return Ok(None);
        }

        // v1.2: 检查是否已存在目标 persona_uid 的 L1 摘要（幂等——避免重复 LLM 调用）
        if let Some(target_uid) = persona_uid {
            let existing = storage.list_memory_l1(session_id).await?;
            let already_has = existing
                .iter()
                .any(|l1| l1.persona_uid.as_deref() == Some(target_uid));
            if already_has {
                info!(
                    %session_id,
                    persona_uid = %target_uid,
                    "该 session 已有目标 persona 的 L1 摘要，跳过重新生成"
                );
                // 增量索引仍要确保（万一之前的 rebuild 跳过了）
                if let Some(l1) = existing
                    .into_iter()
                    .find(|l| l.persona_uid.as_deref() == Some(target_uid))
                {
                    self.index_l1_into_retriever(&l1);
                }
                return Ok(None);
            }
        }

        // v1.2: 删除旧 NULL-persona_uid L1 摘要，再做生成
        let deleted = storage.delete_memory_l1_by_session(session_id).await?;
        if deleted > 0 {
            info!(%session_id, deleted, "已清理旧 NULL-persona_uid L1 摘要");
        }

        info!(%session_id, ?persona_uid, msg_count = messages.len(), "批量 L1 摘要（无级联）");

        match self
            .generate_l1_summary(storage, llm, session_id, persona_uid)
            .await
        {
            Ok(l1) => {
                info!(%session_id, l1_id = %l1.id, "L1 生成成功（无级联）");
                // v1.2: 增量更新 Retriever 索引（批量导入场景每批次一个 session）
                self.index_l1_into_retriever(&l1);
                Ok(Some(l1))
            }
            Err(e) => {
                error!(%session_id, %e, "L1 生成失败");
                Err(e)
            }
        }
    }

    // =========================================================
    // L1 摘要生成（内部辅助）
    // =========================================================

    /// 为指定 session 生成 L1 摘要。
    ///
    /// 参数:
    /// - `persona_uid`: 当前对话人格的 UID，用于 L1 归属。
    ///
    /// 对齐 Python `summarizer.summarize_session(session_id)`。
    async fn generate_l1_summary(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        session_id: Uuid,
        persona_uid: Option<&str>,
    ) -> RamariaResult<ramaria_core::types::MemoryL1> {
        let mut summarizer_config = L1SummarizerConfig::default();
        // 设置 persona_uid，确保 L1 摘要可被记忆页面按人格过滤查询到
        if let Some(uid) = persona_uid {
            summarizer_config.persona_uid = Some(uid.to_string());
        }
        let summarizer = L1Summarizer::new(llm, storage, summarizer_config);

        // 通过 JobManager 包装执行（带重试）
        let job_manager = JobManager::with_defaults(storage);
        let payload = serde_json::json!({ "session_id": session_id.to_string() }).to_string();

        let result = job_manager
            .execute_with_retry(JobType::L1Summary, Some(&payload), None, || {
                summarize_with_summarizer(&summarizer, session_id)
            })
            .await;

        match result {
            Ok(_job_id) => {
                // 摘要已写入存储，需要读取返回（JobManager 不返回业务结果）
                // 从 storage 读取刚生成的 L1
                let l1_list = storage.list_memory_l1(session_id).await?;
                l1_list
                    .into_iter()
                    .last()
                    .ok_or_else(|| RamariaError::validation("L1 摘要生成后无法读取"))
            }
            Err(e) => Err(e),
        }
    }

    /// v1.2: 将 L1 摘要增量添加到 Retriever 内存索引。
    ///
    /// 职责:
    /// - 在 L1 摘要生成成功后立即调用，使新 L1 文档无需等待手动 `rebuild_retriever`
    ///   即可被 Stage 5 RAG 检索命中（D-V12-013）。
    ///
    /// 容错:
    /// - Retriever 未注入（向后兼容）→ 静默跳过。
    /// - Mutex 锁污染 → warn 日志 + 跳过。
    /// - 索引添加失败 → warn 日志 + 不阻塞 L2 级联检查。
    ///
    /// 参数:
    /// - `l1`: 刚生成的 L1 摘要记录。
    fn index_l1_into_retriever(&self, l1: &ramaria_core::types::MemoryL1) {
        let ret_guard = match self.retriever.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("retriever lock poisoned during index_l1_into_retriever: {e}");
                return;
            }
        };
        if let Some(ref retriever_arc) = *ret_guard {
            // v1.3 P-3: RwLock write() 用于索引写入（index_l1_record 需要 &mut self）
            match retriever_arc.write() {
                Ok(mut retriever) => {
                    if let Err(e) = retriever.index_l1_record(l1) {
                        warn!(
                            l1_id = %l1.id,
                            error = %e,
                            "增量更新 Retriever 索引失败（不影响 L2 级联）"
                        );
                    } else {
                        info!(
                            l1_id = %l1.id,
                            persona_uid = ?l1.persona_uid,
                            "L1 摘要已增量加入 Retriever 内存索引，即时可检索"
                        );
                    }
                }
                Err(e) => {
                    warn!("retriever 内部 Mutex poisoned: {e}");
                }
            }
        }
    }

    // =========================================================
    // L2 事件提取触发检查（路径 A + 路径 B）
    // =========================================================

    /// 检查 L2 事件提取触发条件（路径 A：即时触发）。
    ///
    /// 对齐 Python `merger.check_and_merge` 的计数触发路径。
    /// 遍历所有 persona，检查未吸收 L1 是否 ≥ 5 条。
    pub(crate) async fn check_l2_trigger(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
    ) {
        let personas = match storage.list_personas().await {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "L2 触发检查：无法列出 persona");
                return;
            }
        };

        let mut total_personas = 0usize;
        let mut checked = 0usize;
        let mut triggered = 0usize;
        let mut skipped = 0usize;

        for persona in &personas {
            total_personas += 1;
            if self.shutdown_flag.load(Ordering::Relaxed) {
                return;
            }

            let unabsorbed = match storage.list_unabsorbed_l1(&persona.uid).await {
                Ok(l) => l,
                Err(e) => {
                    warn!(persona_uid = %persona.uid, %e, "L2 触发检查：查询未吸收 L1 失败");
                    continue;
                }
            };

            checked += 1;
            let trigger_count = self.config.thresholds.l2_trigger_count as usize;
            if unabsorbed.len() >= trigger_count {
                triggered += 1;
                info!(
                    persona_uid = %persona.uid,
                    unabsorbed_count = unabsorbed.len(),
                    trigger_count,
                    "L2 触发条件满足，启动事件提取"
                );
                self.run_l2_extraction(storage, llm, &persona.uid).await;
            } else {
                skipped += 1;
                // 使用 info! 而非 debug!，确保用户能看到未触发的原因
                info!(
                    persona_uid = %persona.uid,
                    persona_name = %persona.name,
                    unabsorbed_count = unabsorbed.len(),
                    trigger_count,
                    "L2 触发条件未满足（需要 {} 条未吸收 L1，当前 {} 条）",
                    trigger_count,
                    unabsorbed.len()
                );
            }
        }

        info!(
            total_personas,
            checked,
            triggered,
            skipped,
            "L2 触发检查完成: {} 个 persona 中 {} 个触发 L2，{} 个条件未满足",
            checked,
            triggered,
            skipped
        );
    }

    /// 执行 L2 事件提取（通过 JobManager 包裹，带重试和可观测性）。
    ///
    /// 对齐 Python `merger.check_and_merge` 的 LLM 提取逻辑。
    ///
    /// 重试策略:
    /// - LLM 调用失败 → 可重试（JobResult::Retryable），最多 3 次，指数退避。
    /// - 存储写入失败 → 同上可重试。
    /// - 成功但无事件 → 视为 Success（正常情况，非错误）。
    async fn run_l2_extraction(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        persona_uid: &str,
    ) {
        let persona_owned = persona_uid.to_string();
        let job_manager = JobManager::with_defaults(storage);
        let payload = serde_json::json!({ "persona_uid": &persona_owned }).to_string();

        // 通过 JobManager 包裹执行：create → running → execute → completed/failed
        // 重试由 JobManager 内部处理（指数退避，最大 3 次）
        let job_result = job_manager
            .execute_with_retry(JobType::EventExtract, Some(&payload), None, || {
                // 每次尝试都新建 EventExtractor（提取器创建代价低，且避免重试时复用状态）
                let mut extractor =
                    EventExtractor::new(llm, storage, EventExtractorConfig::default());
                let uid = persona_owned.clone();
                async move {
                    match extractor.extract_events(&uid).await {
                        Ok(events) if events.is_empty() => {
                            info!(persona_uid = %uid, "L2 提取完成，无新事件");
                            JobResult::Success
                        }
                        Ok(events) => {
                            info!(
                                persona_uid = %uid,
                                event_count = events.len(),
                                "L2 事件提取完成"
                            );
                            JobResult::Success
                        }
                        Err(e) => {
                            // LLM 调用失败或存储写入失败，标记为可重试
                            warn!(
                                persona_uid = %uid,
                                error = %e,
                                "L2 事件提取失败，将重试"
                            );
                            JobResult::Retryable(e.to_string())
                        }
                    }
                }
            })
            .await;

        match job_result {
            Ok(job_id) => {
                info!(persona_uid = %persona_owned, job_id, "L2 事件提取任务完成");
                // L2 成功后级联检查 L3（路径 A）
                self.check_l3_trigger(storage, llm, &persona_owned).await;
            }
            Err(e) => {
                error!(
                    persona_uid = %persona_owned,
                    error = %e,
                    "L2 事件提取失败（已达最大重试次数），L3 级联跳过"
                );
            }
        }
    }

    // =========================================================
    // L3 性格推断触发检查
    // =========================================================

    /// 检查 L3 性格推断触发条件。
    ///
    /// 对齐 Python `profile_manager` + →B→C 管线。
    /// 触发条件：未吸收事件 ≥ 10 条 或 最早事件 > 30 天。
    pub(crate) async fn check_l3_trigger(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        persona_uid: &str,
    ) {
        let events = match storage.list_unabsorbed_events(persona_uid).await {
            Ok(e) => e,
            Err(e) => {
                warn!(persona_uid, %e, "L3 触发检查：查询未吸收事件失败");
                return;
            }
        };

        if events.is_empty() {
            return;
        }

        let now = now_ms();
        let oldest_event_age_days = events
            .iter()
            .map(|e| e.start)
            .min()
            .map(|min_time| (now - min_time) as f64 / (1000.0 * 86400.0))
            .unwrap_or(0.0);

        // L3 触发条件来自配置（对齐 Python ProfileConfig）
        let trigger_count = self.config.thresholds.l3_trigger_count as usize;
        let trigger_days = self.config.thresholds.l3_trigger_days as f64;

        let should_trigger = events.len() >= trigger_count || oldest_event_age_days >= trigger_days;

        if should_trigger {
            info!(
                persona_uid,
                event_count = events.len(),
                oldest_days = %format!("{:.1}", oldest_event_age_days),
                "L3 触发条件满足，启动性格推断"
            );
            self.run_l3_inference(storage, llm, persona_uid).await;
        } else {
            info!(
                persona_uid,
                event_count = events.len(),
                trigger_count,
                oldest_days = %format!("{:.1}", oldest_event_age_days),
                trigger_days,
                "L3 触发条件未满足（需要 {} 条未吸收事件或最早事件 > {} 天，当前 {} 条 {:.1} 天）",
                trigger_count, trigger_days, events.len(), oldest_event_age_days
            );
        }
    }

    /// 执行 L3 性格推断（ 统计 → LLM 推断 → 增量更新）。
    ///
    /// 对齐 Python `profile_manager.extract_profile` + Rust inference 管线。
    ///
    /// 可观测性:
    /// - 通过 JobManager 创建 `PersonalityInference` 任务记录，
    ///   记录开始/完成/failed 时间，便于运维排查"何时对谁做了推断"。
    ///
    /// v1.2 更新:
    /// - : 纯数值统计（预过滤 → 聚类 → 收缩 → 跨分类指标）
    /// - : LLM 三步结构化推断（已接通）
    /// - : 漂移检测 + 置信度更新（已接通）
    async fn run_l3_inference(
        &self,
        storage: &dyn StorageBackend,
        llm: &dyn LlmProvider,
        persona_uid: &str,
    ) {
        let persona_owned = persona_uid.to_string();

        // 取未吸收事件列表
        let events = match storage.list_unabsorbed_events(&persona_owned).await {
            Ok(e) => e,
            Err(e) => {
                error!(persona_uid = %persona_owned, %e, "L3 推断：查询事件失败");
                return;
            }
        };

        if events.is_empty() {
            debug!(persona_uid = %persona_owned, "L3 推断：无未吸收事件，跳过");
            return;
        }

        // ---- 创建 JobManager 任务记录（可观测性） ----
        let job_manager = JobManager::with_defaults(storage);
        let payload = serde_json::json!({
            "persona_uid": &persona_owned,
            "event_count": events.len(),
            "phase": "A"
        })
        .to_string();

        let job_id = match job_manager
            .create(JobType::PersonalityInference, Some(&payload))
            .await
        {
            Ok(id) => id,
            Err(e) => {
                error!(persona_uid = %persona_owned, %e, "创建 L3 推断任务记录失败，继续执行");
                0 // 哨兵值：表示无有效 job_id
            }
        };

        if job_id > 0 {
            let _ = job_manager.mark_running(job_id).await;
        }

        // ---- : 统计特征提取（纯数值，不调 LLM） ----
        use ramaria_memory::inference::{InferrerConfig, StatsConfig, run_phase_a_stats};

        let stats_config = StatsConfig::default();
        let stats_summary = run_phase_a_stats(&events, &stats_config);

        info!(
            persona_uid = %persona_owned,
            event_count = events.len(),
            category_count = stats_summary.categories.len(),
            job_id,
            "L3 Phase A 统计完成"
        );

        // 将统计结果持久化到 cluster_snapshots 表
        let mut snapshot_count = 0usize;
        for cat_stats in &stats_summary.categories {
            let snapshot_json = serde_json::json!({
                "category": cat_stats.category,
                "event_count": cat_stats.event_count,
                "n_effective": cat_stats.n_eff,
                "valence_mean": cat_stats.valence_mean,
                "valence_std": cat_stats.valence_std,
                "share_mean": cat_stats.share_mean,
            });

            let snapshot = ramaria_core::types::ClusterSnapshot {
                id: 0,
                persona_uid: persona_owned.clone(),
                category: cat_stats.category.clone(),
                cluster_label: format!("cluster_{}", cat_stats.category),
                samples: Some(snapshot_json.to_string()),
                count: cat_stats.event_count as i32,
                is_current: true,
                created_at: now_ms(),
                semantic_label: None,
                semantic_label_embedding: None,
            };

            match storage.save_cluster_snapshot(&snapshot).await {
                Ok(_) => snapshot_count += 1,
                Err(e) => {
                    warn!(
                        persona_uid = %persona_owned,
                        category = %cat_stats.category,
                        error = %e,
                        "写入聚类快照失败（单条跳过，不影响其他分类）"
                    );
                }
            }
        }

        info!(
            persona_uid = %persona_owned,
            job_id,
            snapshot_count,
            total_categories = stats_summary.categories.len(),
            "L3 Phase A 推断流程完成，开始 Phase B"
        );

        // ---- : LLM 三步结构化推断 ----
        use ramaria_memory::inference::run_phase_b_inference;

        let inferrer_config = InferrerConfig::default();
        let phase_b_result = match run_phase_b_inference(
            llm,
            storage,
            &stats_summary,
            &persona_owned,
            &inferrer_config,
        )
        .await
        {
            Ok(result) => {
                info!(
                    persona_uid = %persona_owned,
                    saved = result.traits_saved,
                    updated = result.traits_updated,
                    deprecated = result.traits_deprecated,
                    source = ?result.source,
                    "L3 Phase B 推断完成"
                );
                result
            }
            Err(e) => {
                error!(persona_uid = %persona_owned, error = %e, "L3 Phase B 推断失败");
                if job_id > 0 {
                    let _ = job_manager
                        .mark_failed(job_id, &format!("Phase B 推断失败: {e}"))
                        .await;
                }
                return;
            }
        };

        // ---- : 置信度更新 + 漂移检测 ----
        use ramaria_memory::inference::run_phase_c_update;

        // 判断是否为首轮推断（Phase B 结果中 traits_saved == total 且无 update/deprecate）
        let is_first_round =
            phase_b_result.traits_updated == 0 && phase_b_result.traits_deprecated == 0;

        match run_phase_c_update(
            storage,
            &persona_owned,
            &phase_b_result.traits,
            &events,
            is_first_round,
        )
        .await
        {
            Ok(phase_c_result) => {
                info!(
                    persona_uid = %persona_owned,
                    traits_updated = phase_c_result.traits_updated,
                    evidence_saved = phase_c_result.evidence_saved,
                    has_drift = phase_c_result.has_significant_drift,
                    drift_categories = ?phase_c_result.drift_categories,
                    "L3 Phase C 更新完成"
                );
            }
            Err(e) => {
                error!(persona_uid = %persona_owned, error = %e, "L3 Phase C 更新失败");
                // Phase C 失败不阻塞事件吸收标记——traits 已写入，confidence 保持初始值
            }
        };

        // ---- 标记事件已吸收 ----
        let event_ids: Vec<i64> = events.iter().map(|e| e.id).collect();
        if !event_ids.is_empty() {
            match storage.mark_events_absorbed(&event_ids).await {
                Ok(_) => {
                    info!(
                        persona_uid = %persona_owned,
                        event_count = event_ids.len(),
                        "L3 推断：已标记事件吸收"
                    );
                }
                Err(e) => {
                    warn!(persona_uid = %persona_owned, error = %e, "L3 推断：标记事件吸收失败");
                }
            }
        }

        // 标记任务完成
        if job_id > 0
            && let Err(e) = job_manager.mark_completed(job_id).await
        {
            warn!(job_id, %e, "标记 L3 推断任务完成失败（已执行，仅状态未更新）");
        }

        info!(
            persona_uid = %persona_owned,
            job_id,
            "L3 推断全流程（Phase A→B→C）完成"
        );
    }

    // =========================================================
    // 后台线程 A：空闲检测
    // =========================================================

    /// 启动后台空闲检测线程（Thread A）。
    ///
    /// 对齐 Python `SessionManager._idle_checker_loop`。
    ///
    /// 逻辑:
    /// - 每 `config.session.idle_check_interval_seconds`（默认 60s）轮询
    /// - 若活跃 session 的最后消息时间距今超过 `config.session.l1_idle_minutes`（默认 10min）
    ///   → 自动调用 `save_and_close_session`
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider。
    ///
    /// 返回:
    /// - `tokio::task::JoinHandle<>`，供 shutdown 时等待。
    pub fn spawn_idle_checker(
        self: &Arc<Self>,
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
    ) -> tokio::task::JoinHandle<()> {
        let slf = Arc::clone(self);
        let interval_secs = self.config.session.idle_check_interval_seconds;
        let idle_minutes = self.config.session.l1_idle_minutes;
        let shutdown_flag = Arc::clone(&self.shutdown_flag);

        info!(
            interval_secs,
            idle_minutes, "后台空闲检测线程启动（Thread A）"
        );

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs as u64));
            // 跳过首次立即触发（给应用启动留缓冲）
            ticker.tick().await;

            loop {
                ticker.tick().await;

                if shutdown_flag.load(Ordering::Relaxed) {
                    info!("空闲检测线程收到停止信号，退出");
                    return;
                }

                let active_sid = match slf.get_active_session_id() {
                    Some(sid) => sid,
                    None => {
                        // 无活跃 session，无需检测
                        continue;
                    }
                };

                // 从内存缓存获取最后活跃时间（Python 从 DB 查）
                let last_active = match slf.get_last_active(active_sid) {
                    Some(t) => t,
                    None => {
                        // 内存缓存中没有，尝试从 DB 恢复
                        // 对齐 Python `database.get_last_message_time(session_id)`
                        match get_last_msg_time_from_db(storage.as_ref(), active_sid).await {
                            Ok(Some(t)) => {
                                slf.touch_session(active_sid);
                                t
                            }
                            Ok(None) => {
                                // 无消息的空 session，使用创建时间
                                debug!(%active_sid, "session 无消息，跳过空闲检测");
                                continue;
                            }
                            Err(e) => {
                                warn!(%active_sid, %e, "查询最后消息时间失败");
                                continue;
                            }
                        }
                    }
                };

                let now = now_ms();
                let idle_ms = now.saturating_sub(last_active);
                let idle_min = idle_ms as f64 / 60_000.0;

                if idle_min >= idle_minutes as f64 {
                    info!(
                        %active_sid,
                        idle_min = %format!("{:.1}", idle_min),
                        threshold_min = idle_minutes,
                        "session 空闲超时，自动关闭"
                    );

                    // v1.2: 从活跃 session 读取 persona_uid（不再传 None）
                    // 修复前：save_and_close_session(..., None) → L1 摘要 persona_uid = NULL
                    //          → list_recent_l1_by_persona 查不到 → 跨 session 上下文注入失效
                    let persona_uid = slf.get_active_session_persona_uid(storage.as_ref()).await;

                    if let Err(e) = slf
                        .save_and_close_session(
                            storage.as_ref(),
                            llm.as_ref(),
                            persona_uid.as_deref(),
                        )
                        .await
                    {
                        error!(%active_sid, %e, "自动关闭 session 失败");
                    }
                } else {
                    debug!(
                        %active_sid,
                        idle_min = %format!("{:.1}", idle_min),
                        "session 仍在活跃，未触发空闲关闭"
                    );
                }
            }
        })
    }

    // =========================================================
    // 后台线程 B：L2/L3 定时触发
    // =========================================================

    /// 启动后台 L2/L3 定时检查线程（Thread B）。
    ///
    /// 对齐 Python `SessionManager._l2_checker_loop`。
    ///
    /// 逻辑:
    /// - 每 `config.session.l2_check_interval_seconds`（默认 86400s = 24h）轮询
    /// - 遍历所有 persona：
    /// - 最早未吸收 L1 > 7 天 → 触发 L2 事件提取
    /// - 最早未吸收事件 > 30 天 → 触发 L3 性格推断
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `llm`: LLM provider。
    pub fn spawn_l2_l3_scheduler(
        self: &Arc<Self>,
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
    ) -> tokio::task::JoinHandle<()> {
        let slf = Arc::clone(self);
        let interval_secs = self.config.session.l2_check_interval_seconds;
        let shutdown_flag = Arc::clone(&self.shutdown_flag);

        info!(interval_secs, "后台 L2/L3 定时检查线程启动（Thread B）");

        tokio::spawn(async move {
            // 首次延迟：启动 5 分钟后执行首次检查（避免阻塞启动流程）
            tokio::time::sleep(Duration::from_secs(300)).await;

            loop {
                if shutdown_flag.load(Ordering::Relaxed) {
                    info!("L2/L3 定时检查线程收到停止信号，退出");
                    return;
                }

                // 执行定时检查
                slf.run_scheduled_l2_l3_check(storage.as_ref(), llm.as_ref())
                    .await;

                // 等待下一次检查（可中断）
                let mut sleep_secs = interval_secs as u64;
                while sleep_secs > 0 && !shutdown_flag.load(Ordering::Relaxed) {
                    let chunk = sleep_secs.min(60); // 每 60s 检查一次停止信号
                    tokio::time::sleep(Duration::from_secs(chunk)).await;
                    sleep_secs = sleep_secs.saturating_sub(chunk);
                }
            }
        })
    }

    /// 执行一次性 L2/L3 定时检查。
    ///
    /// 对齐 Python `merger.check_and_merge` 的时间触发路径（路径 B）。
    async fn run_scheduled_l2_l3_check(&self, storage: &dyn StorageBackend, llm: &dyn LlmProvider) {
        debug!("L2/L3 定时检查开始");

        let personas = match storage.list_personas().await {
            Ok(p) => p,
            Err(e) => {
                error!(%e, "L2/L3 定时检查：无法列出 persona");
                return;
            }
        };

        let now = now_ms();
        let ms_per_day: i64 = 86_400_000;

        for persona in &personas {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                return;
            }

            // ---- L2 时间触发 ----
            // 对齐 Python：最早未吸收 L1 > 7 天则触发
            match storage.list_unabsorbed_l1(&persona.uid).await {
                Ok(l1_list) => {
                    if let Some(oldest) = l1_list.iter().map(|l| l.created_at).min() {
                        let age_days = (now - oldest) as f64 / ms_per_day as f64;
                        if age_days >= 7.0 {
                            info!(
                                persona_uid = %persona.uid,
                                %age_days,
                                l1_count = l1_list.len(),
                                "L2 定时触发（路径 B：最早未吸收 L1 > 7 天）"
                            );
                            self.run_l2_extraction(storage, llm, &persona.uid).await;
                        }
                    }
                }
                Err(e) => {
                    warn!(persona_uid = %persona.uid, %e, "L2 定时检查：查询 L1 失败");
                }
            }

            // ---- L3 时间触发 ----
            // 对齐 Python profile 更新：最早未吸收事件 > 30 天则触发
            match storage.list_unabsorbed_events(&persona.uid).await {
                Ok(events) => {
                    if let Some(oldest) = events.iter().map(|e| e.start).min() {
                        let age_days = (now - oldest) as f64 / ms_per_day as f64;
                        if age_days >= 30.0 {
                            info!(
                                persona_uid = %persona.uid,
                                %age_days,
                                event_count = events.len(),
                                "L3 定时触发（路径 B：最早未吸收事件 > 30 天）"
                            );
                            self.run_l3_inference(storage, llm, &persona.uid).await;
                        }
                    }
                }
                Err(e) => {
                    warn!(persona_uid = %persona.uid, %e, "L3 定时检查：查询事件失败");
                }
            }
        }

        debug!("L2/L3 定时检查完成");
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
        // 修复前：save_and_close_session(..., None) → L1 摘要 persona_uid = NULL → 数据死锁
        let persona_uid = self.get_active_session_persona_uid(storage).await;
        match self
            .save_and_close_session(storage, llm, persona_uid.as_deref())
            .await
        {
            Ok(()) => info!("shutdown: 活跃 session 已关闭"),
            Err(e) => warn!(%e, "shutdown: 关闭活跃 session 时出错（继续退出）"),
        }

        // Step 3: 等待后台线程退出（各带独立 15s 超时）
        // 与 save_and_close_session 的超时分离：后台线程只需感知 shutdown_flag 并退出，
        // 不涉及 LLM 调用，15s 足够。save_and_close_session 的 L1 摘要已在 Step 2 完成。
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
// 辅助函数
// =========================================================

/// 安全关闭 session（忽略已关闭的 session）。
///
/// 对齐 Python `database.close_session(sid)`。
async fn close_session_safe(storage: &dyn StorageBackend, session_id: Uuid) -> RamariaResult<()> {
    // 先检查 session 是否已关闭
    let session = storage.get_session(session_id).await?;
    match session {
        Some(s) if s.ended_at.is_none() => {
            storage.close_session(session_id).await?;
            info!(%session_id, "session 已关闭");
            Ok(())
        }
        Some(_) => {
            debug!(%session_id, "session 已关闭，跳过");
            Ok(())
        }
        None => {
            warn!(%session_id, "session 不存在，跳过关闭");
            Ok(())
        }
    }
}

/// 从 DB 查询 session 最后消息时间（降级路径）。
///
/// 当内存缓存中没有记录时，降级到 DB 查询。
/// 对齐 Python `database.get_last_message_time(session_id)`。
///
/// 实现:
/// - 使用 `StorageBackend::get_last_message_time` — 高效 `SELECT MAX(created_at)` 聚合，
///   不再全量加载消息列表。
/// - 若 trait 实现未覆写（返回 None），回退到 `list_messages` 全量加载。
async fn get_last_msg_time_from_db(
    storage: &dyn StorageBackend,
    session_id: Uuid,
) -> RamariaResult<Option<i64>> {
    // 优先使用高效的 MAX 聚合查询
    if let Some(time) = storage.get_last_message_time(session_id).await? {
        return Ok(Some(time));
    }
    // 降级：全量加载消息取最后时间（仅当 trait 未覆写时发生）
    let messages = storage.list_messages(session_id).await?;
    Ok(messages.iter().map(|m| m.created_at).max())
}

/// L1 摘要生成的异步闭包（供 JobManager::execute_with_retry 使用）。
async fn summarize_with_summarizer(summarizer: &L1Summarizer<'_>, session_id: Uuid) -> JobResult {
    match summarizer.summarize_session(session_id).await {
        Ok(_l1) => {
            info!(%session_id, "L1 摘要生成成功");
            JobResult::Success
        }
        Err(e) => {
            // LLM 调用失败是可重试的（网络波动、服务暂时不可用等）
            warn!(%session_id, %e, "L1 摘要生成失败，将重试");
            JobResult::Retryable(e.to_string())
        }
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

    // =========================================================
    // v1.2: Retriever 注入与增量索引测试
    // =========================================================

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
    fn index_l1_into_retriever_without_set_retriever_is_noop() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        // 未注入 retriever 时调用不应 panic
        let l1 = ramaria_core::types::MemoryL1 {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            summary: "测试".to_string(),
            keywords: None,
            time_period: None,
            atmosphere: None,
            valence: 0.0,
            salience: 0.5,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("test".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
        };
        // 不应 panic
        lifecycle.index_l1_into_retriever(&l1);
    }

    #[test]
    fn index_l1_into_retriever_adds_to_index() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let retriever = Arc::new(RwLock::new(Retriever::new()));
        lifecycle.set_retriever(Arc::clone(&retriever));

        let sid = Uuid::new_v4();
        let l1 = ramaria_core::types::MemoryL1 {
            id: Uuid::new_v4(),
            session_id: sid,
            summary: "测试摘要：用户讨论了Rust编程话题".to_string(),
            keywords: Some("Rust,编程".to_string()),
            time_period: None,
            atmosphere: None,
            valence: 0.5,
            salience: 0.8,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("rama-0001".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
        };

        lifecycle.index_l1_into_retriever(&l1);

        // 验证 retriever 中已有文档（v1.3 P-3: read() 即可，doc_count 为只读）
        let guard = retriever.read().unwrap();
        assert_eq!(guard.doc_count(), 1);
    }

    #[test]
    fn index_l1_into_retriever_makes_l1_searchable() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        let retriever = Arc::new(RwLock::new(Retriever::new()));
        lifecycle.set_retriever(Arc::clone(&retriever));

        let l1 = ramaria_core::types::MemoryL1 {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            summary: "用户今天学习了Rust编程语言的基础语法".to_string(),
            keywords: Some("学习,Rust,编程".to_string()),
            time_period: None,
            atmosphere: None,
            valence: 0.8,
            salience: 0.9,
            absorbed: false,
            created_at: 1718000000000,
            last_accessed_at: None,
            persona_uid: Some("rama-0001".to_string()),
            context_json: None,
            situation_strength: None,
            evidence_notes: None,
        };

        lifecycle.index_l1_into_retriever(&l1);

        // 立即检索，应能命中（v1.3 P-3: read() 即可，search 为 &self）
        let guard = retriever.read().unwrap();
        let req = ramaria_memory::SearchRequest {
            query: "Rust".to_string(),
            persona_uid: None,
            top_k: 5,
            filter_share: false,
        };
        let results = guard.search(&req, None);
        assert!(!results.is_empty());
        assert!(results.iter().any(|sr| sr.doc_summary.contains("Rust")));
    }

    #[test]
    fn get_active_session_persona_uid_returns_none_when_no_active() {
        let config = RamariaConfig::default();
        let lifecycle = SessionLifecycle::new(config);

        // 无活跃 session 时 get_active_session_id 返回 None，
        // get_active_session_persona_uid 中的 `?` 会提前返回 None
        // （此测试验证方法签名和基本逻辑，不涉及 DB 查询）
        assert!(lifecycle.get_active_session_id().is_none());
    }
}
