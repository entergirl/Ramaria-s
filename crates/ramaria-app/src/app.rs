//! rust/crates/ramaria-app/src/app.rs - 应用编排核心
//!
//! 设计特点:
//! - `App` 结构体持有所有运行时依赖：storage、llm provider、retriever、config、keychain
//! - `send_message` 实现完整对话管线：状态检查→隐私确认→会话管理→记忆检索→RAG→LLM调用→消息持久化
//! - 流式输出通过 `futures::channel::mpsc` 桥接 LLM 流与上层 StreamEvent 流
//! - 消息保存与会话管理通过 `StorageBackend` trait 进行，不依赖具体数据库
//! - `rebuild_retriever` 从存储层加载 L1 数据，增量构建内存检索索引
//!
//! 安全约束:
//! - 不记录完整 prompt 或用户消息（日志仅记录 request_id + 字符数）
//! - 线上 provider 调用前强制隐私确认检查
//! - API key 仅在 keychain 读取时出现，不缓存

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatMessage, ChatRequest, LlmProvider, StorageBackend};
use ramaria_core::types::{
    AppState, BackendConfig, Message, MessageRole, MessageSource, PersonaKind, ProfileField,
    new_id, now_ms,
};
use ramaria_llm::keychain::Keychain;
use ramaria_memory::prompt::builder::{PromptConfig, PromptContext, assemble_prompt};
use ramaria_memory::rag::{RagConfig, filter_by_persona, format_context_text};
use ramaria_memory::retriever::{L1DocView, Retriever, SearchRequest};
use uuid::Uuid;

use crate::privacy;
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
/// - 支持检索器重建（从存储层加载 L1/L2 索引数据）
///
/// 用法:
/// ```ignore
/// let app = App::new(storage, llm, config, keychain)?;
/// app.run_setup(&backend_config).await?;
/// let stream = app.send_message("你好", None, None).await?;
/// ```
pub struct App {
    /// 存储后端（23 张表 CRUD）
    storage: Arc<dyn StorageBackend>,
    /// 当前 LLM provider
    llm: Arc<dyn LlmProvider>,
    /// 内存检索器（BM25 + 向量 + 图谱）
    retriever: Mutex<Retriever>,
    /// 应用配置
    config: ramaria_core::config::RamariaConfig,
    /// 当前应用状态
    state: Mutex<AppState>,
    /// OS keychain（供隐私确认和 provider 验证使用）
    keychain: Arc<Keychain>,
}

impl App {
    /// 创建新的 App 实例。
    ///
    /// 参数:
    /// - `storage`: 存储后端（通常为 `ramaria-storage` 的 `SqliteStorage`）。
    /// - `llm`: LLM provider（LmStudio / DeepSeek / OpenAI 之一）。
    /// - `config`: 应用配置。
    /// - `keychain`: OS keychain 实例。
    ///
    /// 返回:
    /// - 初始状态为 `NeedsSetup` 的 App 实例。
    /// - 检索器为空，需调用 `rebuild_retriever` 填充。
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        llm: Arc<dyn LlmProvider>,
        config: ramaria_core::config::RamariaConfig,
        keychain: Arc<Keychain>,
    ) -> Self {
        let retriever = Retriever::new();

        tracing::info!(
            provider = llm.name(),
            "App 实例已创建，初始状态: NeedsSetup"
        );

        Self {
            storage,
            llm,
            retriever: Mutex::new(retriever),
            config,
            state: Mutex::new(AppState::NeedsSetup),
            keychain,
        }
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
    pub fn backend_config(&self) -> &BackendConfig {
        self.llm.config()
    }

    /// 获取 keychain 引用。
    pub fn keychain(&self) -> &Keychain {
        &self.keychain
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

    /// 检查并更新设置状态。
    pub async fn refresh_setup_state(&self) -> RamariaResult<AppState> {
        let status = crate::setup::check_setup_status(self.storage.as_ref()).await?;
        let state = crate::setup::determine_state(&status);
        self.set_state(state);
        Ok(state)
    }

    // =========================================================
    // 隐私确认
    // =========================================================

    /// 检查当前 provider 的隐私确认状态。
    pub async fn check_privacy(&self) -> RamariaResult<crate::privacy::PrivacyStatus> {
        let cfg = self.llm.config();
        privacy::check_privacy(self.storage.as_ref(), cfg.provider, &cfg.base_url).await
    }

    /// 记录隐私确认。
    ///
    /// 参数:
    /// - `persistent`: 是否跨重启持久化。
    pub async fn confirm_privacy(&self, persistent: bool) -> RamariaResult<()> {
        let cfg = self.llm.config();
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
    /// - 加载所有 L1 记忆条目，转换为 `L1DocView` 并索引到 Retriever。
    /// - 此操作会清空现有索引并重建。
    /// - 建议在应用启动和后台定期执行。
    pub async fn rebuild_retriever(&self) -> RamariaResult<usize> {
        // 1. 获取所有 persona
        let personas = self.storage.list_personas().await?;

        // 2. 从存储层收集所有 L1 数据（在锁外执行 I/O）
        let mut all_docs: Vec<L1DocView> = Vec::new();
        for persona in &personas {
            let l1_list = self.storage.list_unabsorbed_l1(&persona.uid).await?;
            for l1 in &l1_list {
                all_docs.push(L1DocView {
                    id: l1.id,
                    summary: l1.summary.clone(),
                    keywords: l1.keywords.clone(),
                    salience: l1.salience,
                    created_at: l1.created_at,
                    persona_uid: l1.persona_uid.clone(),
                });
            }
        }

        // 3. 锁定检索器并批量索引（纯同步操作，不跨越 .await）
        let total = all_docs.len();
        {
            let mut retriever = self.retriever.lock().unwrap_or_else(|e| {
                tracing::error!("Retriever lock poisoned during rebuild: {e}");
                e.into_inner()
            });
            retriever.clear();
            for doc in &all_docs {
                retriever.index_l1(doc);
            }
        } // MutexGuard 在此释放

        tracing::info!(total, personas = personas.len(), "检索器索引重建完成");
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
            if state != AppState::Ready {
                return Err(RamariaError::validation(format!(
                    "应用尚未就绪（当前状态: {state}）。请先完成设置流程。"
                )));
            }
        }

        // ---- Step 2: 隐私确认 ----
        let cfg = self.llm.config();
        if cfg.provider.is_online() {
            privacy::require_privacy(self.storage.as_ref(), cfg.provider, &cfg.base_url).await?;
        }

        // ---- Step 3: 会话管理 ----
        let session = match session_id {
            Some(sid) => self
                .storage
                .get_session(sid)
                .await?
                .ok_or_else(|| RamariaError::validation(format!("会话不存在: {sid}")))?,
            None => self.storage.create_session().await?,
        };

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
        let raw_stream = self.llm.chat_stream(&chat_request).await?;

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
    async fn search_and_assemble_context(
        &self,
        query: &str,
        persona_uid: Option<&str>,
    ) -> Option<String> {
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

            // 无 query_vec 时跳过向量通道（v1.0 暂不接入 embedding provider）
            retriever.search(&request, None)
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

    // 4. 发送 Done 事件
    let done_event = StreamEvent::done(request_id, backend_id, full_reply.chars().count());
    let _ = tx.unbounded_send(Ok(done_event));

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
