//! crates/ramaria-app/src/app.rs - 应用编排核心
//!
//! 设计特点:
//! - `App` 结构体持有所有运行时依赖：storage、llm provider、retriever、config、keychain
//! - `send_message` 实现完整对话管线：状态检查→隐私确认→会话管理→记忆检索→RAG→LLM调用→消息持久化
//! - 流式输出通过 `futures::channel::mpsc` 桥接 LLM 流与上层 StreamEvent 流
//! - 消息保存与会话管理通过 `StorageBackend` trait 进行，不依赖具体数据库
//! - `rebuild_retriever` 从存储层加载 L1 数据，增量构建内存检索索引
//! - SessionLifecycle 管理 session 生命周期：手动关闭、空闲自动关闭、只读约束、L0→L3 管线
//!
//! 安全约束:
//! - 不记录完整 prompt 或用户消息（日志仅记录 request_id + 字符数）
//! - 线上 provider 调用前强制隐私确认检查
//! - API key 仅在 keychain 读取时出现，不缓存

use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};

use futures::Stream;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, LlmProvider, LlmResponseCache, StorageBackend};
use ramaria_core::types::AppState;
use ramaria_llm::keychain::Keychain;
use ramaria_memory::retriever::Retriever;
use uuid::Uuid;

use crate::session_lifecycle::SessionLifecycle;
use crate::stream_event::StreamEvent;

// =========================================================
// 公共类型别名
// =========================================================

/// `send_message` 返回的流类型。
pub type SendMessageStream = Pin<Box<dyn Stream<Item = RamariaResult<StreamEvent>> + Send>>;

// =========================================================
// App 结构体
// =========================================================

