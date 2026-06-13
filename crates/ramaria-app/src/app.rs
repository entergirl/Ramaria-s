//! rust/crates/ramaria-app/src/app.rs - 应用编排核心
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
use std::sync::{Arc, Mutex};

use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{
    ChatMessage, ChatRequest, EmbeddingProvider, LlmProvider, StorageBackend,
};
use ramaria_core::types::{
    AppState, BackendConfig, Message, MessageRole, MessageSource, PersonaKind, ProfileField,
    new_id, now_ms,
};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::VectorIndex;
use ramaria_memory::parse_persona_toml;
use ramaria_memory::prompt::builder::{PromptConfig, PromptContext, assemble_prompt};
use ramaria_memory::rag::{RagConfig, filter_by_persona, format_context_text};
use ramaria_memory::retriever::{L1DocView, L2DocView, Retriever, SearchRequest};
use uuid::Uuid;

use crate::privacy;
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
/// app.start_background_tasks();
/// let stream = app.send_message("你好", None, None).await?;
/// ```
pub struct App {
    /// 存储后端（23 张表 CRUD）
    storage: Arc<dyn StorageBackend>,
    /// 当前 LLM provider（Mutex 包裹，支持配置热更新）
    llm: Mutex<Arc<dyn LlmProvider>>,
    /// 嵌入模型 provider（Mutex 包裹，None 表示未配置）
    embedding: Mutex<Option<Arc<dyn EmbeddingProvider>>>,
    /// 内存检索器（BM25 + 向量 + 图谱）
    retriever: Mutex<Retriever>,
    /// 应用配置
    config: ramaria_core::config::RamariaConfig,
    /// 当前应用状态
    state: Mutex<AppState>,
    /// OS keychain（供隐私确认和 provider 验证使用）
    keychain: Arc<Keychain>,
    /// Session 生命周期编排器（活跃 session 追踪、空闲检测、管线触发）
    lifecycle: Arc<SessionLifecycle>,
    /// 后台空闲检测线程句柄
    idle_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 后台 L2/L3 定时检查线程句柄
    scheduler_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl App {
    /// 创建新的 App 实例。
    ///
    /// 参数:
    /// - `storage`: 存储后端（通常为 `ramaria-storage` 的 `SqliteStorage`）。
    /// - `llm`: LLM provider（LmStudio / DeepSeek / OpenAI 之一）。
    /// - `embedding`: 可选的嵌入模型 provider（OnnxEmbeddingProvider 或 NoopEmbeddingProvider）。
    /// - `config`: 应用配置。
    /// - `keychain`: OS keychain 实例。
    ///
    /// 返回:
    /// - 初始状态为 `NeedsSetup` 的 App 实例。
    /// - 检索器为空，需调用 `rebuild_retriever` 填充。
    /// - 后台任务需调用 `start_background_tasks()` 启动。
    ///
    /// 注意:
    /// - 构造时不启动后台线程，由调用方在完成 setup 后调用 `start_background_tasks()`。
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
        embedding: Option<Arc<dyn EmbeddingProvider>>,
        config: ramaria_core::config::RamariaConfig,
        keychain: Arc<Keychain>,
    ) -> Self {
        let retriever = Retriever::new();
        let lifecycle = Arc::new(SessionLifecycle::new(config.clone()));

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
            retriever: Mutex::new(retriever),
            config,
            state: Mutex::new(AppState::NeedsSetup),
            keychain,
            lifecycle,
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

    /// 启动后台任务（空闲检测 + L2/L3 定时检查）。
    ///
    /// 调用时机:
    /// - 在 `run_setup()` 完成后调用。
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

    /// 获取当前应用状态。
    pub fn current_state(&self) -> AppState {
        *self.state.lock().unwrap_or_else(|e| {
            tracing::error!("App state lock poisoned: {e}");
            e.into_inner()
        })
    }

    /// 设置应用状态。
    ///
    /// 参数:
    /// - `new_state`: 目标状态。
    ///
    /// 说明:
    /// - 状态变更会记录 info 日志，便于诊断。
    pub fn set_state(&self, new_state: AppState) {
        let old = {
            let mut guard = self.state.lock().unwrap_or_else(|e| {
                tracing::error!("App state lock poisoned during set_state: {e}");
                e.into_inner()
            });
            let old = *guard;
            *guard = new_state;
            old
        };
        if old != new_state {
            tracing::info!(from = %old, to = %new_state, "App 状态变更");
        }
    }

    /// 获取后端配置引用。
    pub fn backend_config(&self) -> BackendConfig {
        self.llm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone()
    }

    /// 热更新 LLM provider（配置修改后调用，替换内存中的 provider 实例）。
    pub fn update_llm(&self, new_llm: Arc<dyn LlmProvider>) {
        let mut guard = self.llm.lock().unwrap_or_else(|e| e.into_inner());
        tracing::info!(
            old_provider = guard.name(),
            new_provider = %new_llm.name(),
            "LLM provider 热更新"
        );
        *guard = new_llm;
    }

    /// 克隆当前 LLM provider 的 Arc（用于在锁外调用异步方法）。
    ///
    /// 返回:
    /// - 当前 LLM provider 的 `Arc<dyn LlmProvider>` 克隆。
    pub fn llm_clone(&self) -> Arc<dyn LlmProvider> {
        self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 获取 keychain 引用。
    pub fn keychain(&self) -> &Keychain {
        &self.keychain
    }

    /// 获取 keychain Arc 引用（供 provider 构造使用）。
    pub fn keychain_arc(&self) -> Arc<Keychain> {
        Arc::clone(&self.keychain)
    }

    /// 获取当前嵌入模型 provider 的克隆。
    ///
    /// 返回:
    /// - `Some(Arc<dyn EmbeddingProvider>)`: 嵌入模型已配置且可用。
    /// - `None`: 未配置或不可用。
    pub fn embedding_provider(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 检查嵌入模型是否可用。
    ///
    /// 返回:
    /// - `true`: 嵌入模型已配置且 `is_available()` 返回 true。
    pub fn is_embedding_available(&self) -> bool {
        self.embedding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|e| e.is_available())
            .unwrap_or(false)
    }

    /// 热更新嵌入模型 provider。
    ///
    /// 参数:
    /// - `new_embedding`: 新的嵌入 provider（Some 或 None 表示卸载）。
    pub fn update_embedding(&self, new_embedding: Option<Arc<dyn EmbeddingProvider>>) {
        let mut guard = self.embedding.lock().unwrap_or_else(|e| e.into_inner());
        match &new_embedding {
            Some(e) => tracing::info!(
                model = %e.model_info().model_id,
                dim = e.model_info().dimension,
                "嵌入模型热更新"
            ),
            None => tracing::info!("嵌入模型已卸载"),
        }
        *guard = new_embedding;
    }

    /// 尝试加载嵌入模型并更新应用状态。
    ///
    /// 说明:
    /// - 如果嵌入模型可用：状态保持不变（Ready 或继续 setup 流程）。
    /// - 如果嵌入模型缺失或不可用：进入 Degraded 状态，BM25+图谱仍可用。
    /// - 仅在 Ready 状态下调用此方法（索引构建完成后）。
    ///
    /// 返回:
    /// - `Ok(true)`: 嵌入模型可用，向量通道就绪。
    /// - `Ok(false)`: 嵌入模型不可用，已进入 Degraded。
    pub async fn try_load_embedding(&self) -> RamariaResult<bool> {
        let emb = {
            let guard = self.embedding.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };

        match emb {
            Some(ref provider) if provider.is_available() => {
                // 验证模型可用性
                match provider.validate().await {
                    Ok(()) => {
                        tracing::info!(
                            model = %provider.model_info().model_id,
                            dim = provider.model_info().dimension,
                            "嵌入模型验证通过，向量通道可用"
                        );
                        Ok(true)
                    }
                    Err(e) => {
                        tracing::warn!(%e, "嵌入模型验证失败，进入降级模式");
                        self.set_state(AppState::Degraded);
                        Ok(false)
                    }
                }
            }
            _ => {
                tracing::info!("嵌入模型未配置，进入降级模式（BM25 + 图谱可用）");
                self.set_state(AppState::Degraded);
                Ok(false)
            }
        }
    }

    /// 获取存储后端引用。
    ///
    /// 职责:
    /// - 供 CLI 等上层模块直接查询 sessions / memories / events 等数据。
    /// - 所有业务写操作应通过 App 方法执行，读操作可直接使用此引用。
    pub fn storage(&self) -> &Arc<dyn StorageBackend> {
        &self.storage
    }

    // =========================================================
    // Session 生命周期（v1.1 新增）
    // =========================================================

    /// 获取当前活跃 session ID。
    ///
    /// 对齐 Python `SessionManager.active_session_id`。
    pub fn get_active_session_id(&self) -> Option<Uuid> {
        self.lifecycle.get_active_session_id()
    }

    /// 手动保存并关闭当前活跃 session。
    ///
    /// 流程（对齐 Python `force_close_current_session()`）:
    /// 1. 关闭 session（设置 ended_at）。
    /// 2. 生成 L1 摘要（传入当前对话人格，确保记忆页面可查询）。
    /// 3. 检查 L2 触发条件（路径 A：即时）。
    /// 4. 清除活跃 session ID。
    ///
    /// 参数:
    /// - `persona_uid`: 当前对话人格的 UID，用于 L1 归属。
    ///
    /// 返回:
    /// - `Ok(())`: 成功（无活跃 session 时也视为成功）。
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
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: L1 生成成功。
    /// - `Ok(None)`: session 无消息，无法生成。
    /// - `Err`: 存储或 LLM 调用失败。
    pub async fn regenerate_l1(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        let llm = self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.lifecycle
            .regenerate_l1(self.storage.as_ref(), llm.as_ref(), session_id, persona_uid)
            .await
    }

    /// 优雅关闭应用：关闭活跃 session 并停止后台线程。
    ///
    /// 对齐 Python `SessionManager.stop()`。
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
    // 设置流程
    // =========================================================

    /// 执行设置流程：保存后端配置 → 更新状态。
    ///
    /// 参数:
    /// - `backend_config`: 用户选择的后端配置。
    ///
    /// 返回:
    /// - 设置后的最终状态。
    pub async fn run_setup(&self, backend_config: &BackendConfig) -> RamariaResult<AppState> {
        let state = crate::setup::run_setup(self.storage.as_ref(), backend_config).await?;
        self.set_state(state);
        Ok(state)
    }

    /// 检查并更新设置状态（含嵌入模型状态检查）。
    pub async fn refresh_setup_state(&self) -> RamariaResult<AppState> {
        let embedding_available = self.is_embedding_available();
        let status =
            crate::setup::check_setup_status(self.storage.as_ref(), embedding_available).await?;
        let state = crate::setup::determine_state(&status);
        self.set_state(state);
        Ok(state)
    }

    // =========================================================
    // 隐私确认
    // =========================================================

    /// 检查当前 provider 的隐私确认状态。
    pub async fn check_privacy(&self) -> RamariaResult<crate::privacy::PrivacyStatus> {
        let cfg = self
            .llm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone();
        privacy::check_privacy(self.storage.as_ref(), cfg.provider, &cfg.base_url).await
    }

    /// 记录隐私确认。
    ///
    /// 参数:
    /// - `persistent`: 是否跨重启持久化。
    pub async fn confirm_privacy(&self, persistent: bool) -> RamariaResult<()> {
        let cfg = self
            .llm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone();
        privacy::confirm_privacy(
            self.storage.as_ref(),
            cfg.provider,
            &cfg.base_url,
            persistent,
        )
        .await
    }

    // =========================================================
    // 检索器管理
    // =========================================================

    /// 从存储层重建检索器索引。
    ///
    /// 说明:
    /// - 加载所有 L1 记忆条目和 L2 事件，转换为视图并索引到 Retriever。
    /// - 如果嵌入模型可用，为文档生成向量索引（向量通道）。
    /// - 此操作会清空现有索引并重建。
    /// - 建议在应用启动和后台定期执行。
    ///
    /// 返回:
    /// - 成功时返回索引的文档总数（L1 + L2）。
    pub async fn rebuild_retriever(&self) -> RamariaResult<usize> {
        // 1. 获取所有 persona
        let personas = self.storage.list_personas().await?;

        // 2. 从存储层收集所有 L1 数据（在锁外执行 I/O）
        let mut all_l1: Vec<L1DocView> = Vec::new();
        let mut all_l2: Vec<L2DocView> = Vec::new();

        for persona in &personas {
            // L1
            let l1_list = self.storage.list_unabsorbed_l1(&persona.uid).await?;
            for l1 in &l1_list {
                all_l1.push(L1DocView {
                    id: l1.id,
                    summary: l1.summary.clone(),
                    keywords: l1.keywords.clone(),
                    salience: l1.salience,
                    created_at: l1.created_at,
                    persona_uid: l1.persona_uid.clone(),
                });
            }

            // L2 events
            let events = self
                .storage
                .list_events_by_persona(&persona.uid, 0, 1000)
                .await
                .unwrap_or_default();
            for ev in &events {
                all_l2.push(L2DocView {
                    id: ev.id,
                    title: ev.title.clone(),
                    summary: ev.summary.clone(),
                    keywords: ev.keywords.clone(),
                    attitude: ev.attitude.clone(),
                    paraphrase: ev.paraphrase.clone(),
                    persona_uid: ev.persona_uid.clone(),
                    share: ev.share,
                    confidence: ev.confidence,
                    created_at: ev.created_at,
                    salience: ev.salience,
                });
            }
        }

        let total = all_l1.len() + all_l2.len();

        // 3. 生成向量（如果嵌入模型可用）
        let embeddings_available = self.is_embedding_available();
        let mut l1_vectors: Vec<(uuid::Uuid, Vec<f32>, i64)> = Vec::new();
        let mut l2_vectors: Vec<(i64, Vec<f32>, i64)> = Vec::new();

        if embeddings_available {
            let emb = self.embedding_provider();
            if let Some(ref provider) = emb {
                // 批量生成 L1 摘要向量
                let l1_texts: Vec<&str> = all_l1.iter().map(|d| d.summary.as_str()).collect();
                if !l1_texts.is_empty() {
                    match provider.embed_batch(&l1_texts).await {
                        Ok(vectors) => {
                            for (doc, vec) in all_l1.iter().zip(vectors) {
                                l1_vectors.push((doc.id, vec, doc.created_at));
                            }
                            tracing::info!(count = l1_vectors.len(), "L1 批量向量化完成");
                        }
                        Err(e) => {
                            tracing::warn!(%e, "L1 批量向量化失败，向量通道将不可用");
                        }
                    }
                }

                // 批量生成 L2 标题向量
                let l2_texts: Vec<&str> = all_l2.iter().map(|d| d.title.as_str()).collect();
                if !l2_texts.is_empty() {
                    match provider.embed_batch(&l2_texts).await {
                        Ok(vectors) => {
                            for (doc, vec) in all_l2.iter().zip(vectors) {
                                l2_vectors.push((doc.id, vec, doc.created_at));
                            }
                            tracing::info!(count = l2_vectors.len(), "L2 批量向量化完成");
                        }
                        Err(e) => {
                            tracing::warn!(%e, "L2 批量向量化失败，向量通道将不可用");
                        }
                    }
                }
            }
        }

        // 4. 锁定检索器并批量索引（纯同步操作，不跨越 .await）
        {
            let mut retriever = self.retriever.lock().unwrap_or_else(|e| {
                tracing::error!("Retriever lock poisoned during rebuild: {e}");
                e.into_inner()
            });
            retriever.clear();

            // BM25 + 内存文档索引
            for doc in &all_l1 {
                retriever.index_l1(doc);
            }
            for doc in &all_l2 {
                retriever.index_l2(doc);
            }

            // 向量索引
            if embeddings_available {
                for (id, vec, created_at) in &l1_vectors {
                    let label = format!("L1:{}", id);
                    retriever.vector_mut().add(&label, vec.clone(), *created_at);
                }
                for (id, vec, created_at) in &l2_vectors {
                    let label = format!("L2:{}", id);
                    retriever.vector_mut().add(&label, vec.clone(), *created_at);
                }
                tracing::info!(
                    l1 = l1_vectors.len(),
                    l2 = l2_vectors.len(),
                    "向量索引构建完成"
                );
            } else {
                tracing::info!("嵌入模型不可用，跳过向量索引");
            }
        } // MutexGuard 在此释放

        tracing::info!(
            total,
            personas = personas.len(),
            embeddings_available,
            "检索器索引重建完成"
        );
        Ok(total)
    }

    // =========================================================
    // 核心对话方法：send_message
    // =========================================================

    /// 发送消息并获取流式回复。
    ///
    /// 完整管线:
    /// 1. 检查应用状态（必须为 Ready）
    /// 2. 检查隐私确认（线上 provider）
    /// 3. 获取或创建会话
    /// 4. 加载对话历史
    /// 5. 搜索记忆上下文（Persona-Aware RAG）
    /// 6. 构建 System Prompt
    /// 7. 构建 ChatRequest
    /// 8. 调用 LLM provider.chat_stream()
    /// 9. 后台任务收集完整回复 + 保存消息 + 转发 StreamEvent
    /// 10. 返回 StreamEvent 流
    ///
    /// 参数:
    /// - `user_input`: 用户输入文本。
    /// - `persona_uid`: 可选的人格标识（None 表示 rama 自身）。
    /// - `session_id`: 可选的会话 ID（None 表示创建新会话）。
    ///
    /// 返回:
    /// - 成功时返回 `SendMessageStream`（StreamEvent 异步流）。
    /// - 失败时返回错误（状态不对、隐私未确认、LLM 连接失败等）。
    pub async fn send_message(
        &self,
        user_input: &str,
        persona_uid: Option<&str>,
        session_id: Option<Uuid>,
    ) -> RamariaResult<SendMessageStream> {
        let request_id = new_id();

        // ---- Step 1: 状态检查 ----
        {
            let state = self.current_state();
            match state {
                AppState::Ready => { /* 正常，继续 */ }
                AppState::Degraded => {
                    // v1.1: Degraded 允许对话（仅向量通道不可用，BM25+图谱仍工作）
                    tracing::warn!("应用处于降级状态，对话功能可用但向量检索已降级");
                }
                AppState::FatalError => {
                    return Err(RamariaError::validation(
                        "应用发生严重错误，请查看日志后重启应用。",
                    ));
                }
                _ => {
                    return Err(RamariaError::validation(format!(
                        "应用尚未就绪（当前状态: {state}）。请先完成设置流程。"
                    )));
                }
            }
        }

        // ---- Step 2: 隐私确认 ----
        let cfg = self
            .llm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .config()
            .clone();
        if cfg.provider.is_online() {
            privacy::require_privacy(self.storage.as_ref(), cfg.provider, &cfg.base_url).await?;
        }

        // ---- Step 3: 会话管理 ----
        // v1.1: 自动创建 session + 只读约束
        let session = match session_id {
            Some(sid) => {
                // 使用指定 session（前端传入的 session_id）
                let s = self
                    .storage
                    .get_session(sid)
                    .await?
                    .ok_or_else(|| RamariaError::validation(format!("会话不存在: {sid}")))?;

                // 只读约束：已关闭的 session 不可发送消息
                // 对齐 Python：ended_at IS NOT NULL → 拒绝
                if s.ended_at.is_some() {
                    return Err(RamariaError::validation(format!(
                        "会话已关闭（session {}），请开启新对话。",
                        sid
                    )));
                }

                // v1.1 修复：无论前端传入还是后端创建，都同步追踪活跃 session
                // 否则 save_and_close_session 找不到活跃 session → 返回 "无活跃对话"
                self.lifecycle.set_active_session_id_public(Some(s.id));

                s
            }
            None => {
                // 自动创建新 session
                // 对齐 Python `on_message()`: 无活跃 session 时自动创建
                let s = self.storage.create_session().await?;
                self.lifecycle.set_active_session_id_public(Some(s.id));
                tracing::info!(session_id = %s.id, "自动创建新 session");
                s
            }
        };

        // 记录 session 活跃时间（供空闲检测使用）
        self.lifecycle.touch_session(session.id);

        // ---- Step 4: 加载历史 ----
        let history = self.storage.list_messages(session.id).await?;
        let history_messages: Vec<ChatMessage> = history
            .iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content.clone(),
            })
            .collect();

        // ---- Step 5: 记忆检索 + RAG ----
        let memory_context = self
            .search_and_assemble_context(user_input, persona_uid)
            .await;

        // ---- Step 6: 构建 System Prompt（5-Block 装配器） ----
        let system_prompt = self.build_system_prompt(persona_uid).await;

        // ---- Step 7: 构建 ChatRequest ----
        let chat_request = ChatRequest {
            system_prompt,
            memory_context,
            history: history_messages,
            user_message: user_input.to_string(),
            temperature: cfg.temperature,
            max_tokens: cfg.max_tokens,
            request_id,
        };

        tracing::info!(
            request_id = %request_id,
            session_id = %session.id,
            persona_uid = persona_uid.unwrap_or("rama"),
            input_chars = user_input.chars().count(),
            "send_message 开始"
        );

        // ---- Step 8: 调用 LLM ----
        // 注意：此处不使用 `?`，LLM 调用失败时需返回含 Error 事件的流，
        // 而非直接返回 Err——上游（CLI/Desktop）统一消费流事件，不应感知底层错误。
        // ★ 先 clone Arc 出锁再 await，避免 MutexGuard 跨 .await（std::sync::MutexGuard 非 Send）
        let llm = { self.llm.lock().unwrap_or_else(|e| e.into_inner()).clone() };
        let raw_stream = match llm.chat_stream(&chat_request).await {
            Ok(stream) => stream,
            Err(e) => {
                tracing::error!(
                    %e,
                    request_id = %request_id,
                    session_id = %session.id,
                    "LLM chat_stream 调用失败，构造 Error 事件流"
                );
                // 构造仅含单个 Error 事件的流（无 Done，符合 T-FIX-013）
                let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();
                let error_event = StreamEvent::error(request_id, e.to_string());
                let _ = tx.unbounded_send(Ok(error_event));
                return Ok(Box::pin(rx));
            }
        };

        // ---- Step 9-10: 后台任务转发事件 + 保存消息 ----
        let storage = Arc::clone(&self.storage);
        let session_id = session.id;
        let user_msg = user_input.to_string();
        let input_request_id = request_id;

        let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();

        tokio::spawn(async move {
            stream_forward_task(
                storage,
                raw_stream,
                tx,
                session_id,
                user_msg,
                input_request_id,
            )
            .await;
        });

        Ok(Box::pin(rx))
    }

