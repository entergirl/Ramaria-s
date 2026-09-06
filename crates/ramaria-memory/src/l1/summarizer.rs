//! crates/ramaria-memory/src/l1/summarizer.rs - L0→L1 摘要生成管线
//!
//! 设计特点:
//! - 依赖注入: 通过 `&dyn LlmProvider` + `&dyn StorageBackend` 解耦具体实现
//! - 完整流程: 取消息 → 格式化 → 选Prompt → 调LLM → 解析JSON → 校验 → 存L1 + 写关键词
//! - JSON 解析三步递进: 直接解析 → 剥离 think 标签 → 正则提取
//! - 字段校验对齐五档效价/显著性，非法值自动钳制到最近合法档位
//! - 关键词自动写回 keyword_pool，驱动词典累积
//! - 所有可恢复错误转换为 RamariaError，保留上下文

use ramaria_core::keyword::KeywordToken;
use ramaria_core::traits::ChatRequest;
use ramaria_core::types::EvidenceNote;
use ramaria_core::{LlmProviderTrait, MemoryL1, RamariaError, RamariaResult, StorageBackend};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::prompt::{KEYWORD_INJECT_LIMIT, KEYWORD_INJECT_THRESHOLD, build_l1_prompt};
use crate::utils;
use crate::utt::UttChunk;

// =========================================================
// LLM 响应 JSON 结构（反序列化目标）
// =========================================================

/// LLM 返回的 L1 摘要 JSON 结构。
///
/// 字段:
/// - 所有字段均为 `Option`，以容忍 LLM 输出缺失字段。
/// - 校验阶段再填充默认值，避免解析阶段 panic。
/// - `situation_strength` 为新增字段，prompt 已包含此输出，缺失时由 config 注入或默认 3。
/// - `evidence_notes` 为结构化证据线索（v1.4），LLM 可能输出缺失或空数组，
///   校验失败时降级为 `Some(vec![])` 但不阻塞 L1 生成。
///   M1~M3 过渡期 LLM 仍可能输出旧字符串数组，由自定义反序列化器统一转换为
///   对象数组（字符串落 `text` 槽位）；M4 起 prompt 升级为对象数组。
#[derive(Debug, Deserialize)]
struct L1SummaryResponse {
    summary: Option<String>,
    keywords: Option<String>,
    time_period: Option<String>,
    atmosphere: Option<String>,
    valence: Option<f64>,
    salience: Option<f64>,
    /// 情境强度 1-5，None 时按默认值 3 处理
    #[serde(default)]
    situation_strength: Option<i32>,
    /// 证据线索列表（宽容解析：对象数组 / 旧字符串数组 / 缺失）
    #[serde(default, deserialize_with = "deserialize_evidence_notes")]
    evidence_notes: Option<Vec<EvidenceNote>>,
    /// 相对上一块的话题延续关系（v1.5 B2）：延续/转折/无关。
    /// 无上一块时 prompt 不含该字段，LLM 不会输出 → None。
    #[serde(default)]
    continuation: Option<String>,
}

// =========================================================
// L1 Summarizer 配置
// =========================================================