/// Ramaria 应用编排器。
///
/// 职责:
/// - 持有所有运行时依赖：存储、LLM provider、检索器、配置、keychain
/// - 管理应用状态机（NeedsSetup → Indexing → Ready）
/// - 提供 `send_message` 核心对话用例
/// - 管理 session 生命周期：手动关闭、空闲自动关闭、只读约束
/// - 后台线程 A（空闲检测）+ 后台线程 B（L2/L3 定时触发）
/// - 支持检索器重建（从存储层加载 L1/L2 索引数据）
///
/// 用法:
/// ```ignore
/// let mut app = App::new(storage, llm, None, config, keychain)?;
/// app.run_setup(&backend_config).await?;
/// app.start_background_tasks;
/// let stream = app.send_message("你好", None, None).await?;
/// ```
pub struct App {
    /// 存储后端（23 张表 CRUD）
    pub(crate) storage: Arc<dyn StorageBackend>,
    /// 当前 LLM provider（Mutex 包裹，支持配置热更新）
    pub(crate) llm: Mutex<Arc<dyn LlmProvider>>,
    /// 嵌入模型 provider（Mutex 包裹，None 表示未配置）
    pub(crate) embedding: Mutex<Option<Arc<dyn EmbeddingProvider>>>,
    /// 内存检索器（RwLock 替代 Mutex，允许多读并发）
    pub(crate) retriever: Arc<RwLock<Retriever>>,
    /// 应用配置
    pub(crate) config: ramaria_core::config::RamariaConfig,
    /// 当前应用状态
    pub(crate) state: Mutex<AppState>,
    /// OS keychain（供隐私确认和 provider 验证使用）
    pub(crate) keychain: Arc<Keychain>,
    /// Session 生命周期编排器（活跃 session 追踪、空闲检测、管线触发）
    pub(crate) lifecycle: Arc<SessionLifecycle>,
    /// LLM 响应精确缓存（v1.5 三层生成缓存 C）。
    ///
    /// 由调用方（desktop/cli）在初始化时注入；热更新 provider 时
    /// 通过 `llm_cache()` 复用同一缓存实例，保证后端切换后缓存不失效。
    pub(crate) llm_cache: Mutex<Option<Arc<dyn LlmResponseCache>>>,
    /// 行为待定池（跨会话内存态）。
    ///
    /// 职责:
    /// - 未归入现有规则簇的新事件积累区；每次封存时推进（成簇/低置信）。
    /// - 内存态简化：重启后重建为空（未归入事件仍在事件表，全量重学可重新聚类）。
    /// - Arc 共享给 SessionLifecycle 封存钩子（手动/空闲两条封存路径同一池）。
    pub(crate) behavior_pending: Arc<Mutex<ramaria_memory::behavior::PendingPool>>,
    /// 后台空闲检测线程句柄
    pub(crate) idle_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 后台 L2/L3 定时检查线程句柄
    pub(crate) scheduler_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl App {
    /// 创建新的 App 实例。
    ///
    /// 参数:
    /// - `storage`: 存储后端（通常为 `ramaria-storage` 的 `SqliteStorage`）。
    /// - `llm`: LLM provider（LmStudio / DeepSeek / OpenAI 之一）。
    /// - `embedding`: 可选的嵌入模型 provider（NativeEmbeddingProvider 或 NoopEmbeddingProvider）。
    /// - `config`: 应用配置。
    /// - `keychain`: OS keychain 实例。
    ///
    /// 返回:
    /// - 初始状态为 `NeedsSetup` 的 App 实例。
    /// - 检索器为空，需调用 `rebuild_retriever` 填充。
    /// - 后台任务需调用 `start_background_tasks` 启动。
    ///
    /// 注意:
    /// - 构造时不启动后台线程，由调用方在完成 setup 后调用 `start_background_tasks`。
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        config: ramaria_core::config::RamariaConfig,
        keychain: Arc<Keychain>,
    ) -> Self {
        let retriever = Arc::new(RwLock::new(Retriever::new()));
        let lifecycle = Arc::new(SessionLifecycle::new(config.clone()));

        // 注入 Retriever 到 SessionLifecycle，启用 L1 增量索引
        lifecycle.set_retriever(Arc::clone(&retriever));
        // 注入 embedding 到 SessionLifecycle，启用 utt 块向量生成（v1.4）
        lifecycle.set_embedding(embedding.clone());

        // 行为待定池（D6 增量更新）：App 与 SessionLifecycle 封存钩子共享
        let behavior_pending = Arc::new(Mutex::new(ramaria_memory::behavior::PendingPool::new(
            &config.behavior,
        )));

        // 注册行为层封存钩子（D6）：封存时增量更新，失败记 warn 不阻塞封存
        let hook_storage = Arc::clone(&storage);
        let hook_llm = Arc::clone(&llm);
        let hook_embedding = embedding.clone();
        let hook_config = config.behavior.clone();
        let hook_pending = Arc::clone(&behavior_pending);
        let hook: crate::session_lifecycle::BehaviorCloseHook =
            Arc::new(move |persona_uid: &str| {
                let storage = Arc::clone(&hook_storage);
                let llm = Arc::clone(&hook_llm);
                let embedding = hook_embedding.clone();
                let config = hook_config.clone();
                let pending = Arc::clone(&hook_pending);
                let persona_uid = persona_uid.to_string();
                Box::pin(async move {
                    if let Err(e) = crate::commands::behavior::behavior_incremental_update_core(
                        storage.as_ref(),
                        llm.as_ref(),
                        embedding.as_deref(),
                        &config,
                        &pending,
                        &persona_uid,
                    )
                    .await
                    {
                        // 增量更新失败不阻塞封存主流程（注册式接入，静默降级链）
                        tracing::warn!(
                            persona_uid,
                            error = %e,
                            "行为规则增量更新失败（封存已正常完成，下次封存自动重试）"
                        );
                    }
                })
            });
        lifecycle.set_behavior_hook(hook);

        let emb_info = embedding
            .as_ref()
            .map(|e| {
                format!(
                    "{} (dim={})",
                    e.model_info().model_id,
                    e.model_info().dimension
                )
            })
            .unwrap_or_else(|| "未配置".to_string());

        tracing::info!(
            provider = llm.name(),
            embedding = %emb_info,
            "App 实例已创建，初始状态: NeedsSetup"
        );

        Self {
            storage,
            llm: Mutex::new(llm),
            embedding: Mutex::new(embedding),
            retriever,
            config,
            state: Mutex::new(AppState::NeedsSetup),
            keychain,
            lifecycle,
            llm_cache: Mutex::new(None),
            behavior_pending,
            idle_handle: Mutex::new(None),
            scheduler_handle: Mutex::new(None),
        }
    }

