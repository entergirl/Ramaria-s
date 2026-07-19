//! rust/crates/ramaria-app/src/app_chat.rs - 核心对话管线
//!
//! 设计特点:
//! - `send_message` Steps 1-5 委托 `SendMessagePipeline` + 5 个独立 Stage 执行
//! - Steps 6-10（System Prompt、Token Budget、LLM 调用、消息保存）保留为本文件逻辑
//! - 自由函数: `load_persona_toml_prompt`（冷启动兜底）、`stream_forward_task`（流式转发）
//! - 降级策略: 嵌入模型不可用 → 仅 BM25+图谱检索；persona.toml 缺失 → 默认 Ramaria prompt
//! - 安全约束: 不记录完整 prompt 或用户消息；线上 LLM 调用前强制隐私确认
//! - 向后兼容：`send_message` 对外接口（参数/返回值）完全不变

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, StorageBackend};
use ramaria_core::types::{Message, MessageRole, MessageSource, ProfileField, new_id, now_ms};
use ramaria_memory::SHARED_CHAT_STYLE_RULES;
use ramaria_memory::parse_persona_toml;
use ramaria_memory::prompt::builder::{PromptConfig, PromptContext, assemble_prompt};
use ramaria_memory::token_budget::{self, TokenBudgetConfig};
use uuid::Uuid;

use crate::App;
use crate::pipeline::{PipelineData, SendMessagePipeline};
use crate::stages::{
    StageCheckPrivacy, StageCheckState, StageLoadHistory, StageResolveSession, StageRetrieveMemory,
};
use crate::stream_event::StreamEvent;

// =========================================================
// send_message: 核心对话管线（v1.2 Pipeline 重构版）
// =========================================================