/// L1 Summarizer 配置。
///
/// 字段约定:
/// - `max_tokens`: LLM 最大输出 token 数，默认 1024。v1.4 起 L1 输出包含
///   `evidence_notes` 结构化对象数组（1-3 条 × text/time/who/cause 槽位），
///   完整 JSON 明显长于旧版字符串数组输出；512（Python 旧值）过紧会导致
///   LLM 输出被截断、JSON 解析失败，故默认提升至 1024。
/// - `temperature`: LLM 生成温度，默认 0.3。
/// - `conversation_format_user`: 用户消息格式化前缀。
/// - `conversation_format_assistant`: 助手消息格式化前缀。
/// - `persona_uid`: 本条摘要描述的对象（人格标识），None 表示描述默认用户。
/// - `context_json`: 分组上下文，含 chat_partners 列表。
/// - `situation_strength`: 情境强度（1-5），None 时 LLM 输出缺失则默认 3。
/// - `utt_splitter`（v1.5 B2）: utt 切分配置。`Some` → 将 session 消息切分为
///   话语块并逐块生成 L1（块 N 注入上一块上文，上下文感知生成，§6.3）；
///   `None` → 整会话一块，与 v1.4 行为完全一致（独立摘要，无上文注入）。
///   短会话（单块）自然回退 v1.4 行为。
/// - `prior_context_threshold`（v1.5 B2）: 上一块消息数 ≤ 此阈值 → 注入 L0 原文；
///   超过 → 注入上一块 L1 摘要 + 结构化线索。默认 20（§6.3 示例值）。
/// - `prior_context_max_chars`（v1.5 B2）: 长块无上一 L1 时回退注入原文的截断上限。
#[derive(Debug, Clone)]
pub struct L1SummarizerConfig {
    /// LLM 生成温度 0.0..2.0
    pub temperature: f64,
    /// LLM 最大输出 tokens
    pub max_tokens: u32,
    /// 用户消息格式化前缀
    pub user_prefix: String,
    /// 助手消息格式化前缀
    pub assistant_prefix: String,
    /// 人格关联——本条摘要描述的对象
    pub persona_uid: Option<String>,
    /// 分组上下文——JSON 格式 `{"chat_partners": ["user-0001", "char-0003"]}`
    pub context_json: Option<String>,
    /// 情境强度默认值（1-5），None 时使用 3
    pub situation_strength: Option<i32>,
    /// utt 切分配置（v1.5 B2 上下文感知生成），None = v1.4 整会话单块
    pub utt_splitter: Option<crate::utt::UttSplitterConfig>,
    /// 上一块消息数阈值（≤ 注入原文，> 注入上一 L1 摘要+线索），默认 20
    pub prior_context_threshold: usize,
    /// 长块无上一 L1 时原文截断上限（字符），默认 1500
    pub prior_context_max_chars: usize,
}

impl Default for L1SummarizerConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 1024,
            user_prefix: "用户：".to_string(),
            assistant_prefix: "助手：".to_string(),
            persona_uid: None,
            context_json: None,
            situation_strength: None,
            utt_splitter: Some(crate::utt::UttSplitterConfig::default()),
            prior_context_threshold: 20,
            prior_context_max_chars: 1500,
        }
    }
}

// =========================================================
// L1 Summarizer
// =========================================================

/// L0→L1 摘要生成器。
///
/// 职责:
/// - 接收已关闭的 session_id，读取全部 L0 消息。
/// - 调用 LLM 生成结构化摘要 JSON。
/// - 解析、校验并写入 `memory_l1` 表。
/// - 将关键词写回 `keyword_pool`。
///
/// 用法:
/// ```no_run
/// # use ramaria_memory::{L1Summarizer, L1SummarizerConfig};
/// // llm / storage 由上层注入（&dyn LlmProviderTrait / &dyn StorageBackend）；
/// // 需完整 mock 才能运行，故示例仅示意构造（no_run）。
/// let summarizer = L1Summarizer::new(todo!(), todo!(), L1SummarizerConfig::default());
/// # let _ = &summarizer;
/// ```
pub struct L1Summarizer<'a> {
    config: L1SummarizerConfig,
    llm: &'a dyn LlmProviderTrait,
    storage: &'a dyn StorageBackend,
}

impl<'a> L1Summarizer<'a> {
    /// 创建新的 L1Summarizer。
    pub fn new(
        llm: &'a dyn LlmProviderTrait,
        storage: &'a dyn StorageBackend,
        config: L1SummarizerConfig,
    ) -> Self {
        Self {
            config,
            llm,
            storage,
        }
    }

    // =========================================================
    // 公共 API
    // =========================================================

