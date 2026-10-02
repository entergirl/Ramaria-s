//! crates/ramaria-memory/src/l1/summarizer/pipeline.rs - L1 摘要编排入口
//!
//! 设计特点:
//! - summarize_session: 整会话按话语块生成，块 N 注入上一块上文（§6.3 混合形态）。
//! - summarize_progressive: 渐进式按段生成，未达阈值时回退 summarize_session。
//! - 块级容错：单块失败记 warn 并降级继续，全部失败返回最后一个错误。
//! - 写库失败为硬错误；关键词写回失败为非致命（记 warn 不阻塞）。

use ramaria_core::keyword::KeywordToken;
use ramaria_core::{LlmProviderTrait, MemoryL1, RamariaError, RamariaResult, StorageBackend};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::utt::UttChunk;

use super::config::L1SummarizerConfig;
use super::{build_prior_context, is_progressive_triggered};

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
    pub(super) config: L1SummarizerConfig,
    pub(super) llm: &'a dyn LlmProviderTrait,
    pub(super) storage: &'a dyn StorageBackend,
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
    /// - 最早/最晚消息时间跨度 > `span_hours`（默认 24 小时；
    ///   按 `created_at` 极值计算，不依赖消息排序——storage 通常返回升序，
    ///   导入等路径乱序时跨度判定仍确定）。
    ///
    /// 触发行为:
    /// - 按 `tail_msg_count`（默认 60）切段、全段生成：每段独立生成 L1（不跨段混合），
    ///   尾段覆盖最新对话。
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
}