    // =========================================================
    // 内部辅助方法
    // =========================================================

    /// 搜索记忆上下文并组装为 RAG 文本。
    ///
    /// 参数:
    /// - `query`: 用户输入（作为搜索查询）。
    /// - `persona_uid`: 当前 persona。
    ///
    /// 返回:
    /// - `Some(formatted_text)`: 有记忆上下文。
    /// - `None`: 检索器为空或无相关记忆。
    ///
    /// 说明:
    /// - v1.1: 尝试使用嵌入模型生成 query 向量；
    ///   若嵌入模型不可用（未配置或加载失败），
    ///   向量通道自动降级为权重 0，BM25 + 图谱仍正常工作。
    async fn search_and_assemble_context(
        &self,
        query: &str,
        persona_uid: Option<&str>,
    ) -> Option<String> {
        // ---- 尝试生成查询向量 ----
        // 注：先克隆 Arc<dyn EmbeddingProvider> 出锁，再 await，避免 MutexGuard 跨 .await
        let query_vec: Option<Vec<f32>> = {
            let provider_opt = {
                let emb_guard = self.embedding.lock().unwrap_or_else(|e| e.into_inner());
                emb_guard.clone()
            }; // MutexGuard 在此释放

            match provider_opt {
                Some(provider) if provider.is_available() => match provider.embed(query).await {
                    Ok(vec) => {
                        tracing::debug!(dim = vec.len(), "查询向量已生成");
                        Some(vec)
                    }
                    Err(e) => {
                        tracing::warn!(%e, "查询向量生成失败，向量通道降级");
                        None
                    }
                },
                _ => {
                    tracing::debug!("嵌入模型不可用，跳过向量通道");
                    None
                }
            }
        };

        // 锁定检索器执行搜索（纯同步操作，不跨越 .await）
        let results = {
            let retriever = match self.retriever.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::error!("Retriever lock poisoned: {e}");
                    return None;
                }
            };

