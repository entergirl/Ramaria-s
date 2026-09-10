//! crates/ramaria-app/src/app_chat.rs - 核心对话管线
//!
//! 设计特点:
//! - `send_message` Steps 1-5 委托 `SendMessagePipeline` + 5 个独立 Stage 执行
//! - Steps 6-10（System Prompt、Token Budget、LLM 调用、消息保存）保留为本文件逻辑
//! - 注入协调预算（`[injection_budget]`，默认关闭）：开启时 Step 6 走
//!   `build_system_prompt_coordinated`（RAG 基座 + 四层在统一池内按可配顺序分配），
//!   关闭时走既有 `build_system_prompt_with_context`（行为逐字段等价 v1.7）
//! - 自由函数: `load_persona_toml_prompt`（冷启动兜底）、`stream_forward_task`（流式转发）
//! - 降级策略: 嵌入模型不可用 → 仅 BM25+图谱检索；persona.toml 缺失 → 默认 Ramaria prompt
//! - 安全约束: 不记录完整 prompt 或用户消息；线上 LLM 调用前强制隐私确认
//! - 向后兼容：`send_message` 对外接口（参数/返回值）完全不变

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatMessage, ChatRequest, StorageBackend};
use ramaria_core::types::{Message, MessageRole, MessageSource, ProfileField, new_id, now_ms};
use ramaria_memory::SHARED_CHAT_STYLE_RULES;
use ramaria_memory::parse_persona_toml;
use ramaria_memory::prompt::builder::{
    PromptConfig, PromptContext, assemble_prompt, assemble_prompt_coordinated,
};
use ramaria_memory::token_budget::{self, TokenBudgetConfig};
use uuid::Uuid;

use crate::App;
use crate::pipeline::{PipelineData, SendMessagePipeline};
use crate::stages::{
    StageCheckPrivacy, StageCheckState, StageLoadHistory, StageResolveSession, StageRetrieveMemory,
};
use crate::stream_event::StreamEvent;

// =========================================================
// send_message: 核心对话管线（Pipeline 重构版）
// =========================================================