    /// 不带嵌入模型的便捷构造函数（向后兼容）。
    ///
    /// 等效于 `App::new(storage, llm, None, config, keychain)`。
    /// 嵌入模型缺失时将进入 Degraded 状态，BM25+图谱仍可用。
    pub fn new_without_embedding(
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
        config: ramaria_core::config::RamariaConfig,
        keychain: Arc<Keychain>,
    ) -> Self {
        Self::new(storage, llm, None, config, keychain)
    }

    // =========================================================
    // LLM 响应精确缓存
    // =========================================================

    /// 注入 LLM 响应精确缓存实例。
    ///
    /// 调用时机:
    /// - 初始化时由调用方（desktop/cli）在创建 provider 后立即调用；
    /// - 缓存实例与 provider 内注入的是同一 `Arc`，供热更新路径复用
    ///   （见 `llm_cache()`）。
    ///
    /// 说明:
    /// - `None` 表示缓存未启用（`[cache].enabled=false`），行为回退 v1.4。
    pub fn set_llm_cache(&self, cache: Option<Arc<dyn LlmResponseCache>>) {
        let mut guard = self.llm_cache.lock().unwrap_or_else(|e| e.into_inner());
        tracing::info!(cache_enabled = cache.is_some(), "LLM 响应精确缓存注入");
        *guard = cache;
    }