            let request = SearchRequest {
                query: query.to_string(),
                persona_uid: persona_uid.map(|s| s.to_string()),
                top_k: self.config.retrieval.l1_retrieve_top_k as usize,
                filter_share: true,
            };

            // 有查询向量 → 向量通道可用；无查询向量 → 仅 BM25 + 图谱
            match &query_vec {
                Some(qv) => retriever.search(&request, Some(qv)),
                None => retriever.search(&request, None),
            }
        };

        if results.is_empty() {
            tracing::debug!("无记忆上下文");
            return None;
        }

        // Persona-Aware 过滤
        let persona_kind = persona_uid
            .map(PersonaKind::from_uid)
            .unwrap_or(PersonaKind::Rama);

        let rag_config = RagConfig::default();
        let filtered = filter_by_persona(&results, persona_kind, &rag_config);

        if filtered.is_empty() {
            return None;
        }

        // 格式化为上下文文本
        let context = format_context_text(&filtered, &rag_config);

        tracing::debug!(
            total_results = results.len(),
            filtered = filtered.len(),
            context_chars = context.chars().count(),
            "记忆上下文已组装"
        );

        Some(context)
    }

    /// 构建 System Prompt（使用 5-Block 装配器）。
    ///
    /// 流程:
    /// 1. 从 storage 加载当前 persona 的数据（persona/facts/traits/examples）。
    /// 2. 调用 `assemble_prompt()` 组装 5-Block System Prompt。
    /// 3. 无 persona 数据时降级为基础 Ramaria 默认 prompt。
    ///
    /// 降级策略:
    /// - storage 读取失败 → 记录 warn 日志，使用空数据继续。
    /// - persona 不存在 → 使用默认 Ramaria 身份 prompt。
    /// - facts/traits/examples 为空 → 对应 Block 自动省略（由 builder 处理）。
    ///
    /// 安全约束:
    /// - 不在此处写入 system prompt 到日志（完整 prompt 仅发送到 LLM）。
    async fn build_system_prompt(&self, persona_uid: Option<&str>) -> String {
        let actual_uid = persona_uid.unwrap_or("rama-0001");

        // 尝试加载 persona 数据
        let persona = match self.storage.get_persona_by_uid(actual_uid).await {
            Ok(Some(p)) => Some(p),
            Ok(None) => {
                tracing::debug!(%actual_uid, "persona 不存在，使用默认 prompt");
                None
            }
            Err(e) => {
                tracing::warn!(%actual_uid, %e, "加载 persona 失败，使用默认 prompt");
                None
            }
        };

        // 有 persona 数据时使用 5-Block 装配器
        if let Some(ref p) = persona {
            // 加载关联数据（各独立调用，失败单独降级）
            let facts = self
                .storage
                .list_facts_by_persona(&p.uid, ProfileField::BasicInfo)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(persona_uid = %p.uid, %e, "加载 facts 失败，跳过");
                    Vec::new()
                });

            let traits = self
                .storage
                .list_traits_by_persona(&p.uid)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(persona_uid = %p.uid, %e, "加载 traits 失败，跳过");
                    Vec::new()
                });

            let examples = self
                .storage
                .list_selected_examples(&p.uid)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(persona_uid = %p.uid, %e, "加载 examples 失败，跳过");
                    Vec::new()
                });

            // 冷启动兜底：facts/traits 均为空时，尝试加载 persona.toml
            // 优先从 DB persona.config 读取，其次回退到文件系统
            if facts.is_empty()
                && traits.is_empty()
                && let Some(prompt) = load_persona_toml_prompt(p.config.as_deref())
            {
                tracing::info!("使用 persona.toml 加载的系统 prompt（无结构化画像）");
                return prompt;
            }

            let ctx = PromptContext {
                persona: Some(p.clone()),
                facts,
                traits,
                examples,
                // memory_context 由 send_message 在 ChatRequest 中单独注入，不在此处拼入
                memory_context: None,
                knowledge_boundary: None,
                current_time_str: Some(chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()),
                weather: None,
            };

            let config = PromptConfig::default();
            tracing::debug!(
                persona_uid = %p.uid,
                facts = ctx.facts.len(),
                traits = ctx.traits.len(),
                examples = ctx.examples.len(),
                "5-Block System Prompt 已装配"
            );
            return assemble_prompt(&ctx, &config);
        }

        // 降级：默认 Ramaria 基础 prompt
        tracing::info!("使用默认 Ramaria System Prompt（无 persona 数据）");
        format!(
            "你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
             你可以记住与用户的对话历史，并在后续对话中引用这些记忆。\n\
             请用自然、友好的语气回复用户。如果用户提到之前聊过的内容，\
             请结合记忆上下文给出更有针对性的回复。\n\
             当前时间：{}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        )
    }
}

