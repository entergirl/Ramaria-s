//! crates/ramaria-app/src/app_chat.rs - 核心对话管线
//!
//! 设计特点:
//! - 生产对话管线只装配 Stage 1-5（`SendMessagePipeline` + 5 个独立 Stage）；
//!   Steps 6-10（System Prompt / Token Budget / ChatRequest / LLM 调用 / 消息持久化）
//!   为本文件内联实现；`stages/{build_prompt,token_budget,build_request,call_llm,
//!   persist_message}.rs` 为未接线（预留），仅供 `tests/m2_integration.rs` 组装验证
//! - 注入协调预算（`[injection_budget]`，默认关闭）：开启时 Step 6 走
//!   `build_system_prompt_coordinated`（RAG 基座 + 四层在统一池内按可配顺序分配），
//!   关闭时走普通装配路径（行为逐字段等价）
//! - 装配素材加载 / 普通装配 / 示例预选 / 行为路由为薄委托（实现见
//!   `ramaria_memory::chat` / `ramaria_memory::behavior::orchestrate`，与 service / MCP 入口同源）
//! - 自由函数: `stream_forward_task`（流式转发）
//! - 降级策略: 嵌入模型不可用 → 仅 BM25+图谱检索；persona.toml 缺失 → 默认 Ramaria prompt
//! - 安全约束: 不记录完整 prompt 或用户消息；线上 LLM 调用前强制隐私确认

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use futures::channel::mpsc;
use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::{ChatMessage, ChatRequest, StorageBackend};
use ramaria_core::types::{Message, MessageRole, MessageSource, new_id, now_ms};
use ramaria_memory::chat::LoadedPromptMaterial;
use ramaria_memory::prompt::builder::assemble_prompt_coordinated;
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
    /// 完整管线:
    /// Steps 1-5 → `SendMessagePipeline` 编排 5 个独立 Stage
    /// Steps 6-10（System Prompt / Token Budget / ChatRequest / LLM 调用 / 消息持久化）
    /// → 本文件内联实现
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
        let llm = { lock_recover(&self.llm, "app_chat.llm").clone() };
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
        // 检索索引健康标志：与 App 共享同一标志位，供 Stage 5 在重建失败后告警
        let retriever_rebuild_failed = Arc::clone(&self.retriever_rebuild_failed);

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
        .with_retriever_rebuild_failed(retriever_rebuild_failed)
    }

    // =========================================================
    // 内部辅助方法
    // =========================================================

    /// 加载 System Prompt 装配素材（薄委托：实现与降级口径见
    /// `ramaria_memory::chat::load_prompt_material`，与 service / MCP 入口同源）。
    ///
    /// 参数:
    /// - 与 `ramaria_memory::chat::PromptMaterialInputs` 字段一一对应
    ///   （persona 数据 / 近期摘要 / utt / 桥接 / 行为决策 / 示例 / 知识事实 / 闸门与去重配置）。
    ///
    /// 返回:
    /// - `Plain`: 无 persona / persona.toml 冷启动兜底（纯文本 prompt）。
    /// - `Structured`: 结构化上下文 + 渲染配置（普通 / 协调装配共用）。
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
        let inputs = self.prompt_material_inputs(
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
        );
        ramaria_memory::chat::load_prompt_material(self.storage.as_ref(), &inputs).await
    }

    /// 组装装配素材输入集合（素材加载薄委托的公共构造）。
    ///
    /// 说明:
    /// - 14 个业务参数与 `ramaria_memory::chat::PromptMaterialInputs` 字段逐一对应；
    /// - `style_enabled` 取主配置 `[style].enabled`（表达层风格子系统总开关，
    ///   与注入闸门独立：关闭时不加载自动风格规则）。
    #[allow(clippy::too_many_arguments)]
    fn prompt_material_inputs<'a>(
        &self,
        persona_uid: Option<&'a str>,
        recent_summaries: &'a [String],
        last_active_at: Option<&'a str>,
        utt_context: Option<&'a str>,
        bridge_context: Option<&'a str>,
        behavior_decision: Option<ramaria_memory::behavior::MergedDecision>,
        examples: Vec<ramaria_core::types::PersonaExample>,
        max_examples: usize,
        knowledge_facts: Vec<ramaria_core::types::PersonaFact>,
        rag_covered_labels: &'a [String],
        knowledge_budget_chars: Option<usize>,
        injection: &'a ramaria_core::config::InjectionGate,
        rag_text: Option<&'a str>,
        layer_dedup: &'a ramaria_core::config::LayerDedupConfig,
    ) -> ramaria_memory::chat::PromptMaterialInputs<'a> {
        ramaria_memory::chat::PromptMaterialInputs {
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
            style_enabled: self.config.style.enabled,
            rag_text,
            layer_dedup,
        }
    }
}