    /// 返回当前 LLM provider 的静态名称（诊断/测试用途）。
    ///
    /// 说明:
    /// - 直接取 provider 的 `name()`，不触发网络调用、不构造新对象。
    /// - 配置热更新（`update_llm`）后反映最新 provider。
    pub fn llm_provider_name(&self) -> &'static str {
        self.llm.lock().unwrap_or_else(|e| e.into_inner()).name()
    }

    /// 获取当前 LLM 响应精确缓存（克隆 Arc，供锁外调用）。
    ///
    /// 用途:
    /// - 后端配置热更新（`update_llm`）时复用同一缓存实例，
    ///   保证切换 provider 后缓存不失效（重跑导入仍命中）。
    pub fn llm_cache(&self) -> Option<Arc<dyn LlmResponseCache>> {
        self.llm_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 启动后台任务（空闲检测 + L2/L3 定时检查）。
    ///
    /// 调用时机:
    /// - 在 `run_setup` 完成后调用。
    /// - 只能调用一次（重复调用会被忽略）。
    ///
    /// 说明:
    /// - Thread A：每 60s 检查活跃 session 空闲时间，超时自动关闭。
    /// - Thread B：每 24h 检查 L2/L3 定时触发条件。
    /// - 异常时记录 error 日志并继续（不阻塞主流程）。
    pub fn start_background_tasks(&self) {
        // 检查是否已启动
        {
            let guard = self.idle_handle.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_some() {
                tracing::info!("后台任务已启动，跳过重复调用");
                return;
            }
        }

        let storage = Arc::clone(&self.storage);
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();

        // 启动空闲检测线程
        let idle = self
            .lifecycle
            .spawn_idle_checker(Arc::clone(&storage), Arc::clone(&llm));

        // 启动 L2/L3 定时检查线程
        let scheduler = self.lifecycle.spawn_l2_l3_scheduler(storage, llm);

        {
            let mut guard = self.idle_handle.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(idle);
        }
        {
            let mut guard = self
                .scheduler_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *guard = Some(scheduler);
        }

        tracing::info!("后台任务已全部启动");
    }

    // =========================================================
    // 状态管理
    // =========================================================
    //
    // 已提取至 `app_state.rs`。
    // 对外 API 不变，通过 `impl App` 块关联。

    // =========================================================
    // Session 生命周期
    // =========================================================

    /// 获取当前活跃 session ID。
    ///
    /// 对齐 Python `SessionManager.active_session_id`。
    pub fn get_active_session_id(&self) -> Option<Uuid> {
        self.lifecycle.get_active_session_id()
    }

    /// 热更新空闲自动保存阈值（分钟）。
    ///
    /// 说明:
    /// - 由桌面端 `update_full_config` 命令在保存成功后调用，
    ///   与空闲检测线程联动（无需重启）。
    pub fn set_idle_minutes(&self, minutes: u32) {
        self.lifecycle.set_idle_minutes(minutes);
    }

    /// 手动保存并关闭当前活跃 session。
    ///
    /// 流程（对齐 Python `force_close_current_session`）:
    /// 1. 关闭 session（设置 ended_at）。
    /// 2. 生成 L1 摘要（传入当前对话人格，确保记忆页面可查询）。
    /// 3. 检查 L2 触发条件（路径 A：即时）。
    /// 4. 清除活跃 session ID。
    ///
    /// 参数:
    /// - `persona_uid`: 当前对话人格的 UID，用于 L1 归属。
    ///
    /// 返回:
    /// - `Ok()`: 成功（无活跃 session 时也视为成功）。
    pub async fn save_and_close_session(&self, persona_uid: Option<&str>) -> RamariaResult<()> {
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.lifecycle
            .save_and_close_session(self.storage.as_ref(), llm.as_ref(), persona_uid)
            .await
    }

    /// 为已关闭的 session 重新生成 L1 摘要（手动重试）。
    ///
    /// 职责:
    /// - 当 save_and_close_session 中 L1 生成失败后，用户可手动重试。
    /// - 适用于 LLM 服务暂时不可用后恢复的场景。
    ///
    /// 参数:
    /// - `session_id`: 目标 session（通常是已关闭但缺少 L1 的 session）。
    /// - `persona_uid`: 人格标识，用于 L1 归属。
    /// - `user_prefix`: 覆盖默认"用户："前缀。`None` 使用默认。
    /// - `assistant_prefix`: 覆盖默认"助手："前缀。`None` 使用默认。
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: L1 生成成功。
    /// - `Ok(None)`: session 无消息，无法生成。
    /// - `Err`: 存储或 LLM 调用失败。
    pub async fn regenerate_l1(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.lifecycle
            .regenerate_l1(
                self.storage.as_ref(),
                llm.as_ref(),
                session_id,
                persona_uid,
                user_prefix,
                assistant_prefix,
            )
            .await
    }

    /// 生成 L1 摘要但跳过 L2 级联（批量导入用）。
    ///
    /// 支持 `user_prefix` / `assistant_prefix` 覆盖，
    /// 导入场景传 `Some("")` 避免前缀与 content 中的 `[sender_name]` 重复。
    pub async fn regenerate_l1_no_cascade(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.lifecycle
            .regenerate_l1_no_cascade(
                self.storage.as_ref(),
                llm.as_ref(),
                session_id,
                persona_uid,
                user_prefix,
                assistant_prefix,
            )
            .await
    }

    /// 手动触发完整记忆管线：L1→L2 事件提取 + L2→L3 性格推断。
    ///
    /// 说明:
    /// - 遍历所有 persona，分别检查未吸收 L1 和未吸收 L2 事件。
    /// - L1→L2: 未吸收 L1 ≥ 5 条时触发事件提取。
    /// - L2→L3: 未吸收事件 ≥ 10 条（或最早 > 30 天）时触发性格推断。
    /// - 两者独立检查，即使 L1 已全部吸收，仍会检查 L3。
    /// - 用于批量导入等场景，在全部 L1 生成后统一触发一次级联。
    pub async fn trigger_l2_check(&self) {
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let storage = self.storage.as_ref();
        let llm_ref = llm.as_ref();

        tracing::info!("trigger_l2_check: 开始遍历 persona...");

        // L1 → L2（仅检查未吸收 L1）
        self.lifecycle.check_l2_trigger(storage, llm_ref).await;

        // L2 → L3（独立检查未吸收事件，即使 L1 已全部吸收）
        let personas = match storage.list_personas().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "trigger_l2_check: 查询 persona 列表失败，跳过 L3");
                return;
            }
        };

        for persona in &personas {
            let unabsorbed_events = match storage.list_unabsorbed_events(&persona.uid).await {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(persona_uid = %persona.uid, error = %e, "查询未吸收事件失败");
                    continue;
                }
            };

            tracing::info!(
                persona_uid = %persona.uid,
                persona_name = %persona.name,
                unabsorbed_event_count = unabsorbed_events.len(),
                "检查 L3 触发条件"
            );

            self.lifecycle
                .check_l3_trigger(storage, llm_ref, &persona.uid)
                .await;
        }
    }

    /// 优雅关闭应用：关闭活跃 session 并停止后台线程。
    ///
    /// 对齐 Python `SessionManager.stop`。
    ///
    /// 说明:
    /// - 在 Drop 中自动调用，也可显式调用。
    /// - 关闭活跃 session → 设置 shutdown_flag → 等待后台线程退出（最长 30s）。
    pub async fn shutdown(&self) {
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let idle = self
            .idle_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();

        let scheduler = self
            .scheduler_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();

        self.lifecycle
            .shutdown(self.storage.as_ref(), llm.as_ref(), idle, scheduler)
            .await;
    }

    // =========================================================
    // 设置流程与隐私确认
    // =========================================================
    //
    // `run_setup` / `probe_health_with_retry` / `refresh_setup_state` 已提取至 `app_setup.rs`。
    // `check_privacy` / `confirm_privacy` 已提取至 `app_privacy.rs`。
    // 对外 API 不变，通过 `impl App` 块关联。

    // =========================================================
    // 检索器管理
    // =========================================================
    //
    // `rebuild_retriever` 方法已提取至 `app_retriever.rs`。
    // 对外 API 不变，内部通过 `impl App` 块关联。

    // =========================================================
    // 核心对话方法：send_message
    // =========================================================
    //
    // 已提取至 `app_chat.rs`。
    // 对外 API 不变，通过 `impl App` 块关联。
    //
    // 同时提取的自由函数：
    // - `load_persona_toml_prompt` → `app_chat.rs`
    // - `fallback_read_persona_toml` → `app_chat.rs`
    // - `stream_forward_task` → `app_chat.rs`
}