impl App {
    /// 发送消息并获取流式回复。
    ///
    /// 完整管线（v1.2 Pipeline + Stage 模式）:
    /// Steps 1-5 → `SendMessagePipeline` 编排 5 个独立 Stage
    /// Steps 6-10 → 本方法继续执行（M2 将拆分为 Stage 6-10）
    ///
    /// 参数:
    /// - `user_input`: 用户输入文本。
    /// - `persona_uid`: 可选的人格标识（None 表示 rama 自身）。
    /// - `session_id`: 可选的会话 ID（None 表示创建新会话）。
    ///
    /// 返回:
    /// - 成功时返回 `SendMessageStream`（StreamEvent 异步流）。
    /// - 失败时返回错误（状态不对、隐私未确认、会话已关闭等）。
    pub async fn send_message(
        &self,
        user_input: &str,
        persona_uid: Option<&str>,
        session_id: Option<Uuid>,
    ) -> RamariaResult<crate::app::SendMessageStream> {
        let request_id = new_id();

        // ---- 构建 PipelineContext + PipelineData ----
        let ctx = self.build_pipeline_context();
        let data = PipelineData::new(
            user_input.to_string(),
            persona_uid.map(|s| s.to_string()),
            session_id,
            request_id,
        )
        .with_app_state(self.current_state());

        // ---- Steps 1-5: 委托 Pipeline 编排器 ----
        let pipeline = SendMessagePipeline::new(vec![
            Box::new(StageCheckState::new()),
            Box::new(StageCheckPrivacy::new()),
            Box::new(StageResolveSession::new()),
            Box::new(StageLoadHistory::new()),
            Box::new(StageRetrieveMemory::new()),
        ]);

        let result = pipeline
            .execute(&ctx, data)
            .await
            .map_err(ramaria_core::error::RamariaError::from)?;

        // ---- 从 PipelineData 提取 Stage 1-5 产出 ----
        let session = result
            .session
            .expect("Stage 3 (ResolveSession) must set session");
        let history_messages = result.history_messages;
        let recent_summaries = result.recent_summaries;
        let last_active_at = result.last_active_at;
        let memory_context = result.memory_context;
        let cfg = result
            .backend_config
            .expect("Stage 2 (CheckPrivacy) must set backend_config");

        // ---- Step 6: 构建 System Prompt（5-Block 装配器） ----
        let system_prompt = self
            .build_system_prompt_with_context(
                persona_uid,
                &recent_summaries,
                last_active_at.as_deref(),
            )
            .await;

        // ---- Step 6.5: Token 预算管理 ----
        let context_window = cfg.capability.context_window as usize;
        let budget_config = TokenBudgetConfig::new(context_window, cfg.max_tokens);
        let budgeted = token_budget::apply_token_budget(
            &system_prompt,
            memory_context.as_deref(),
            &history_messages,
            user_input,
            &budget_config,
        );

        if budgeted.estimated_tokens > context_window {
            tracing::warn!(
                request_id = %request_id,
                estimated = budgeted.estimated_tokens,
                window = context_window,
                "token 预算超出上下文窗口，可能发生截断"
            );
        }
        tracing::debug!(
            request_id = %request_id,
            estimated_tokens = budgeted.estimated_tokens,
            context_window = context_window,
            history_kept = budgeted.history.len(),
            history_original = history_messages.len(),
            "token 预算已应用"
        );

        // ---- Step 7: 构建 ChatRequest ----
        let chat_request = ChatRequest {
            system_prompt: budgeted.system_prompt,
            memory_context: budgeted.memory_context,
            history: budgeted.history,
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
        // ★ 先 clone Arc 出锁再 await，避免 MutexGuard 跨 .await
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
        let persona_for_save = persona_uid.map(|s| s.to_string());

        let (tx, rx) = mpsc::unbounded::<RamariaResult<StreamEvent>>();

        tokio::spawn(async move {
            stream_forward_task(
                storage,
                raw_stream,
                tx,
                session_id,
                user_msg,
                input_request_id,
                persona_for_save,
            )
            .await;
        });

        Ok(Box::pin(rx))
    }

    // =========================================================
    // Pipeline 上下文构建
    // =========================================================

    /// 从 App 运行时依赖构建 PipelineContext。
    ///
    /// 注意:
    /// - 所有字段通过 Arc 克隆共享，零所有权拷贝
    /// - LLM 和 Embedding 从 Mutex 中 clone Arc 出锁后传入
    /// - Retriever 通过 Arc 引用共享（v1.3 P-3 已改为 Arc<RwLock<Retriever>>）
    fn build_pipeline_context(&self) -> crate::pipeline::PipelineContext {
        let llm = self.llm_clone();
        let embedding = self.embedding_provider();
        let storage = Arc::clone(&self.storage);
        let config = self.config.clone();
        let retriever = Arc::clone(&self.retriever);
        let keychain = Arc::clone(&self.keychain);
        let lifecycle = Arc::clone(&self.lifecycle);

        crate::pipeline::PipelineContext::new(
            storage, llm, embedding, config, retriever, keychain, lifecycle,
        )
    }

    // =========================================================
    // 内部辅助方法
    // =========================================================

    /// 构建 System Prompt（使用 5-Block 装配器，含跨 session 上下文）。
    ///
    /// 流程:
    /// 1. 从 storage 加载当前 persona 的数据（persona/facts/traits/examples）。
    /// 2. 注入近期 L1 摘要（跨 session 上下文）和最后活跃时间。
    /// 3. 调用 `assemble_prompt` 组装 5-Block System Prompt。
    /// 4. 无 persona 数据时降级为基础 Ramaria 默认 prompt。
    ///
    /// 参数:
    /// - `persona_uid`: 人格标识。
    /// - `recent_summaries`: 近期 L1 摘要列表（预格式化文本）。
    /// - `last_active_at`: 最后活跃时间字符串（YYYY-MM-DD HH:MM 格式）。
    ///
    /// 降级策略:
    /// - storage 读取失败 → 记录 warn 日志，使用空数据继续。
    /// - persona 不存在 → 使用默认 Ramaria 身份 prompt。
    /// - facts/traits/examples 为空 → 对应 Block 自动省略（由 builder 处理）。
    /// - recent_summaries 为空 → Block C1 显示"首次对话"提示。
    ///
    /// 安全约束:
    /// - 不在此处写入 system prompt 到日志（完整 prompt 仅发送到 LLM）。
    async fn build_system_prompt_with_context(
        &self,
        persona_uid: Option<&str>,
        recent_summaries: &[String],
        last_active_at: Option<&str>,
    ) -> String {
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
                // 跨 session 上下文: 近期 L1 摘要 + 最后活跃时间
                recent_session_summaries: recent_summaries.to_vec(),
                last_active_at: last_active_at.map(|s| s.to_string()),
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
        .filter(|s| !s.trim().is_empty())
        // v1.3 T4: 无自定义 E_rules 时使用共享社交平台口吻
        .unwrap_or(SHARED_CHAT_STYLE_RULES);

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
/// - 新路径为目录扫描模式，每文件 = 一个 persona。
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
    persona_uid: Option<String>,
) {
    use futures::StreamExt;

    futures::pin_mut!(raw_stream);

    let mut full_reply = String::new();
    let mut backend_id: Option<String> = None;
    let mut has_error = false;
    let now = now_ms();

    // 1. 保存用户消息
    //    v1.2: 用户消息现在也携带 persona_uid，表示"在此 persona 的对话上下文中"
    let user_msg = Message::new(
        session_id,
        MessageRole::User,
        user_message,
        MessageSource::Local,
    )
    .with_persona_uid(persona_uid.clone());
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
    // 助手消息携带 persona_uid，用于前端在左侧气泡显示"谁在回复"
    if !has_error && !full_reply.is_empty() {
        let assistant_msg = Message::new(
            session_id,
            MessageRole::Assistant,
            full_reply.clone(),
            MessageSource::Online,
        )
        .with_persona_uid(persona_uid.clone());
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