    /// 为指定 session 生成 L1 摘要。
    ///
    /// v1.5 B2（§6.3）上下文感知生成：
    /// - 配置了 `utt_splitter` 时，将 session 消息切分为话语块，逐块生成 L1；
    ///   块 N（N>0）生成时注入上一块上文（短块注入原文 / 长块注入上一 L1 摘要+线索），
    ///   输出 `continuation`（延续/转折/无关）。
    /// - 单块 session 或未配置切分器 → 与 v1.4 行为完全一致（独立摘要）。
    ///
    /// 块级容错：
    /// - 某块 LLM 调用/解析失败 → 记 warn、该块不产出 L1，后续块以上一块
    ///   原文（截断）作为上文继续生成（不阻塞整体）。
    /// - 全部块均失败 → 返回最后一个错误（与 v1.4 失败语义一致）。
    /// - 成功块统一写库；写库失败为硬错误直接返回。
    ///
    /// 参数:
    /// - `session_id`: 已关闭的 session UUID。
    ///
    /// 返回:
    /// - 成功时返回最后成功块的 `MemoryL1`（调用方从库读取的顺序一致）。
    /// - session 无消息时返回 Validation 错误。
    pub async fn summarize_session(&self, session_id: Uuid) -> RamariaResult<MemoryL1> {
        // 1. 读取 session 全部消息
        let messages = self.storage.list_messages(session_id).await.map_err(|e| {
            warn!(%session_id, error=%e, "读取 session 消息失败");
            RamariaError::storage(format!("读取 session {session_id} 消息失败: {e}"))
        })?;

        if messages.is_empty() {
            return Err(RamariaError::validation(format!(
                "session {session_id} 无消息，无法生成摘要"
            )));
        }

        debug!(%session_id, msg_count = messages.len(), "开始生成 L1 摘要");

        // 2. 切分为话语块（B2 上下文感知生成的块粒度）
        //    - 配置了 utt_splitter → 用 split_messages 切分（目标 persona 为 config.persona_uid）
        //    - 未配置 → 整会话一块（v1.4 行为）
        //    - 切分结果为空（如纯用户消息块被丢弃）→ 回退整会话一块，
        //      保证与 v1.4 至少产出一条摘要的语义一致。
        let chunks = match &self.config.utt_splitter {
            Some(splitter_cfg) => {
                let target = self.config.persona_uid.as_deref();
                let split = crate::utt::splitter::split_messages(&messages, target, splitter_cfg);
                if split.is_empty() {
                    vec![UttChunk::from_messages(messages.clone())]
                } else {
                    split
                }
            }
            None => vec![UttChunk::from_messages(messages)],
        };
        debug!(%session_id, block_count = chunks.len(), "L1 摘要按块生成");

        // 3. 逐块生成（内存收集，全部成功后统一写库）
        //    - generated[i] = 块 i 的 (L1, 关键词列表)（None = 该块生成失败，降级）
        //    - 块 i 的上文来自块 i-1 的生成结果（内存传递，只注入最近 1 块，不链式）
        let mut generated: Vec<Option<(MemoryL1, Vec<KeywordToken>)>> =
            Vec::with_capacity(chunks.len());
        let mut last_error: Option<RamariaError> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            // 构建上一块上文（混合形态，§6.3）：
            // - 上一块消息数 ≤ prior_context_threshold → 注入 L0 原文
            // - 长块 → 注入上一 L1 摘要 + 结构化线索（上一 L1 缺失 → 原文截断）
            let prior_context = if i == 0 {
                None
            } else {
                Some(build_prior_context(
                    &chunks[i - 1],
                    generated[i - 1].as_ref().map(|(l1, _)| l1),
                    &self.config,
                    &self.config.user_prefix,
                    &self.config.assistant_prefix,
                ))
            };

            match self
                .generate_chunk_l1(session_id, chunk, prior_context.as_deref())
                .await
            {
                Ok((l1, keywords)) => {
                    debug!(%session_id, block_index = i, "块 {} L1 生成成功", i);
                    generated.push(Some((l1, keywords)));
                }
                Err(e) => {
                    // 块级降级：不阻塞整体，后续块以原文（截断）作为上文
                    warn!(%session_id, block_index = i, error=%e, "L1 块生成失败，该块无摘要（降级继续）");
                    last_error = Some(e);
                    generated.push(None);
                }
            }
        }