impl App {
    /// 发送消息并获取流式回复。
    ///
    /// 完整管线（Pipeline + Stage 模式）:
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
        // 默认按 App 当前配置执行（对外接口与 v1.4 完全一致，向后兼容）
        self.send_message_with_config(user_input, persona_uid, session_id, &self.config)
            .await
    }

    /// 以指定配置发送消息（探针档位实验等需要覆盖运行时配置的场景）。
    ///
    /// 与 `send_message` 的唯一区别：本方法允许调用方提供完整的 `RamariaConfig`，
    /// 对话管线（检索、examples 预选、prompt 装配、token 预算）全部按该配置执行；
    /// 探针档位对比（θ_gap / 条数上限 / top_k）通过覆盖 `config.utt` 生效。
    ///
    /// 用法:
    /// - 普通调用: 传 `&self.config`（效果与 `send_message` 完全一致）。
    /// - 档位实验: 克隆当前配置后修改目标字段再传入（`probe run` 场景）。
    ///
    /// 说明:
    /// - 不修改 App 内部状态：仅本次调用按传入配置执行，进程内其他调用不受影响。
    /// - 状态检查 / 隐私确认 / 会话解析等 Stage 行为与 `send_message` 一致。
    pub async fn send_message_with_config(
        &self,
        user_input: &str,
        persona_uid: Option<&str>,
        session_id: Option<Uuid>,
        config: &ramaria_core::config::RamariaConfig,
    ) -> RamariaResult<crate::app::SendMessageStream> {
        // 普通对话路径：无预置上文，委托共用实现
        self.send_message_inner(user_input, persona_uid, session_id, config, Vec::new())
            .await
    }

    /// 同 `send_message_with_config`，但额外预置一段上文历史（时间正序）。
    ///
    /// 说明:
    /// - `seed_history` 不落库、不参与本 session 的消息持久化，仅进入本轮 prompt 的历史段
    ///   （与 DB 加载的 session 历史拼接，seed 在前）。
    /// - 传空 Vec 时与 `send_message_with_config` 完全等价。
    ///
    /// 参数:
    /// - 前四个参数同 `send_message_with_config`。
    /// - `seed_history`: 调用方预置的上文（时间正序，早于本 session 历史）。
    ///
    /// 返回:
    /// - 与 `send_message_with_config` 相同的 `SendMessageStream`。
    pub async fn send_message_with_history(
        &self,
        user_input: &str,
        persona_uid: Option<&str>,
        session_id: Option<Uuid>,
        config: &ramaria_core::config::RamariaConfig,
        seed_history: Vec<ChatMessage>,
    ) -> RamariaResult<crate::app::SendMessageStream> {
        self.send_message_inner(user_input, persona_uid, session_id, config, seed_history)
            .await
    }

    /// `send_message_with_config` / `send_message_with_history` 共用的管线实现。
    ///
    /// 参数:
    /// - 前四个参数同 `send_message_with_config`。
    /// - `seed_history`: 调用方预置的上文（时间正序）；空 Vec 即普通对话路径。
    ///
    /// 返回:
    /// - 与 `send_message_with_config` 相同的 `SendMessageStream`。
    async fn send_message_inner(
        &self,
        user_input: &str,
        persona_uid: Option<&str>,
        session_id: Option<Uuid>,
        config: &ramaria_core::config::RamariaConfig,
        seed_history: Vec<ChatMessage>,
    ) -> RamariaResult<crate::app::SendMessageStream> {
        let request_id = new_id();

        // ---- 构建 PipelineContext + PipelineData（按传入配置执行） ----
        let ctx = self.build_pipeline_context(config);
        let pipeline_data = PipelineData::new(
            user_input.to_string(),
            persona_uid.map(|s| s.to_string()),
            session_id,
            request_id,
        )
        .with_app_state(self.current_state())
        .with_seed_history(seed_history);

        // ---- Steps 1-5: 委托 Pipeline 编排器 ----
        let pipeline = SendMessagePipeline::new(vec![
            Box::new(StageCheckState::new()),
            Box::new(StageCheckPrivacy::new()),
            Box::new(StageResolveSession::new()),
            Box::new(StageLoadHistory::new()),
            Box::new(StageRetrieveMemory::new()),
        ]);

        let result = pipeline
            .execute(&ctx, pipeline_data)
            .await
            .map_err(ramaria_core::error::RamariaError::from)?;

        // ---- 从 PipelineData 提取 Stage 1-5 产出 ----
        let session = result
            .session
            .expect("Stage 3 (ResolveSession) must set session");
        let history_messages = result.history_messages;

        // ---- Step 5.4: 弱反馈信号检测（H2，v1.7） ----
        // S2 纠正 / S3 继续：检测"上一条助手回复 → 当前用户消息"的间隔与前缀。
        // 当前用户消息尚未落库（在 stream_forward_task 中保存），此处构造检测序列：
        // 把当前用户消息（当前时间戳）追加到已加载的会话消息末尾，作为检测输入的
        // 最后一条；检测后不落库（仅用于信号判定），消息本体仍由后续管线保存。
        // 静默降级：检测/写入失败记 warn 不阻塞对话；[feedback].enabled=false 跳过。
        // 仅活跃 session 检测（超时封存不计入）；30s 去重由内部排除项处理。
        if config.feedback.enabled {
            let mut recent = match self.storage.list_messages(session.id).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(%e, "读取会话消息用于弱反馈检测失败，跳过");
                    Vec::new()
                }
            };
            // 追加当前用户消息作为检测输入的最后一条（时间戳取当前，供间隔判定）
            recent.push(Message::new(
                session.id,
                MessageRole::User,
                user_input.to_string(),
                MessageSource::Local,
            ));
            if let Err(e) = crate::feedback::process_feedback_for_new_message(
                self.storage.as_ref(),
                &config.feedback,
                session.id,
                persona_uid,
                &recent,
            )
            .await
            {
                tracing::warn!(%e, "弱反馈信号处理失败（不阻塞对话主流程）");
            }
        }
        let recent_summaries = result.recent_summaries;
        let last_active_at = result.last_active_at;
        // RAG 相关记忆闸门（探针消融 B0 等关闭）：关闭时置空，
        // ChatRequest 不携带 `<memory_context>`，但 RAG 检索 stage 仍执行
        // （若 `injection.memory_rag=false` 时 stage 已跳过检索，此处恒 None）。
        // `mut`：协调预算开启时 RAG 摘要可能被裁剪/丢弃，需回写最终结果。
        let mut memory_context = if config.injection.memory_rag {
            result.memory_context
        } else {
            None
        };
        // RAG 实际注入的文档覆盖集合：仅当记忆闸门开启且 memory_context 真正注入时
        // 才消费 stage 记录的文档 label。消融档（memory_rag=false）下覆盖集合为空，
        // 知识卡片不被 RAG 去重误伤，保持"关掉 RAG 后断言层仍兜底"的消融语义。
        let rag_covered_labels = if memory_context.is_some() {
            result.memory_doc_labels
        } else {
            Vec::new()
        };
        let utt_context = result.utt_context;
        let bridge_context = result.bridge_context;
        let cfg = result
            .backend_config
            .expect("Stage 2 (CheckPrivacy) must set backend_config");

        // ---- Step 5.5: 行为层情境路由 ----
        // [behavior].enabled=false / 注入闸门关闭 / 未命中 / 路由失败 → None（静默降级，
        // prompt 不含行为块）；命中 → 合并主/次规则注入行为块。
        let behavior_decision = if config.injection.behavior && config.behavior.enabled {
            // history_messages 为 ChatMessage（role+content），行为路由仅消费
            // role/content（查询构造），转换为轻量 Message 列表
            let route_messages: Vec<Message> = history_messages
                .iter()
                .map(|m| Message::new(session.id, m.role, m.content.clone(), MessageSource::Local))
                .collect();
            match crate::commands::behavior::behavior_route(
                self,
                persona_uid.unwrap_or("rama-0001"),
                &route_messages,
            )
            .await
            {
                Ok(r) if r.matched => r
                    .primary
                    .as_ref()
                    .map(|p| ramaria_memory::behavior::merge_route_targets(p, &r.secondary)),
                // 未命中 → 静默降级（等同 v1.4）
                Ok(_) => None,
                // 路由失败（存储/查询异常）→ 记 warn 降级，不阻塞主流程
                Err(e) => {
                    tracing::warn!(
                        %e,
                        request_id = %request_id,
                        "行为情境路由失败，静默降级不注入行为块"
                    );
                    None
                }
            }
        } else {
            None
        };

        // ---- Step 5.6: 知识层判定器检索 ----
        // [knowledge].auto_fact_detect=false / 注入闸门关闭 / 判定器未命中 / 检索失败
        // → 空 facts，prompt 不含知识块（静默降级）。
        // 数据读取与门控判定同源：均按本次生效配置（`config`，档位覆盖场景下
        // 与 `self.config` 可不同）传入，保证 send_message_with_config 的
        // "knowledge 检索参数/总开关按该配置执行"契约成立。
        let knowledge_facts = if config.injection.knowledge && config.knowledge.auto_fact_detect {
            crate::app_knowledge::load_knowledge_facts(
                self.storage.as_ref(),
                config.knowledge.clone(),
                persona_uid.unwrap_or("rama-0001"),
                user_input,
            )
            .await
        } else {
            Vec::new()
        };

        // ---- Step 6: 构建 System Prompt（5-Block 装配器） ----
        // examples 预选（v1.4）：评分轮换 + 记忆未命中兜底；enabled=false 回退 v1.3 静态注入；
        // 注入闸门关闭（表达层消融）时不加载示例（空 → prompt 不含 ## 对话示例）。
        let examples = if config.injection.examples {
            load_examples_for_input(
                self.storage.as_ref(),
                &config.examples,
                persona_uid,
                user_input,
                memory_context.is_some(),
            )
            .await
        } else {
            Vec::new()
        };
        // 注入协调预算开关（`[injection_budget]`，默认关闭 → 走既有装配路径）。
        let coordinated_budget = &config.injection_budget;
        let system_prompt = if coordinated_budget.enabled {
            // 协调路径：RAG 基座 + 四层注入在统一 token 池内按可配顺序分配，
            // 替代"整条 system_prompt 事后无差别截断 + RAG 独立成摊"。
            let built = self
                .build_system_prompt_coordinated(
                    persona_uid,
                    &recent_summaries,
                    last_active_at.as_deref(),
                    utt_context.as_deref(),
                    bridge_context.as_deref(),
                    behavior_decision,
                    examples,
                    config.examples.max_examples as usize,
                    knowledge_facts,
                    &rag_covered_labels,
                    Some(config.knowledge.injection_budget_chars),
                    &config.injection,
                    memory_context.as_deref(),
                    coordinated_budget,
                    &config.layer_dedup,
                )
                .await;
            // 协调可能裁剪/丢弃 RAG：以协调结果回写 memory_context（供 Step 7 使用）
            memory_context = built.memory_context;
            tracing::debug!(
                request_id = %request_id,
                dropped = ?built.dropped,
                fallback_truncated = built.fallback_truncated,
                injected_tokens = built.injected_tokens,
                "注入协调预算已应用（system_prompt 内注入 + RAG）"
            );
            built.system_prompt
        } else {
            self.build_system_prompt_with_context(
                persona_uid,
                &recent_summaries,
                last_active_at.as_deref(),
                utt_context.as_deref(),
                bridge_context.as_deref(),
                behavior_decision,
                examples,
                config.examples.max_examples as usize,
                knowledge_facts,
                // RAG 实际注入的文档覆盖集合（供知识层注入去重；默认空）
                &rag_covered_labels,
                // 知识块渲染预算（core [knowledge].injection_budget_chars，默认 800）
                Some(config.knowledge.injection_budget_chars),
                &config.injection,
                // RAG 实际注入文本（层间仲裁的内容级保留参照；None = RAG 未注入）
                memory_context.as_deref(),
                // 层间证据去重与冲突仲裁配置（默认关闭 = 回退既有引用级去重）
                &config.layer_dedup,
            )
            .await
        };

        // ---- Step 6.5: Token 预算管理 ----
        let context_window = cfg.capability.context_window as usize;
        let mut budget_config = TokenBudgetConfig::new(context_window, cfg.max_tokens);
        if coordinated_budget.enabled {
            // 协调路径已在装配期把 system_prompt 内注入约束到协调池内（固定骨架
            // 稳定且不入池）；此处放开整条句截断，避免对协调结果二次无差别裁剪。
            budget_config.system_prompt_reserve = context_window;
        }
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
            template_version: ramaria_memory::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
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
    /// - Retriever 通过 Arc 引用共享（已改为 Arc<RwLock<Retriever>>）
    fn build_pipeline_context(
        &self,
        config: &ramaria_core::config::RamariaConfig,
    ) -> crate::pipeline::PipelineContext {
        let llm = self.llm_clone();
        let embedding = self.embedding_provider();
        let storage = Arc::clone(&self.storage);
        let config = config.clone();
        let retriever = Arc::clone(&self.retriever);
        let keyword_service = Arc::clone(&self.keyword_service);
        let keychain = Arc::clone(&self.keychain);
        let lifecycle = Arc::clone(&self.lifecycle);

        crate::pipeline::PipelineContext::new(
            storage,
            llm,
            embedding,
            config,
            retriever,
            keyword_service,
            keychain,
            lifecycle,
        )
    }

    // =========================================================
    // 内部辅助方法
    // =========================================================

    /// 加载 System Prompt 装配素材（普通 / 协调装配共享）。
    ///
    /// 流程:
    /// 1. 从 storage 加载当前 persona 的数据（persona/facts/traits/examples）。
    /// 2. 注入近期 L1 摘要（跨 session 上下文）和最后活跃时间。
    /// 3. 返回结构化上下文（`Structured`）或纯文本降级（`Plain`）。
    ///    - 无 persona → `Plain`（默认 Ramaria prompt）。
    ///    - persona 存在但 facts/traits 均为空且 persona.toml 可用 → `Plain`（冷启动）。
    ///    - 其余 → `Structured(ctx, config)`（由调用方选择普通/协调装配）。
    ///
    /// 参数:
    /// - `persona_uid`: 人格标识。
    /// - `recent_summaries`: 近期 L1 摘要列表（预格式化文本）。
    /// - `last_active_at`: 最后活跃时间字符串（YYYY-MM-DD HH:MM 格式）。
    /// - `utt_context`: utt 原文片段（已按预算裁剪渲染；None 表示不注入，等同 v1.3）。
    /// - `bridge_context`: 桥接内容（上一会话尾部原文，已按预算截断；None 表示不注入）。
    /// - `behavior_decision`: 行为层路由合并决策（None = 未命中/关闭，
    ///   不注入行为块，prompt 不含行为层段落）。
    /// - `examples`: 已选好的 Few-shot 示例（由 `load_examples_for_input` 评分轮换/兜底后传入）。
    /// - `max_examples`: examples 注入上限（来自生效配置 `examples.max_examples`，
    ///   v1.5 起由调用方传入以支持配置覆盖的探针场景）。
    /// - `knowledge_facts`: 知识层判定器命中的 active facts（`# 知识（知识层，按需）`
    ///   内容源；装配前经 RAG 覆盖/角色层去重，空集不产生段落）。
    /// - `rag_covered_labels`: RAG 摘要实际注入的文档 label 集合（`L1:{uuid}`/`L2:{id}`）。
    ///   知识层去重消费：同一事实已由 RAG 摘要文本覆盖则不重复注入（兜底语义不失效）。
    ///   空集合 = RAG 未注入/闸门关闭 → 知识卡片不去重（回退既有兜底行为）。
    /// - `knowledge_budget_chars`: 知识块渲染预算（对齐 core `[knowledge].injection_budget_chars`；
    ///   `None` 使用 memory prompt 层默认预算）。
    /// - `rag_text`: RAG 摘要实际注入文本（`memory_context`；`None` = RAG 未注入）。
    ///   层间仲裁开启时作为"保留参照"做内容级判重（RAG 摘要为主）。
    /// - `layer_dedup`: 层间证据去重与冲突仲裁配置（`[layer_dedup]`；默认关闭 =
    ///   回退既有引用级去重，prompt 输出与既有版本逐字段等价）。
    ///
    /// 降级策略:
    /// - storage 读取失败 → 记录 warn 日志，使用空数据继续。
    /// - persona 不存在 → 使用默认 Ramaria 身份 prompt。
    /// - facts/traits/examples 为空 → 对应 Block 自动省略（由 builder 处理）。
    /// - recent_summaries 为空 → Block C1 显示"首次对话"提示。
    /// - behavior_decision=None → 行为块不注入（静默降级，等同 v1.4）。
    ///
    /// 安全约束:
    /// - 不在此处写入 system prompt 到日志（完整 prompt 仅发送到 LLM）。
    // 参数均为装配 5-Block prompt 所需的独立输入，打包成结构体反而降低可读性；
    // 由 `send_message_with_config` 统一传入（v1.5 探针配置覆盖场景）。
    #[allow(clippy::too_many_arguments)]
    async fn load_prompt_material(
        &self,
        persona_uid: Option<&str>,
        recent_summaries: &[String],
        last_active_at: Option<&str>,
        utt_context: Option<&str>,
        bridge_context: Option<&str>,
        behavior_decision: Option<ramaria_memory::behavior::MergedDecision>,
        examples: Vec<ramaria_core::types::PersonaExample>,
        max_examples: usize,
        knowledge_facts: Vec<ramaria_core::types::PersonaFact>,
        rag_covered_labels: &[String],
        knowledge_budget_chars: Option<usize>,
        injection: &ramaria_core::config::InjectionGate,
        rag_text: Option<&str>,
        layer_dedup: &ramaria_core::config::LayerDedupConfig,
    ) -> LoadedPromptMaterial {
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

            // 自动风格规则（表达层 A3）：仅 [style].enabled 且注入闸门开启时加载
            // （探针消融 F3/B0/B1/S_* 关闭表达层时跳过加载）；
            // 数据不足/无显著项 → None（不注入，prompt 与 v1.6 语义等价）
            let style_rule_text = if self.config.style.enabled && injection.speaking_style {
                crate::app_style::load_style_rule(self.storage.as_ref(), &p.uid)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(persona_uid = %p.uid, %e, "加载自动风格规则失败，跳过");
                        None
                    })
            } else {
                None
            };

            // examples 由调用方（send_message）预选后传入：
            // v1.4 起注入侧按话题/情绪/长度评分轮换，
            // 并在记忆检索未命中时作风格兜底。

            // 冷启动兜底：facts/traits 均为空时，尝试加载 persona.toml
            // 优先从 DB persona.config 读取，其次回退到文件系统
            if facts.is_empty()
                && traits.is_empty()
                && let Some(prompt) = load_persona_toml_prompt(p.config.as_deref())
            {
                tracing::info!("使用 persona.toml 加载的系统 prompt（无结构化画像）");
                return LoadedPromptMaterial::Plain(prompt);
            }

            // 知识层注入去重：RAG 摘要为主召回路径、断言知识为兜底。
            // 装配前剔除两类重复——① 来源文档已进入 RAG 覆盖集合的事实（同一事实已由
            // 摘要文本提供）；② 与角色层已知事实区同 id 的记录（角色区已展示，取后者去重）。
            // 无来源引用（手工/冷启动等）或 RAG 未覆盖的事实保留，兜底注入不失效。
            //
            // `[layer_dedup]` 开启（默认关闭 = 回退既有引用级去重路径）时，在引用级之上
            // 追加内容级去重与冲突仲裁（layer_guard）：剔除与角色区/RAG 摘要文本/行为规则
            // 内容级重复的知识卡片，产出保留方引用（证据可追溯，日志不含原文）。
            let knowledge_facts = if knowledge_facts.is_empty() {
                knowledge_facts
            } else if layer_dedup.enabled {
                use ramaria_memory::prompt::layer_guard::{
                    LayerGuardInput, RetentionKind, RetentionReference, arbitrate_fact_layers,
                };
                let covered: std::collections::HashSet<String> =
                    rag_covered_labels.iter().cloned().collect();
                // 保留参照：RAG 摘要文本（RAG 为主）+ 行为规则 reaction（行为层优先）
                let mut references: Vec<RetentionReference> = Vec::with_capacity(2);
                if let Some(rag) = rag_text.map(str::trim).filter(|s| !s.is_empty()) {
                    references.push(RetentionReference {
                        kind: RetentionKind::RagSummary,
                        text: rag,
                    });
                }
                if let Some(reaction) = behavior_decision
                    .as_ref()
                    .and_then(|d| d.primary_rule.reaction.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    references.push(RetentionReference {
                        kind: RetentionKind::BehaviorRule,
                        text: reaction,
                    });
                }
                let outcome = arbitrate_fact_layers(
                    &LayerGuardInput {
                        knowledge: &knowledge_facts,
                        role: &facts,
                        references: &references,
                        rag_covered_labels: &covered,
                    },
                    true,
                );
                if !outcome.traces.is_empty() {
                    // 只记计数/原因类别/保留方引用，不记原文全文（隐私红线）
                    tracing::debug!(
                        persona_uid = %p.uid,
                        before = knowledge_facts.len(),
                        after = outcome.knowledge_facts.len(),
                        reasons = ?outcome
                            .traces
                            .iter()
                            .map(|t| t.reason.as_str())
                            .collect::<Vec<_>>(),
                        kept_refs = ?outcome
                            .traces
                            .iter()
                            .map(|t| t.kept_ref.as_str())
                            .collect::<Vec<_>>(),
                        "层间证据去重与冲突仲裁已应用"
                    );
                }
                outcome.knowledge_facts
            } else {
                let covered: std::collections::HashSet<String> =
                    rag_covered_labels.iter().cloned().collect();
                let deduped = ramaria_memory::fact::retriever::dedup_knowledge_facts(
                    &knowledge_facts,
                    &covered,
                    &facts,
                );
                if deduped.len() != knowledge_facts.len() {
                    tracing::debug!(
                        persona_uid = %p.uid,
                        before = knowledge_facts.len(),
                        after = deduped.len(),
                        "知识层注入去重（RAG 覆盖/角色层重复剔除）"
                    );
                }
                deduped
            };

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
                current_time_str: Some(crate::now_timestamp_str()),
                weather: None,
                chat_style_rules: None, // v2.0: 无自定义规则时使用最小化默认规则
                // v1.4: utt 原文片段（检索层已按白名单与预算过滤，None 等同 v1.3）
                utt_context: utt_context.map(|s| s.to_string()),
                // 桥接内容（桥接层已按白名单与预算过滤，None 表示未启用）
                bridge_context: bridge_context.map(|s| s.to_string()),
                // 行为层路由决策（None = 未命中/关闭）
                behavior_decision,
                // 知识层 active 事实（判定器命中后由 send_message 检索传入，装配前已
                // 按 RAG 覆盖/角色层去重；空 = 关闭/未命中/全部去重 → prompt 不含知识块）
                knowledge_facts,
                // 自动风格规则（None = 风格关闭/数据不足 → prompt 与 v1.6 语义等价）
                style_rule_text,
            };

            // examples.max_examples 经 RamariaConfig 传播，
            // 与 `load_examples_for_input` 的预选上限保持一致（双闸门）。
            // 注入闸门映射（探针消融）：把 InjectionGate 逐子段翻译为 PromptConfig
            // 渲染开关——行为/知识在数据层已置空（behavior_decision/knowledge_facts），
            // 此处只需表达层与记忆块子段的渲染开关。
            let config = PromptConfig {
                max_examples,
                include_examples: injection.examples,
                include_speaking_style: injection.speaking_style,
                include_narrative: injection.narrative,
                include_memory_rag: injection.memory_rag,
                include_utt: injection.utt,
                include_bridge: injection.bridge,
                // 知识块渲染预算接线：core [knowledge].injection_budget_chars
                // 默认 800 → Some(800)，与 layers 默认预算一致（行为等价）；显式值生效。
                knowledge_block_max_chars: knowledge_budget_chars,
                ..Default::default()
            };
            tracing::debug!(
                persona_uid = %p.uid,
                facts = ctx.facts.len(),
                traits = ctx.traits.len(),
                examples = ctx.examples.len(),
                "四层 System Prompt 素材已加载"
            );
            return LoadedPromptMaterial::Structured(Box::new(ctx), config);
        }

        // 降级：默认 Ramaria 基础 prompt
        tracing::info!("使用默认 Ramaria System Prompt（无 persona 数据）");
        LoadedPromptMaterial::Plain(format!(
            "你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
             你可以记住与用户的对话历史，并在后续对话中引用这些记忆。\n\
             请用自然、友好的语气回复用户。如果用户提到之前聊过的内容，\
             请结合记忆上下文给出更有针对性的回复。\n\
             当前时间：{}",
            crate::now_timestamp_str()
        ))
    }
}

