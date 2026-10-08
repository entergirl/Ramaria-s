//! crates/ramaria-memory/src/l1/summarizer/pipeline.rs - L1 摘要编排入口
//!
//! 设计特点:
//! - summarize_session: 整会话按话语块生成，块 N 注入上一块上文（§6.3 混合形态）。
//! - summarize_progressive: 渐进式按段生成，未达阈值时回退 summarize_session。
//! - 图片描述注入：切块前一次加载会话内附件描述映射，对话文本中的
//!   `[图片#{hash}]` 占位符渲染为 `[图片: {描述}]`（无描述 / 查询失败保留占位符）。
//! - 多画像分发：fanout_others 开启时按块/段内他人发言者复制 L1 行，各行独立 persona。
//! - 块级容错：单块失败记 warn 并降级继续，全部失败返回最后一个错误。
//! - 写库失败为硬错误；关键词写回失败为非致命（记 warn 不阻塞）。

use std::collections::{BTreeSet, HashMap};

use ramaria_core::keyword::KeywordToken;
use ramaria_core::types::MessageRole;
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
    /// 关键词写回：
    /// - 写库前读取一次词池快照供全部块复用：未命中词池但与既有规范词相似的词条
    ///   以 `pending` 入池（待确认别名），其余写规范词；
    /// - 快照读取失败降级为空快照（全部按规范词写回），不阻塞写库。
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

        // 2. 附件描述（切块前一次加载；空映射时对话文本零变化）
        let descriptions = self.load_attachment_descriptions(&messages).await;

        // 3. 切分为话语块（上下文感知生成的块粒度）
        //    - 多画像分发开启 → 群聊口径切分（多发言者，全部块保留）
        //    - 否则配置了 utt_splitter → split_messages 切分（目标 persona 为 config.persona_uid）
        //    - 未配置 → 整会话一块
        //    - 切分结果为空（如纯用户消息块被丢弃）→ 回退整会话一块，
        //      保证至少产出一条摘要的语义一致。
        let chunks = match &self.config.utt_splitter {
            Some(splitter_cfg) if self.config.fanout_others => {
                let split =
                    crate::utt::group_splitter::split_messages_group(&messages, splitter_cfg);
                if split.is_empty() {
                    vec![UttChunk::from_messages(messages.clone())]
                } else {
                    split
                }
            }
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

        // 4. 逐块生成（内存收集，全部成功后统一写库）
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
                    &descriptions,
                ))
            };

            match self
                .generate_chunk_l1(session_id, chunk, prior_context.as_deref(), &descriptions)
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

        // 5. 统一写库（成功块）+ 写回关键词
        //    词池快照读取一次供全部块复用；读取失败降级为空快照（全部按规范词写回）
        let pool_rows = match self.storage.list_keyword_pool_entries().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(%session_id, error=%e, "词池快照读取失败，关键词写回降级为直接规范词");
                Vec::new()
            }
        };
        let mut saved_last: Option<MemoryL1> = None;
        for (idx, entry) in generated.iter().enumerate() {
            let Some((l1, keywords)) = entry else {
                continue;
            };
            // 多画像分发：参与者非空时按参与者复制多行落库，否则原行落库
            for row in self.fanout_rows(l1, &chunks[idx]) {
                self.storage.save_memory_l1(&row).await.map_err(|e| {
                    warn!(%session_id, l1_id = %row.id, error=%e, "写入 memory_l1 失败");
                    RamariaError::storage(format!("写入 session {session_id} L1 摘要失败: {e}"))
                })?;
                self.write_back_keywords(session_id, &row, keywords, &pool_rows)
                    .await;
                saved_last = Some(row);
            }
        }

        // 6. 返回
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
    /// - 多画像分发开启时（`fanout_others`），每段按其内他人发言者复制 L1 行，
    ///   返回值与写库内容同为实际落库行集。
    /// - 每段 L1 生成后写回关键词词典 + 倒排索引（与 `summarize_session` 一致）；
    ///   逐段写库前读取一次词池快照供全部段复用，读取失败降级为空快照
    ///   （全部按规范词写回）。
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
    /// - 成功时返回本次实际落库的全部 L1 行（多画像分发开启时含复制行；
    ///   触发时 ≥1 条，未触发时 1 条）。
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

        // 3. 触发：附件描述加载 + 按 tail_msg_count 切分为段（每段 ≤ tail 条，尾块覆盖最新对话）
        //    theta_gap 保持默认（10 分钟）：时间间隙大的消息也切分为独立段。
        //    多画像分发开启 → 群聊口径切分（多发言者，全部块保留）；
        //    否则按目标 persona（`config.persona_uid`）切分。
        let descriptions = self.load_attachment_descriptions(&messages).await;
        let splitter_cfg = crate::utt::UttSplitterConfig {
            theta_gap_minutes: 10,
            max_msgs_per_block: progressive.tail_msg_count.max(1),
        };
        let chunks = if self.config.fanout_others {
            crate::utt::group_splitter::split_messages_group(&messages, &splitter_cfg)
        } else {
            let target = self.config.persona_uid.as_deref();
            crate::utt::splitter::split_messages(&messages, target, &splitter_cfg)
        };

        // 切分为空（如无目标发言）→ 回退整会话摘要（与 summarize_session 语义一致）
        if chunks.is_empty() {
            debug!(%session_id, "渐进式摘要切分为空，回退整会话摘要");
            let l1 = self.summarize_session(session_id).await?;
            return Ok(vec![l1]);
        }
        debug!(%session_id, block_count = chunks.len(), "渐进式摘要按段生成");

        // 4. 逐段生成（复用块级生成逻辑，块间注入上一块上文）
        //    词池快照读取一次供全部段复用；读取失败降级为空快照（全部按规范词写回）
        let pool_rows = match self.storage.list_keyword_pool_entries().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(%session_id, error=%e, "词池快照读取失败，关键词写回降级为直接规范词");
                Vec::new()
            }
        };
        // generated = 实际落库行（多画像分发开启时含全部复制行；关闭时每段 1 行）
        let mut generated: Vec<MemoryL1> = Vec::with_capacity(chunks.len());
        // 上一成功段的代表行（供下一段上文注入；与分发行的摘要内容一致）
        let mut last_segment_l1: Option<MemoryL1> = None;
        let mut last_error: Option<RamariaError> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let prior_context = if i == 0 {
                None
            } else {
                Some(build_prior_context(
                    &chunks[i - 1],
                    last_segment_l1.as_ref(),
                    &self.config,
                    &self.config.user_prefix,
                    &self.config.assistant_prefix,
                    &descriptions,
                ))
            };

            match self
                .generate_chunk_l1(session_id, chunk, prior_context.as_deref(), &descriptions)
                .await
            {
                Ok((l1, keywords)) => {
                    // 多画像分发：参与者非空时按参与者复制多行落库，否则原行落库；
                    // 段 L1 写库（absorbed=false 入候选池），供 L2 封存触发提取
                    let rows = self.fanout_rows(&l1, chunk);
                    for row in &rows {
                        self.storage.save_memory_l1(row).await.map_err(|e| {
                            warn!(%session_id, l1_id = %row.id, error=%e, "渐进式段 L1 写库失败");
                            RamariaError::storage(format!(
                                "渐进式摘要：session {session_id} 段 L1 写库失败: {e}"
                            ))
                        })?;
                        self.write_back_keywords(session_id, row, &keywords, &pool_rows)
                            .await;
                    }
                    debug!(
                        %session_id,
                        block_index = i,
                        row_count = rows.len(),
                        "渐进式段 {} L1 生成成功",
                        i
                    );
                    last_segment_l1 = Some(l1);
                    generated.extend(rows);
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
            saved_rows = generated.len(),
            tail_msg_count = progressive.tail_msg_count,
            "渐进式摘要完成（按段生成 L1，段 L1 已入候选池）"
        );
        Ok(generated)
    }

    // =========================================================
    // 多画像分发（内部辅助）
    // =========================================================

    /// 按块内他人发言者复制 L1 行（多画像分发）。
    ///
    /// 规则:
    /// - `fanout_others=false` → 原行（1 行）；
    /// - 参与者 = 块内 Assistant 角色且 persona_uid 非空的消息发送者集合
    ///   （去重、稳定序；self 的 User 消息天然排除）；
    /// - 参与者为空（无他人参与）→ 原行保留（persona_uid 为 config 值，导入场景即 NULL）；
    /// - 参与者非空 → 每参与者复制一行（新 id、各自 persona_uid），原行不落库。
    ///
    /// 参数:
    /// - `l1`: 块/段生成结果（复制模板）。
    /// - `chunk`: 生成该 L1 的话语块（读取发言者）。
    ///
    /// 返回:
    /// - 待落库的 L1 行列表（各行摘要内容一致，仅 id 与 persona_uid 不同）。
    fn fanout_rows(&self, l1: &MemoryL1, chunk: &UttChunk) -> Vec<MemoryL1> {
        if !self.config.fanout_others {
            return vec![l1.clone()];
        }

        let participants: BTreeSet<&str> = chunk
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant)
            .filter_map(|m| m.persona_uid.as_deref())
            .collect();

        if participants.is_empty() {
            // 无他人参与（纯 self / 无他人发言者）→ 原行保留
            return vec![l1.clone()];
        }

        participants
            .into_iter()
            .map(|uid| {
                let mut row = l1.clone();
                row.id = ramaria_core::types::new_id();
                row.persona_uid = Some(uid.to_string());
                row
            })
            .collect()
    }

    // =========================================================
    // 附件描述加载（内部辅助）
    // =========================================================

    /// 加载会话消息的附件描述映射（`[图片#{hash}]` → 描述）。
    ///
    /// 说明:
    /// - 切块前一次加载全部会话消息的附件行，供对话文本与上文注入渲染；
    /// - 查询失败记 warn 并返回空映射（对话文本保留占位符原文，L1 不阻塞）。
    async fn load_attachment_descriptions(
        &self,
        messages: &[ramaria_core::types::Message],
    ) -> HashMap<String, String> {
        let message_ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();
        if message_ids.is_empty() {
            return HashMap::new();
        }
        match self
            .storage
            .list_attachments_by_messages(&message_ids)
            .await
        {
            Ok(rows) => ramaria_core::types::build_render_map(&rows),
            Err(e) => {
                warn!(error = %e, "L1 摘要附件查询失败，对话文本保留占位符原文");
                HashMap::new()
            }
        }
    }
}