        // 4. 统一写库（成功块）+ 写回关键词
        let mut saved_last: Option<MemoryL1> = None;
        for entry in generated.iter().flatten() {
            let (l1, keywords) = entry;
            self.storage.save_memory_l1(l1).await.map_err(|e| {
                warn!(%session_id, l1_id = %l1.id, error=%e, "写入 memory_l1 失败");
                RamariaError::storage(format!("写入 session {session_id} L1 摘要失败: {e}"))
            })?;
            saved_last = Some(l1.clone());
            self.write_back_keywords(session_id, l1, keywords).await;
        }

        // 5. 返回
        match saved_last {
            Some(l1) => {
                info!(
                    %session_id,
                    l1_id = %l1.id,
                    total_blocks = chunks.len(),
                    success_blocks = generated.iter().filter(|g| g.is_some()).count(),
                    "L1 摘要生成完成（按块）"
                );
                Ok(l1)
            }
            None => Err(last_error.unwrap_or_else(|| {
                RamariaError::validation(format!("session {session_id} 全部 L1 块生成失败"))
            })),
        }
    }

    /// 渐进式摘要（v1.7 B3，决策 D-V17-005）。
    ///
    /// 触发条件（`[l1.progressive]` 配置）:
    /// - 会话消息数 > `msg_threshold`（默认 100），或
    /// - 首末消息时间跨度 > `span_hours`（默认 24 小时）。
    ///
    /// 触发行为:
    /// - 用 `tail_msg_count`（默认 60）作为单块消息数上限切分消息，逐块生成 L1；
    ///   最后一块覆盖最新对话（封存只摘要尾部），前面各段独立成 L1 不跨段混合。
    /// - 全部段 L1 写库且 `absorbed=false`（入候选池），L2 事件提取仍按封存触发
    ///   （`list_unabsorbed_l1` 天然包含渐进式段 L1，无需额外缓冲结构）。
    /// - 每段 L1 生成后写回关键词词典 + 倒排索引（与 `summarize_session` 一致）。
    ///
    /// 未触发:
    /// - 委托 `summarize_session`（v1.6 行为：整会话 / 按 utt 切分），返回单元素列表。
    ///
    /// 容错:
    /// - 某段 LLM 调用/解析失败 → 记 warn、该段不产出，其余段照常生成（不阻塞整体）。
    /// - 全部段均失败 → 返回最后一个错误（与 v1.4 失败语义一致）。
    ///
    /// 参数:
    /// - `session_id`: 已关闭的 session UUID。
    /// - `progressive`: 渐进式摘要配置（未启用时直接回退 v1.6）。
    ///
    /// 返回:
    /// - 成功时返回本次生成的全部 L1（触发时 ≥1 条，未触发时 1 条）。
    pub async fn summarize_progressive(
        &self,
        session_id: Uuid,
        progressive: &ramaria_core::config::L1ProgressiveConfig,
    ) -> RamariaResult<Vec<MemoryL1>> {
        // 1. 读取 session 全部消息
        let messages = self.storage.list_messages(session_id).await.map_err(|e| {
            warn!(%session_id, error=%e, "渐进式摘要：读取 session 消息失败");
            RamariaError::storage(format!(
                "渐进式摘要：读取 session {session_id} 消息失败: {e}"
            ))
        })?;

        if messages.is_empty() {
            return Err(RamariaError::validation(format!(
                "session {session_id} 无消息，无法生成摘要"
            )));
        }

        // 2. 触发判断：未启用或未达阈值 → 回退 v1.6 行为（整会话摘要）
        if !progressive.enabled || !is_progressive_triggered(&messages, progressive) {
            debug!(
                %session_id,
                msg_count = messages.len(),
                progressive_enabled = progressive.enabled,
                "渐进式摘要未触发，回退 v1.6 整会话摘要"
            );
            let l1 = self.summarize_session(session_id).await?;
            return Ok(vec![l1]);
        }

        // 3. 触发：按 tail_msg_count 切分为段（每段 ≤ tail 条，尾块覆盖最新对话）
        //    theta_gap 保持默认（10 分钟）：时间间隙大的消息也切分为独立段。
        let splitter_cfg = crate::utt::UttSplitterConfig {
            theta_gap_minutes: 10,
            max_msgs_per_block: progressive.tail_msg_count.max(1),
        };
        let target = self.config.persona_uid.as_deref();
        let chunks = crate::utt::splitter::split_messages(&messages, target, &splitter_cfg);

        // 无目标发言（如全会话只有用户消息）→ 回退整会话摘要（与 summarize_session 语义一致）
        if chunks.is_empty() {
            debug!(%session_id, "渐进式摘要切分为空，回退整会话摘要");
            let l1 = self.summarize_session(session_id).await?;
            return Ok(vec![l1]);
        }
        debug!(%session_id, block_count = chunks.len(), "渐进式摘要按段生成");

        // 4. 逐段生成（复用块级生成逻辑，块间注入上一块上文）
        let mut generated: Vec<MemoryL1> = Vec::with_capacity(chunks.len());
        let mut last_error: Option<RamariaError> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let prior_context = if i == 0 {
                None
            } else {
                Some(build_prior_context(
                    &chunks[i - 1],
                    generated.last(),
                    &self.config,
                    &self.config.user_prefix,
                    &self.config.assistant_prefix,
                ))
            };

            match self
                .generate_chunk_l1(session_id, chunk, prior_context.as_deref())
                .await
            {
                Ok((l1, keywords)) => {
                    // 段 L1 写库（absorbed=false 入候选池），供 L2 封存触发提取
                    self.storage.save_memory_l1(&l1).await.map_err(|e| {
                        warn!(%session_id, l1_id = %l1.id, error=%e, "渐进式段 L1 写库失败");
                        RamariaError::storage(format!(
                            "渐进式摘要：session {session_id} 段 L1 写库失败: {e}"
                        ))
                    })?;
                    self.write_back_keywords(session_id, &l1, &keywords).await;
                    debug!(%session_id, block_index = i, "渐进式段 {} L1 生成成功", i);
                    generated.push(l1);
                }
                Err(e) => {
                    warn!(%session_id, block_index = i, error=%e, "渐进式段 L1 生成失败（降级继续）");
                    last_error = Some(e);
                }
            }
        }

        // 5. 返回（全部段失败 → 返回最后一个错误，与 v1.4 语义一致）
        if generated.is_empty() {
            return Err(last_error.unwrap_or_else(|| {
                RamariaError::validation(format!("session {session_id} 全部渐进式段 L1 生成失败"))
            }));
        }
        info!(
            %session_id,
            total_blocks = chunks.len(),
            success_blocks = generated.len(),
            tail_msg_count = progressive.tail_msg_count,
            "渐进式摘要完成（按段生成 L1，段 L1 已入候选池）"
        );
        Ok(generated)
    }

    // =========================================================
    // 内部方法
    // =========================================================

    /// 将消息列表格式化为对话文本。
    ///
    /// 格式:
    /// - User 消息: `用户：{content}`
    /// - Assistant 消息: `助手：{content}`
    /// - System/Tool 消息: 跳过（不参与摘要）
    fn format_conversation(&self, messages: &[ramaria_core::types::Message]) -> String {
        format_messages(
            messages,
            &self.config.user_prefix,
            &self.config.assistant_prefix,
        )
    }

    /// 为单个话语块生成 L1（LLM 调用 + 解析 + 校验，不写库）。
    ///
    /// v1.5 B2：块 N 生成时注入上一块上文（`prior_context`），输出含 continuation。
    ///
    /// 参数:
    /// - `session_id`: 来源 session。
    /// - `chunk`: 当前块（其消息为对话原文）。
    /// - `prior_context`: 上一块的上文文本（None = 无上一块，v1.4 独立摘要路径）。
    ///
    /// 返回:
    /// - 校验后的 `(MemoryL1, 关键词列表)`（尚未写入存储，由调用方统一写库）。
    async fn generate_chunk_l1(
        &self,
        session_id: Uuid,
        chunk: &UttChunk,
        prior_context: Option<&str>,
    ) -> RamariaResult<(MemoryL1, Vec<KeywordToken>)> {
        // 1. 格式化当前块对话文本
        let conversation = self.format_conversation(&chunk.messages);

        // 2. 获取关键词候选
        let keyword_candidates = self.get_keyword_candidates().await;

        // 3. 构建 prompt（含上文注入时使用上下文感知模板）
        let prompt = build_l1_prompt(&conversation, keyword_candidates.as_deref(), prior_context);

        // 4. 调用 LLM
        let request_id = Uuid::new_v4();
        let llm_request = ChatRequest {
            system_prompt: String::new(),
            memory_context: None,
            history: vec![],
            user_message: prompt,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            request_id,
            template_version: crate::prompt::PROMPT_TEMPLATE_VERSION.to_string(),
        };

        let raw_response = self.llm.chat(&llm_request).await.map_err(|e| {
            warn!(%session_id, %request_id, block_msg_count = chunk.msg_count, error=%e, "L1 块 LLM 调用失败");
            RamariaError::llm(format!(
                "session {session_id} L1 摘要生成 LLM 调用失败: {e}"
            ))
        })?;

        debug!(%session_id, %request_id, "LLM 返回 {} 字符", raw_response.len());

        // 5. 解析 JSON
        let parsed = self.parse_summary_json(&raw_response)?;

        // 6. 校验并修正字段
        let (mut l1, keywords) = Self::validate_and_build(&parsed, session_id);

        // 注入 config 中的上下文字段
        l1.persona_uid = self.config.persona_uid.clone();
        l1.context_json = self.config.context_json.clone();
        // 优先使用 LLM 输出的 situation_strength，缺失时回退 config 默认值
        l1.situation_strength = parsed
            .situation_strength
            .or(self.config.situation_strength)
            .or(Some(3)); // 最终默认值：中性情境

        // 7. continuation 校验（三选一；无上一块时强制 None——即使 LLM 输出）
        l1.continuation = if prior_context.is_some() {
            validate_continuation(parsed.continuation.as_deref(), session_id)
        } else {
            None
        };

        debug!(
            %session_id,
            block_msg_count = chunk.msg_count,
            continuation = ?l1.continuation,
            has_prior = prior_context.is_some(),
            "L1 块校验完成"
        );

        // 返回（不写库），关键词随调用方统一处理
        Ok((l1, keywords))
    }

    /// 写回关键词词典 + 倒排索引（每块成功后调用，失败记 warn 不阻塞）。
    async fn write_back_keywords(
        &self,
        session_id: Uuid,
        l1: &MemoryL1,
        keywords: &[KeywordToken],
    ) {
        for kw_token in keywords {
            // 写回 keyword_pool
            if let Err(e) = self.storage.upsert_keyword(kw_token.as_str()).await {
                warn!(%session_id, keyword=%kw_token, error=%e, "关键词写回失败（非致命）");
            }
            // 写入 keyword_refs 倒排索引（L1 文档引用，doc_id 使用 UUID 字符串）
            if let Err(e) = self
                .storage
                .insert_keyword_ref(
                    kw_token.as_str(),
                    "l1",
                    &l1.id.to_string(),
                    l1.persona_uid.as_deref().unwrap_or(""),
                    1.0,
                )
                .await
            {
                warn!(%session_id, keyword=%kw_token, error=%e, "关键词引用写入失败（非致命）");
            }
        }
    }

    /// 获取关键词候选字符串。
    ///
    /// 策略:
    /// - 词典 ≤ 100 条: 全部返回
    /// - 词典 > 100 条: 仅返回前 50 条（已按 use_count 降序排列）
    /// - 词典为空: 返回 None
    async fn get_keyword_candidates(&self) -> Option<String> {
        let keywords = match self.storage.list_keywords().await {
            Ok(kws) => kws,
            Err(e) => {
                warn!(error=%e, "读取 keyword_pool 失败，跳过关键词注入");
                return None;
            }
        };

        if keywords.is_empty() {
            return None;
        }

        let selected = if keywords.len() <= KEYWORD_INJECT_THRESHOLD {
            keywords
        } else {
            keywords
                .into_iter()
                .take(KEYWORD_INJECT_LIMIT)
                .collect::<Vec<_>>()
        };

        Some(selected.join(", "))
    }

    /// 三步递进 JSON 解析。
    ///
    /// 步骤:
    /// 1. 直接 `serde_json::from_str`
    /// 2. 剥离 `<think>...</think>` 标签后重试
    /// 3. 正则提取首对 `{...}` 后解析
    ///
    /// 全部失败返回 Validation 错误（隐私红线：不包含原始响应内容，仅记长度供诊断）。
    fn parse_summary_json(&self, raw: &str) -> RamariaResult<L1SummaryResponse> {
        // 步骤 1: 直接解析
        if let Ok(parsed) = serde_json::from_str::<L1SummaryResponse>(raw) {
            return Ok(parsed);
        }

        // 步骤 2: 剥离 think 标签
        let stripped = utils::strip_thinking(raw);
        if stripped != raw
            && let Ok(parsed) = serde_json::from_str::<L1SummaryResponse>(&stripped)
        {
            debug!("剥离 think 标签后解析成功");
            return Ok(parsed);
        }

        // 步骤 3: 正则提取首对花括号
        if let Some(json_segment) = utils::extract_first_json_object(raw)
            && let Ok(parsed) = serde_json::from_str::<L1SummaryResponse>(&json_segment)
        {
            debug!("正则提取 JSON 对象后解析成功");
            return Ok(parsed);
        }

        // 全部失败
        // 隐私红线：LLM 原始响应不落日志，仅记录长度供诊断
        warn!(
            response_len = raw.chars().count(),
            "L1 摘要 JSON 解析全部失败（可能因 max_tokens 输出预算不足被截断）"
        );
        Err(RamariaError::validation(format!(
            "L1 摘要 JSON 解析失败，原始响应 {} 字符（不记录原文，防隐私泄漏）\
             （若响应不完整，可能是 max_tokens 输出预算不足导致截断）",
            raw.chars().count()
        )))
    }

    /// 校验 LLM 返回字段并构建 MemoryL1。
    ///
    /// 校验规则（与 Python v0.x 对齐）:
    /// - `summary`: 必填，为空时填降级文本
    /// - `time_period`: 严格六选一，非法值置 None
    /// - `atmosphere`: 四字以内，超长截断
    /// - `valence`: 五档钳制到最近的合法值
    /// - `salience`: 五档钳制到最近的合法值
    /// - `evidence_notes`: 新增，后处理校验（非空数组 + 每条 ≥ 5 字符），
    ///   校验失败不阻塞 L1 生成，降级为空数组并记 warn 日志
    ///
    /// 返回:
    /// - (MemoryL1, KeywordToken 列表)
    fn validate_and_build(
        parsed: &L1SummaryResponse,
        session_id: Uuid,
    ) -> (MemoryL1, Vec<KeywordToken>) {
        // summary: 必填降级
        let summary = parsed.summary.as_deref().unwrap_or("").trim().to_string();
        let summary = if summary.is_empty() {
            warn!(%session_id, "LLM 返回空 summary，使用降级文本");
            "（摘要生成失败，内容为空）".to_string()
        } else {
            summary
        };

        // keywords: 容许为空，拆分为列表
        let (keywords_str, keywords_list) = parse_keywords(parsed.keywords.as_deref());

        // time_period: 严格六选一
        let time_period = parsed
            .time_period
            .as_deref()
            .map(|s| s.trim().to_string())
            .filter(|s| {
                const VALID: &[&str] = &["清晨", "上午", "下午", "傍晚", "夜间", "深夜"];
                let ok = VALID.contains(&s.as_str());
                if !ok {
                    warn!(%session_id, time_period=%s, "非法的 time_period 值，置为 None");
                }
                ok
            });

        // atmosphere: 四字以内
        let atmosphere = parsed
            .atmosphere
            .as_deref()
            .map(|s| s.trim().to_string())
            .map(|s| {
                let truncated = ramaria_core::text::truncate_chars_bare(&s, 4);
                if truncated.len() != s.chars().count() {
                    debug!(%session_id, original=%s, truncated=%truncated, "atmosphere 超长截断");
                }
                truncated
            });

        // valence: 五档钳制
        let valence = utils::clamp_valence(parsed.valence.unwrap_or(0.0));
        if (valence - parsed.valence.unwrap_or(0.0)).abs() > f64::EPSILON {
            debug!(
                %session_id,
                original = parsed.valence.unwrap_or(0.0),
                clamped = valence,
                "valence 钳制到合法档位"
            );
        }

        // salience: 五档钳制
        let salience = utils::clamp_salience(parsed.salience.unwrap_or(0.5));
        if (salience - parsed.salience.unwrap_or(0.5)).abs() > f64::EPSILON {
            debug!(
                %session_id,
                original = parsed.salience.unwrap_or(0.5),
                clamped = salience,
                "salience 钳制到合法档位"
            );
        }

        // evidence_notes: 后处理校验
        // 规则：非空数组 + 每条 trim 后 ≥ 5 字符
        // 校验失败不阻塞 L1 生成，降级为空数组并记 warn 日志
        let evidence_notes = validate_evidence_notes(parsed.evidence_notes.clone(), session_id);

        // continuation: 三选一校验（v1.5 B2），非法值置 None 不阻塞
        let continuation = validate_continuation(parsed.continuation.as_deref(), session_id);

        let l1 = MemoryL1 {
            id: ramaria_core::types::new_id(),
            session_id,
            summary,
            keywords: keywords_str,
            time_period,
            atmosphere,
            valence,
            salience,
            absorbed: false,
            created_at: ramaria_core::types::now_ms(),
            last_accessed_at: None,
            persona_uid: None,        // 由调用方在 construct 阶段通过 config 注入
            context_json: None,       // 由调用方在 construct 阶段通过 config 注入
            situation_strength: None, // 由 LLM 输出或 config 注入
            evidence_notes: Some(evidence_notes), // 始终为 Some(vec![])，存储层存为 JSON 数组
            continuation,
        };

        (l1, keywords_list)
    }
}