impl App {
    /// 构建 System Prompt（普通装配路径，行为与既有版本一致）。
    ///
    /// 说明:
    /// - 消费 `load_prompt_material`：结构化素材走 `assemble_prompt`，
    ///   纯文本降级（无 persona / 冷启动）原样返回。
    ///
    /// 参数见 `load_prompt_material`。
    #[allow(clippy::too_many_arguments)]
    async fn build_system_prompt_with_context(
        &self,
        persona_uid: Option<&str>,
        recent_summaries: &[String],
        last_active_at: Option<&str>,
        utt_context: Option<&str>,
        bridge_context: Option<&str>,
        behavior_decision: Option<ramaria_memory::behavior::MergedDecision>,
        examples: Vec<ramaria_core::types::PersonaExample>,
        max_examples: usize,
        knowledge_facts: Vec<ramaria_core::types::PersonaFact>,
        rag_covered_labels: &[String],
        knowledge_budget_chars: Option<usize>,
        injection: &ramaria_core::config::InjectionGate,
        rag_text: Option<&str>,
        layer_dedup: &ramaria_core::config::LayerDedupConfig,
    ) -> String {
        match self
            .load_prompt_material(
                persona_uid,
                recent_summaries,
                last_active_at,
                utt_context,
                bridge_context,
                behavior_decision,
                examples,
                max_examples,
                knowledge_facts,
                rag_covered_labels,
                knowledge_budget_chars,
                injection,
                rag_text,
                layer_dedup,
            )
            .await
        {
            LoadedPromptMaterial::Plain(prompt) => prompt,
            LoadedPromptMaterial::Structured(ctx, config) => assemble_prompt(&ctx, &config),
        }
    }