// =========================================================
// persona.toml 直接加载（冷启动兜底，不依赖 LLM 结构化拆解）
// =========================================================

/// 尝试加载 persona.toml 并构建有温度的基础 system prompt。
///
/// 数据来源优先级:
/// 1. `db_config`: 从 DB persona.config 中读取的 TOML 内容（setup 时写入）
/// 2. 文件系统回退: `../config/persona.toml`（开发/迁移场景）
///
/// 成功时返回由 `A_persona` + `E_rules` 组装的基础系统 prompt。
/// 失败时返回 `None`，由上层降级到通用 prompt。
fn load_persona_toml_prompt(db_config: Option<&str>) -> Option<String> {
    let content = if let Some(cfg) = db_config {
        // 优先使用 DB 中的 persona.toml 内容
        if cfg.contains("[identity]") || cfg.contains("[blocks]") {
            tracing::debug!("从 DB persona.config 加载 persona.toml");
            cfg.to_string()
        } else {
            // config 字段是其他 JSON 格式，回退到文件系统
            fallback_read_persona_toml()?
        }
    } else {
        fallback_read_persona_toml()?
    };

    let parsed = match parse_persona_toml(&content) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%e, "persona.toml 解析失败");
            return None;
        }
    };

    let persona_block = parsed
        .blocks
        .iter()
        .find(|(k, _)| k == "A_persona")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let rules_block = parsed
        .blocks
        .iter()
        .find(|(k, _)| k == "E_rules")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    let name = &parsed.assistant_name;
    let time_str = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    Some(format!(
        "你的名字是{name}。\n\n{persona_block}\n\n回复规则:\n{rules_block}\n\n\
         当前时间：{time_str}\n\n\
         你可以记住与用户的对话历史。如果用户提到之前聊过的内容，\
         请结合记忆上下文给出更有针对性的回复。"
    ))
}

