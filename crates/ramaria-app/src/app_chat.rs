//! rust/crates/ramaria-app/src/app_chat.rs - 核心对话管线
//!
//! 设计特点:
//! - 从 `app.rs` 提取的 `send_message` 完整对话管线
//! - 包含记忆检索、System Prompt 装配、Token Budget、LLM 流式调用、消息保存
//! - 自由函数: `load_persona_toml_prompt`（冷启动兜底）、`stream_forward_task`（流式转发）
//! - 降级策略: 嵌入模型不可用 → 仅 BM25+图谱检索；persona.toml 缺失 → 默认 Ramaria prompt
//! - 安全约束: 不记录完整 prompt 或用户消息；线上 LLM 调用前强制隐私确认
//! - 所有方法通过 `impl App` 关联，访问 `pub(crate)` 字段

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatMessage, ChatRequest, StorageBackend};
use ramaria_core::types::{
    AppState, Message, MessageRole, MessageSource, PersonaKind, ProfileField, new_id, now_ms,
};
use ramaria_memory::decay::{DecayConfig, calc_decay_r};
use ramaria_memory::parse_persona_toml;
use ramaria_memory::prompt::builder::{PromptConfig, PromptContext, assemble_prompt};
use ramaria_memory::rag::{RagConfig, filter_by_persona, format_context_text};
use ramaria_memory::retriever::SearchRequest;
use ramaria_memory::token_budget::{self, TokenBudgetConfig};
use uuid::Uuid;

use crate::App;
use crate::privacy;
use crate::stream_event::StreamEvent;

// =========================================================
// send_message: 核心对话管线
// =========================================================