    /// 构建 System Prompt（注入协调预算装配路径，`[injection_budget].enabled=true`）。
    ///
    /// 说明:
    /// - 与 `build_system_prompt_with_context` 共享 `load_prompt_material`；
    ///   结构化素材走 `assemble_prompt_coordinated`（RAG 基座 + 四层在统一池内
    ///   按可配顺序分配），纯文本降级仅对 RAG 做协调裁剪（无注入层块）。
    /// - RAG 摘要经协调后可能被整块丢弃或句子边界截断（默认保留顺序下
    ///   RAG 优先级最高，仅在 `order` 显式排后且预算紧张时发生）。
    ///
    /// 知识层去重说明（已知边界）:
    /// - 知识去重在素材加载期按传入 `rag_covered_labels` 执行；协调将 RAG 整块
    ///   丢弃属"RAG 被配为低优先且预算紧张"的显式取舍，此时知识卡仍按协调前
    ///   覆盖集去重（保守去重）。去重/仲裁的精确一致化由层间去重任务覆盖。
    ///
    /// 参数:
    /// - 参数同 `load_prompt_material`（含 `rag_text`/`layer_dedup`，此处 `rag` 即 rag_text）。
    /// - `budget`: 注入协调预算配置（core `[injection_budget]`）。
    ///
    /// 返回:
    /// - `CoordinatedPrompt`：协调后的 system_prompt / memory_context / 统计。
    #[allow(clippy::too_many_arguments)]
    async fn build_system_prompt_coordinated(
        &self,
        persona_uid: Option<&str>,
        recent_summaries: &[String],
        last_active_at: Option<&str>,
        utt_context: Option<&str>,
        bridge_context: Option<&str>,
        behavior_decision: Option<ramaria_memory::behavior::MergedDecision>,
        examples: Vec<ramaria_core::types::PersonaExample>,
        max_examples: usize,
        knowledge_facts: Vec<ramaria_core::types::PersonaFact>,
        rag_covered_labels: &[String],
        knowledge_budget_chars: Option<usize>,
        injection: &ramaria_core::config::InjectionGate,
        rag: Option<&str>,
        budget: &ramaria_core::config::InjectionBudgetConfig,
        layer_dedup: &ramaria_core::config::LayerDedupConfig,
    ) -> ramaria_memory::prompt::builder::CoordinatedPrompt {
        match self
            .load_prompt_material(
                persona_uid,
                recent_summaries,
                last_active_at,
                utt_context,
                bridge_context,
                behavior_decision,
                examples,
                max_examples,
                knowledge_facts,
                rag_covered_labels,
                knowledge_budget_chars,
                injection,
                rag,
                layer_dedup,
            )
            .await
        {
            LoadedPromptMaterial::Plain(prompt) => {
                let alloc =
                    ramaria_memory::token_budget::allocate_injection_budget(&[], rag, budget);
                ramaria_memory::prompt::builder::CoordinatedPrompt {
                    system_prompt: prompt,
                    memory_context: alloc.memory_context,
                    dropped: alloc.dropped,
                    injected_tokens: alloc.injected_tokens,
                    fallback_truncated: alloc.fallback_truncated,
                }
            }
            LoadedPromptMaterial::Structured(ctx, config) => {
                assemble_prompt_coordinated(&ctx, &config, budget, rag)
            }
        }
    }
}