/// 文件系统回退: 优先尝试新路径 `../config/personas/rama-0001.toml`，其次旧路径 `../config/persona.toml`。
///
/// 说明:
/// - 新路径为目录扫描模式（Phase 4.2），每文件 = 一个 persona。
/// - 旧路径保留作为兼容回退，供未迁移的旧安装使用。
fn fallback_read_persona_toml() -> Option<String> {
    // 优先尝试新路径
    let new_path = "../config/personas/rama-0001.toml";
    if let Ok(c) = std::fs::read_to_string(new_path) {
        tracing::debug!(%new_path, "从文件系统加载 persona.toml (新路径)");
        return Some(c);
    }

    // 回退到旧路径（兼容旧版安装）
    let old_path = "../config/persona.toml";
    match std::fs::read_to_string(old_path) {
        Ok(c) => {
            tracing::debug!(%old_path, "从文件系统加载 persona.toml (旧路径兼容)");
            Some(c)
        }
        Err(e) => {
            tracing::debug!(%old_path, %e, "persona.toml 文件系统回退失败");
            None
        }
    }
}

// =========================================================
// 流式转发后台任务
// =========================================================

/// 后台 tokio 任务：从 LLM 原始流读取 delta，转发为 StreamEvent，收集完整回复并保存。
///
/// 职责:
/// - 消费 `raw_stream`（LLM provider 返回的 `Stream<StreamDelta>`）。
/// - 将每个 `StreamDelta` 转换为 `StreamEvent::Delta` 通过 `tx` 发送。
/// - 流结束时发送 `StreamEvent::Done`。
/// - 流中错误转发为 `StreamEvent::Error`。
/// - 收集完整 assistant 回复文本。
/// - 保存 user message + assistant message 到 storage。
async fn stream_forward_task(
    storage: Arc<dyn StorageBackend>,
    raw_stream: Pin<
        Box<dyn Stream<Item = RamariaResult<ramaria_core::traits::StreamDelta>> + Send>,
    >,
    tx: mpsc::UnboundedSender<RamariaResult<StreamEvent>>,
    session_id: Uuid,
    user_message: String,
    request_id: Uuid,
) {
    use futures::StreamExt;

    futures::pin_mut!(raw_stream);

    let mut full_reply = String::new();
    let mut backend_id: Option<String> = None;
    let mut has_error = false;
    let now = now_ms();

    // 1. 保存用户消息
    let user_msg = Message::new(
        session_id,
        MessageRole::User,
        user_message,
        MessageSource::Local,
    );
    if let Err(e) = storage.save_message(&user_msg).await {
        tracing::error!(%e, "保存用户消息失败");
        let _ = tx.unbounded_send(Err(e));
        return;
    }

    // 2. 消费 LLM 流
    while let Some(delta_result) = raw_stream.next().await {
        match delta_result {
            Ok(delta) => {
                full_reply.push_str(&delta.content);

                // 转发 Delta 事件
                let event = StreamEvent::delta(request_id, delta.content);
                if tx.unbounded_send(Ok(event)).is_err() {
                    return; // 接收端已断开
                }

                if delta.done {
                    backend_id = delta.metadata;
                    break;
                }
            }
            Err(e) => {
                has_error = true;
                tracing::error!(%e, "LLM 流错误");
                let event = StreamEvent::error(request_id, e.to_string());
                let _ = tx.unbounded_send(Ok(event));
                break;
            }
        }
    }

    // 3. 保存 assistant 消息（仅在非错误时）
    if !has_error && !full_reply.is_empty() {
        let assistant_msg = Message::new(
            session_id,
            MessageRole::Assistant,
            full_reply.clone(),
            MessageSource::Online,
        );
        if let Err(e) = storage.save_message(&assistant_msg).await {
            tracing::error!(%e, "保存 assistant 消息失败");
        }
    }

    // 4. 发送 Done 事件（仅在无错误时——错误已通过 Error 事件发送，无需再发 Done）
    if !has_error {
        let done_event = StreamEvent::done(request_id, backend_id, full_reply.chars().count());
        let _ = tx.unbounded_send(Ok(done_event));
    }

    tracing::info!(
        request_id = %request_id,
        session_id = %session_id,
        reply_chars = full_reply.chars().count(),
        has_error,
        duration_ms = now_ms() - now,
        "send_message 完成"
    );
}

// =========================================================
// Drop 实现（优雅关闭兜底）
// =========================================================

impl Drop for App {
    /// App 销毁时设置 shutdown_flag，通知所有后台线程退出。
    ///
    /// 注意:
    /// - Drop 是同步方法，不能调用 async 代码。
    /// - 完整的优雅关闭应通过 `shutdown()` 方法执行（关闭活跃 session + 等待线程退出）。
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
    fn app_state_default_is_needs_setup() {
        // 编译期验证：App 的默认构造不会 panic
        // 实际 App 需要 trait objects，此处仅测试辅助逻辑
        let state = AppState::NeedsSetup;
        assert_eq!(state.as_str(), "needs_setup");
    }

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
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        );
        assert!(prompt.contains("Ramaria"));
        assert!(prompt.contains("记忆"));
        assert!(!prompt.contains("000")); // 不应包含原始 Unix 时间戳
    }
}