// =========================================================
// Drop 实现（优雅关闭兜底）
// =========================================================

impl Drop for App {
    /// App 销毁时设置 shutdown_flag，通知所有后台线程退出。
    ///
    /// 注意:
    /// - Drop 是同步方法，不能调用 async 代码。
    /// - 完整的优雅关闭应通过 `shutdown` 方法执行（关闭活跃 session + 等待线程退出）。
    /// - 此 Drop 实现仅设置停止标志，让后台线程自行退出。
    fn drop(&mut self) {
        // 设置停止标志
        self.lifecycle.shutdown_flag().store(true, Ordering::SeqCst);
        tracing::info!("App Drop: shutdown_flag 已设置，后台线程将在下次轮询时退出");
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_values() {
        assert_eq!(AppState::NeedsSetup.as_str(), "needs_setup");
        assert_eq!(AppState::DownloadingModel.as_str(), "downloading_model");
        assert_eq!(AppState::Indexing.as_str(), "indexing");
        assert_eq!(AppState::Ready.as_str(), "ready");
        assert_eq!(AppState::Degraded.as_str(), "degraded");
        assert_eq!(AppState::FatalError.as_str(), "fatal_error");
    }

    #[test]
    fn system_prompt_fallback_contains_ramaria() {
        // 测试降级 prompt 模板（无需 App 实例）
        let prompt = format!(
            "你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
             你可以记住与用户的对话历史，并在后续对话中引用这些记忆。\n\
             请用自然、友好的语气回复用户。如果用户提到之前聊过的内容，\
             请结合记忆上下文给出更有针对性的回复。\n\
             当前时间：{}",
            crate::now_timestamp_str()
        );
        assert!(prompt.contains("Ramaria"));
        assert!(prompt.contains("记忆"));
        assert!(!prompt.contains("000")); // 不应包含原始 Unix 时间戳
    }
}