// =========================================================
// Prompt 装配素材（共享加载，供普通/协调装配消费）
// =========================================================

/// 已加载的 System Prompt 装配素材。
///
/// 职责:
/// - 承载普通装配（`build_system_prompt_with_context`）与协调装配
///   （`build_system_prompt_coordinated`）共享的 persona 数据加载结果。
enum LoadedPromptMaterial {
    /// 纯文本 prompt（无 persona / persona.toml 冷启动兜底），无可协调注入块。
    Plain(String),
    /// 结构化装配上下文（装箱压缩枚举体积；可经 `render_prompt_parts`
    /// 拆分为固定骨架 + 注入块）。
    Structured(Box<PromptContext>, PromptConfig),
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
        // 无自定义 E_rules 时使用共享社交平台口吻
        .unwrap_or(SHARED_CHAT_STYLE_RULES);

    let name = &parsed.assistant_name;
    let time_str = crate::now_timestamp_str();

    Some(format!(
        "你的名字是{name}。\n\n{persona_block}\n\n回复规则:\n{rules_block}\n\n\
         当前时间：{time_str}\n\n\
         你可以记住与用户的对话历史。如果用户提到之前聊过的内容，\
         请结合记忆上下文给出更有针对性的回复。"
    ))
}

/// 预选 Few-shot 示例（v1.4 examples 激活）。
///
/// 选择策略:
/// - `examples.enabled=false` → 回退 v1.3：静态 `selected=1` 查询（`list_selected_examples`）。
/// - `examples.enabled=true`：
///   - 记忆检索命中（`memory_hit=true`）→ 不注入（避免与记忆内容重复）；
///   - 记忆未命中 → 从候选池按话题/情绪/长度评分轮换选择，风格兜底。
///
/// 降级:
/// - 候选池为空 / 存储失败 → 空列表（不注入，等同 v1.3）。
/// - 评分选择不满足最低条数 → 空列表（example_selector 语义，不强制凑数）。
///
/// 安全约束:
/// - 日志只记录数量，不记录示例内容。
///
/// 参数:
/// - `persona_uid`: 人格 UID（None 表示 rama 自身，回退 "rama-0001"）。
/// - `user_input`: 用户当前输入（话题匹配关键词来源）。
/// - `memory_hit`: 记忆检索是否命中（RAG 上下文非空）。
///
/// 返回:
/// - 注入用示例列表（最多 `[examples].max_examples` 条）。
async fn load_examples_for_input(
    storage: &dyn ramaria_core::traits::StorageBackend,
    examples_cfg: &ramaria_core::config::ExamplesConfig,
    persona_uid: Option<&str>,
    user_input: &str,
    memory_hit: bool,
) -> Vec<ramaria_core::types::PersonaExample> {
    use ramaria_memory::prompt::example_selector::{ExampleSelector, ExampleSelectorConfig};

    let uid = persona_uid.unwrap_or("rama-0001");

    // v1.3 兼容路径：静态 selected 注入（无条件）
    if !examples_cfg.enabled {
        return storage
            .list_selected_examples(uid)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(persona_uid = %uid, %e, "加载 selected examples 失败，跳过");
                Vec::new()
            });
    }

    // v1.4 路径：记忆命中不重复注入（兜底语义）
    if memory_hit {
        tracing::debug!(persona_uid = %uid, "记忆检索命中，跳过 examples 兜底注入");
        return Vec::new();
    }

    // 记忆未命中 → 候选池评分轮换（风格兜底）
    let candidates = storage.list_all_examples(uid).await.unwrap_or_else(|e| {
        tracing::warn!(persona_uid = %uid, %e, "加载 examples 候选池失败，跳过");
        Vec::new()
    });
    if candidates.is_empty() {
        tracing::debug!(persona_uid = %uid, "examples 候选池为空，跳过注入");
        return Vec::new();
    }

    let keywords = ramaria_memory::prompt::example_selector::extract_keywords(user_input);
    let keyword_refs: Vec<&str> = keywords.iter().map(|s| s.as_str()).collect();
    let selector_config = ExampleSelectorConfig {
        max_examples: examples_cfg.max_examples as usize,
        ..ExampleSelectorConfig::default()
    };

    let selected = ExampleSelector::select(&candidates, &keyword_refs, 0.0, &selector_config);

    tracing::debug!(
        persona_uid = %uid,
        candidates = candidates.len(),
        selected = selected.len(),
        "examples 评分轮换完成（记忆未命中兜底注入）"
    );
    selected
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

    // 回退到旧路径
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
    //    用户消息现在也携带 persona_uid，表示"在此 persona 的对话上下文中"
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
        let done_event = StreamEvent::done(
            request_id,
            Some(session_id),
            backend_id,
            full_reply.chars().count(),
        );
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
// examples 预选测试（v1.4）
// =========================================================