// =========================================================
// evidence_notes 宽容反序列化 + 校验
// =========================================================/// 宽容反序列化 evidence_notes（v1.4 结构化升级的过渡兼容）。
///
/// 兼容三种输入:
/// 1. 对象数组 `[{"text": "...", "time": ..., "who": ..., "cause": ...}]` — 直接解析
/// 2. 旧字符串数组 `["...", "..."]` — 字符串落 `text` 槽位，其余置空
/// 3. 缺失 / null / 非数组 — 返回 None
///
/// 说明:
/// - 存储层（memory_l1 表）格式约定：只读写新格式，无旧格式解析分支（见 docs/dev-1.5/v1.5-decisions.md）；
///   此处的宽容解析仅针对 LLM 输出（M4 之前 prompt 仍为旧格式）。
fn deserialize_evidence_notes<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<EvidenceNote>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let items = match value {
        serde_json::Value::Array(items) => items,
        _ => return Ok(None),
    };

    let mut notes = Vec::with_capacity(items.len());
    for item in items {
        match item {
            // 旧格式：字符串 → text 槽位
            serde_json::Value::String(s) => notes.push(EvidenceNote::new(s)),
            // 新格式：对象 → 结构化解析（缺失字段回退 None）
            serde_json::Value::Object(_) => match serde_json::from_value::<EvidenceNote>(item) {
                Ok(note) => notes.push(note),
                Err(e) => {
                    tracing::warn!(error = %e, "evidence_notes 条目解析失败，跳过该条");
                }
            },
            other => {
                tracing::warn!(
                    kind = %other,
                    "evidence_notes 条目类型非法（应为字符串或对象），跳过该条"
                );
            }
        }
    }
    Ok(Some(notes))
}

mod helpers;

use helpers::*;

#[cfg(test)]
mod tests;