impl App {
    /// 构建 System Prompt（普通装配路径，薄委托）。
    ///
    /// 说明:
    /// - 调用 `ramaria_memory::chat::build_system_prompt`：结构化素材走 5-Block
    ///   装配器，纯文本降级（无 persona / 冷启动）原样返回。
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
        let inputs = self.prompt_material_inputs(
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
        );
        ramaria_memory::chat::build_system_prompt(self.storage.as_ref(), &inputs).await
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
// 示例预选（薄委托）
// =========================================================

/// 预选 Few-shot 示例（薄委托：选择策略与降级口径见
/// `ramaria_memory::chat::load_examples_for_input`，与 service / MCP 入口同源）。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `examples_cfg`: 示例配置（`[examples]`）。
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
    ramaria_memory::chat::load_examples_for_input(
        storage,
        examples_cfg,
        persona_uid,
        user_input,
        memory_hit,
    )
    .await
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

// =========================================================
// 回复规则接线测试（生产装配路径）
// =========================================================

#[cfg(test)]
mod prompt_rules_tests {
    use crate::App;
    use crate::stages::test_utils::{MockLlm, MockStorage};
    use ramaria_core::config::{InjectionGate, LayerDedupConfig, RamariaConfig};
    use ramaria_core::traits::{StorageBackend, StoreCrud};
    use ramaria_core::types::{FactSource, Persona, PersonaFact, PersonaKind, ProfileField};
    use ramaria_memory::chat::LoadedPromptMaterial;
    use ramaria_memory::prompt::builder::assemble_prompt;
    use std::sync::Arc;

    /// 构造含指定 persona.config 的 App；写入角色层事实以跳过冷启动兜底。
    async fn app_with_persona_config(config: Option<&str>) -> App {
        let storage = Arc::new(MockStorage::new());
        let mut persona = Persona::new(
            "char-0001".to_string(),
            "小夏".to_string(),
            PersonaKind::Char,
            1,
            "local".to_string(),
        );
        persona.config = config.map(|s| s.to_string());
        storage.add_persona(persona);
        storage
            .save_fact(&PersonaFact::new(
                "char-0001".to_string(),
                ProfileField::BasicInfo,
                "出生于上海".to_string(),
                FactSource::Event,
            ))
            .await
            .expect("保存角色层事实");

        App::new_without_embedding(
            storage as Arc<dyn StorageBackend>,
            Arc::new(MockLlm::local()),
            RamariaConfig::default(),
            Arc::new(ramaria_llm::keychain::Keychain::new()),
        )
    }

    /// 走生产素材加载 + 普通装配路径产出 system prompt。
    async fn assembled_prompt(app: &App) -> String {
        let injection = InjectionGate::default();
        let layer_dedup = LayerDedupConfig::default();
        let material = app
            .load_prompt_material(
                Some("char-0001"),
                &[],
                None,
                None,
                None,
                None,
                Vec::new(),
                5,
                Vec::new(),
                &[],
                None,
                &injection,
                None,
                &layer_dedup,
            )
            .await;
        match material {
            LoadedPromptMaterial::Structured(ctx, config) => assemble_prompt(&ctx, &config),
            LoadedPromptMaterial::Plain(_) => panic!("facts 非空时应走结构化装配"),
        }
    }

    /// 无显式 E_rules：核心规则回退共享规则（含 || 分条契约）。
    #[tokio::test]
    async fn prompt_injects_shared_rules_without_explicit_e_rules() {
        let app = app_with_persona_config(None).await;
        let prompt = assembled_prompt(&app).await;
        assert!(
            prompt.contains("### 核心规则"),
            "生产路径应注入核心规则子段:\n{prompt}"
        );
        assert!(
            prompt.contains("需要分条时用「||」分隔"),
            "缺省 E_rules 时应回退共享规则:\n{prompt}"
        );
    }

    /// 显式 E_rules：核心规则改用 persona 自定义规则（共享规则不注入）。
    #[tokio::test]
    async fn prompt_prefers_explicit_e_rules() {
        let config =
            "[identity]\nassistant_name = \"小夏\"\n\n[blocks]\nE_rules = \"自定义规则内容\"\n";
        let app = app_with_persona_config(Some(config)).await;
        let prompt = assembled_prompt(&app).await;
        assert!(
            prompt.contains("自定义规则内容"),
            "应注入显式 E_rules:\n{prompt}"
        );
        assert!(
            !prompt.contains("需要分条时用「||」分隔"),
            "显式 E_rules 应覆盖共享规则:\n{prompt}"
        );
    }
}