#[cfg(test)]
mod examples_tests {
    use super::load_examples_for_input;
    use crate::stages::test_utils::MockStorage;
    use ramaria_core::config::ExamplesConfig;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::PersonaExample;
    use std::sync::Arc;

    /// 构造候选示例（tags 逗号分隔）。
    fn example(uid: &str, partner: &str, reply: &str, tags: Option<&str>) -> PersonaExample {
        let mut e = PersonaExample::new(uid.to_string(), partner.to_string(), reply.to_string());
        e.tags = tags.map(|s| s.to_string());
        e
    }

    fn enabled_cfg(max_examples: u32) -> ExamplesConfig {
        ExamplesConfig {
            enabled: true,
            max_examples,
        }
    }

    #[tokio::test]
    async fn miss_injects_scored_examples() {
        // 记忆未命中 → 候选池评分轮换注入（风格兜底）
        let storage = Arc::new(MockStorage::new());
        storage
            .save_example(&example(
                "char-0001",
                "今天天气好吗",
                "很好呀我们出去玩吧",
                Some("天气,公园"),
            ))
            .await
            .unwrap();
        storage
            .save_example(&example(
                "char-0001",
                "晚上吃什么",
                "火锅怎么样",
                Some("晚餐,火锅"),
            ))
            .await
            .unwrap();

        let selected = load_examples_for_input(
            storage.as_ref(),
            &enabled_cfg(5),
            Some("char-0001"),
            "今天天气怎么样",
            false,
        )
        .await;

        assert!(!selected.is_empty(), "未命中应注入");
        assert!(
            selected.iter().any(|e| e.reply.contains("很好呀")),
            "话题相关示例应入选"
        );
    }