impl App {
    /// 发送消息并获取流式回复。
    ///
    /// 完整管线:
    /// 1. 检查应用状态（必须为 Ready 或 Degraded）
    /// 2. 检查隐私确认（线上 provider）
    /// 3. 获取或创建会话
    /// 4. 加载对话历史
    /// 5. 搜索记忆上下文（Persona-Aware RAG）
    /// 6. 构建 System Prompt（5-Block 装配器）
    /// 7. Token 预算管理
    /// 8. 构建 ChatRequest
    /// 9. 调用 LLM provider.chat_stream
    /// 10. 后台任务收集完整回复 + 保存消息 + 转发 StreamEvent
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
    ) -> RamariaResult<crate::app::SendMessageStream> {
        let request_id = new_id();

        // ---- Step 1: 状态检查 ----
        {
            let state = self.current_state();
            match state {
                AppState::Ready => { /* 正常，继续 */ }
                AppState::Degraded => {
                    // Degraded 允许对话（仅向量通道不可用，BM25+图谱仍工作）
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
        // 自动创建 session + 只读约束
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

                // 修复：无论前端传入还是后端创建，都同步追踪活跃 session
                // 否则 save_and_close_session 找不到活跃 session → 返回 "无活跃对话"
                self.lifecycle.set_active_session_id_public(Some(s.id));

                s
            }
            None => {
                // 自动创建新 session
                // 对齐 Python `on_message`: 无活跃 session 时自动创建
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

        // ---- Step 4.5: 预加载近期 L1 摘要（跨 session 上下文注入） ----
        // 解决新 session 发"你好"时 LLM 完全不知道上次聊了什么的问题。
        // 不依赖关键词匹配——近期摘要无条件注入 System Prompt Block C1。
        let actual_uid = persona_uid.unwrap_or("rama-0001");
        let recent_l1 = self
            .storage
            .list_recent_l1_by_persona(actual_uid, 3)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    persona_uid = actual_uid,
                    %e,
                    "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                );
                Vec::new()
            });

        // 格式化近期摘要为可读文本
        let recent_summaries: Vec<String> =
            recent_l1.iter().map(format_l1_as_context_line).collect();

        // 获取最后活跃时间（取最近一条 L1 的创建时间）
        let last_active_at: Option<String> = recent_l1.first().map(|l1| {
            // Unix 毫秒 → 可读日期时间
            let secs = l1.created_at / 1000;
            match chrono::DateTime::from_timestamp(secs, 0) {
                Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
                None => String::new(),
            }
        });

        // ---- Step 5: 记忆检索 + RAG ----
        let memory_context = self
            .search_and_assemble_context(user_input, persona_uid)
            .await;

        // ---- Step 6: 构建 System Prompt（5-Block 装配器） ----
        let system_prompt = self
            .build_system_prompt_with_context(
                persona_uid,
                &recent_summaries,
                last_active_at.as_deref(),
            )
            .await;

        // ---- Step 6.5: Token 预算管理 ----
        // 按 provider 的 context_window 限制上下文大小，在句子边界截断
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
    /// - : 尝试使用嵌入模型生成 query 向量；
    ///   若嵌入模型不可用（未配置或加载失败），
    ///   向量通道自动降级为权重 0，BM25 + 图谱仍正常工作。
    /// - v1.2: 检索结果应用 Ebbinghaus 时间衰减，
    ///   使近期记忆的排序优于同样相关但更旧的记忆。
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
        let mut results = {
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

        // ---- 时间衰减：rrf_score × Ebbinghaus decay ----
        // 根据记忆层级选择衰减参数：L2 事件比 L1 摘要衰减更慢。
        let now = now_ms();
        let decay_config_l1 = DecayConfig::l1(); // S=30
        let decay_config_l2 = DecayConfig::l2(); // S=60

        for r in &mut results {
            let decay_config = if r.layer == "l2" {
                &decay_config_l2
            } else {
                &decay_config_l1
            };

            // salience: SearchResult 当前不携带此字段（后续可从 L1DocView/L2DocView 回填）。
            // 使用中性值 0.5，确保时间衰减生效但不过度偏向。
            let salience = 0.5;

            let decay_factor = calc_decay_r(r.created_at, now, salience, decay_config);
            r.rrf_score *= decay_factor;

            tracing::trace!(
                doc_id = %r.doc_id,
                layer = %r.layer,
                rrf_original = r.rrf_score / decay_factor,
                decay_factor = format!("{:.4}", decay_factor),
                rrf_adjusted = format!("{:.4}", r.rrf_score),
                "时间衰减已应用"
            );
        }

        // 重新按衰减后的 rrf_score 排序
        results.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

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
            "记忆上下文已组装（含时间衰减）"
        );

        Some(context)
    }

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
// 辅助: 格式化 L1 摘要为上下文行
// =========================================================

/// 将 MemoryL1 格式化为一行上下文文本，供 Block C1 [近期对话摘要] 使用。
///
/// 格式:
/// - 含时间段: "上午 — 讨论了Python异步编程的线程安全问题。氛围融洽。"
/// - 无时间段: "讨论了Python异步编程的线程安全问题。氛围融洽。"
///
/// 截断规则:
/// - 单条摘要最多 120 字符（与 RAG 格式化一致），超出加省略号。
///
/// 安全约束:
/// - 不在此处记录完整摘要到 INFO 日志（仅 DEBUG 级别可记录）。
fn format_l1_as_context_line(l1: &ramaria_core::types::MemoryL1) -> String {
    let time_label = l1.time_period.as_deref().unwrap_or("");

    let atmosphere = l1.atmosphere.as_deref().unwrap_or("");

    let base = if !time_label.is_empty() && !atmosphere.is_empty() {
        format!("{time_label} — {}。氛围{atmosphere}。", l1.summary)
    } else if !time_label.is_empty() {
        format!("{time_label} — {}", l1.summary)
    } else if !atmosphere.is_empty() {
        format!("{}。氛围{atmosphere}。", l1.summary)
    } else {
        l1.summary.clone()
    };

    // 截断到 120 字符
    if base.chars().count() > 120 {
        let truncated: String = base.chars().take(120).collect();
        truncated + "…"
    } else {
        base
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

    // 1. 保存用户消息（用户发言不关联 persona——发言人是用户自己）
    let user_msg = Message::new(
        session_id,
        MessageRole::User,
        user_message,
        MessageSource::Local,
    );
    // 用户消息不设 persona_uid（用户即自己），前端据此将气泡渲染在右侧
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