    #[tokio::test]
    async fn hit_skips_injection() {
        // 记忆检索命中 → 不重复注入
        let storage = Arc::new(MockStorage::new());
        storage
            .save_example(&example("char-0001", "你好", "你好呀朋友", Some("问候")))
            .await
            .unwrap();

        let selected = load_examples_for_input(
            storage.as_ref(),
            &enabled_cfg(5),
            Some("char-0001"),
            "你好",
            true,
        )
        .await;
        assert!(selected.is_empty(), "命中记忆时不重复注入");
    }

    #[tokio::test]
    async fn empty_pool_skips_injection() {
        let storage = Arc::new(MockStorage::new());
        let selected = load_examples_for_input(
            storage.as_ref(),
            &enabled_cfg(5),
            Some("char-0001"),
            "任何话题",
            false,
        )
        .await;
        assert!(selected.is_empty(), "候选池为空不注入");
    }

    #[tokio::test]
    async fn disabled_falls_back_to_selected() {
        // v1.3 兼容：enabled=false → 静态 selected=1 无条件注入
        let storage = Arc::new(MockStorage::new());
        let mut sel = example("char-0001", "你好", "你好呀朋友", Some("问候"));
        sel.selected = true;
        storage.save_example(&sel).await.unwrap();
        storage
            .save_example(&example("char-0001", "未选中", "未选中的回复内容", None))
            .await
            .unwrap();

        let selected = load_examples_for_input(
            storage.as_ref(),
            &ExamplesConfig {
                enabled: false,
                max_examples: 5,
            },
            Some("char-0001"),
            "任意输入",
            true, // 命中记忆也注入（v1.3 无条件语义）
        )
        .await;
        assert_eq!(selected.len(), 1, "仅 selected=1 的示例注入");
        assert_eq!(selected[0].partner, "你好");
    }

    #[tokio::test]
    async fn disabled_no_selected_returns_empty() {
        let storage = Arc::new(MockStorage::new());
        storage
            .save_example(&example("char-0001", "你好", "你好呀朋友", None))
            .await
            .unwrap();
        let selected = load_examples_for_input(
            storage.as_ref(),
            &ExamplesConfig {
                enabled: false,
                max_examples: 5,
            },
            Some("char-0001"),
            "你好",
            false,
        )
        .await;
        assert!(selected.is_empty(), "无 selected 示例 → 空");
    }

    #[tokio::test]
    async fn topic_match_ranks_first() {
        // 话题相关（tags 含查询关键词）的示例应排在无关示例之前
        let storage = Arc::new(MockStorage::new());
        storage
            .save_example(&example(
                "char-0001",
                "无关话题",
                "这是完全无关的回复内容",
                Some("旅行"),
            ))
            .await
            .unwrap();
        storage
            .save_example(&example(
                "char-0001",
                "编程问题",
                "这个 bug 我帮你看看代码",
                Some("编程,代码"),
            ))
            .await
            .unwrap();

        let selected = load_examples_for_input(
            storage.as_ref(),
            &enabled_cfg(5),
            Some("char-0001"),
            "帮我看看这段代码",
            false,
        )
        .await;
        assert!(!selected.is_empty());
        assert_eq!(
            selected[0].tags.as_deref().unwrap(),
            "编程,代码",
            "话题相关排前"
        );
    }

    #[tokio::test]
    async fn max_examples_respected() {
        let storage = Arc::new(MockStorage::new());
        for i in 0..4 {
            storage
                .save_example(&example(
                    "char-0001",
                    &format!("问题{i}"),
                    &format!("这是第{i}条回复内容"),
                    None,
                ))
                .await
                .unwrap();
        }
        let selected = load_examples_for_input(
            storage.as_ref(),
            &enabled_cfg(2),
            Some("char-0001"),
            "随便聊聊",
            false,
        )
        .await;
        assert_eq!(selected.len(), 2, "max_examples=2 生效");
    }

    #[tokio::test]
    async fn no_persona_falls_back_to_rama() {
        // persona_uid=None（rama 自身会话）→ 查 rama-0001 候选池
        let storage = Arc::new(MockStorage::new());
        storage
            .save_example(&example("rama-0001", "你好", "你好呀我是助手", None))
            .await
            .unwrap();
        let selected =
            load_examples_for_input(storage.as_ref(), &enabled_cfg(5), None, "你好", false).await;
        assert!(!selected.is_empty(), "rama 自身也参与兜底");
    }

    #[tokio::test]
    async fn scoring_uses_tags_without_llm() {
        // 评分纯规则（无 LLM）：相同话题多候选时按 tag 命中数排序
        let storage = Arc::new(MockStorage::new());
        storage
            .save_example(&example("char-0001", "A", "回复内容甲", Some("天气")))
            .await
            .unwrap();
        storage
            .save_example(&example(
                "char-0001",
                "B",
                "回复内容乙",
                Some("天气,公园,散步"),
            ))
            .await
            .unwrap();

        let selected = load_examples_for_input(
            storage.as_ref(),
            &enabled_cfg(5),
            Some("char-0001"),
            "今天天气好去公园散步",
            false,
        )
        .await;
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].partner, "B", "tags 命中更多的示例排前");
    }
}
