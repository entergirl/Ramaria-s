//! crates/ramaria-memory/src/l1/summarizer/pipeline.rs - L1 摘要编排入口
//!
//! 设计特点:
//! - summarize_session: 整会话按话语块生成，块 N 注入上一块上文（§6.3 混合形态）。
//! - summarize_progressive: 渐进式按段生成，未达阈值时回退 summarize_session。
//! - 图片描述注入：切块前一次加载会话内附件描述映射，对话文本中的
//!   `[图片#{hash}]` 占位符渲染为 `[图片: {描述}]`（无描述 / 查询失败保留占位符）。
//! - 多画像分发：fanout_others 开启时按块/段内参与者复制 L1 行，各行独立 persona；
//!   `fanout_include_self` 控制参与者是否含"我"（User 角色发言者）。
//! - 目标相对投影：target_projection 开启时按块内目标集合（绑定人格 ∪ 块内发言者）
//!   逐目标生成 L1 行（各行角色重投影、persona 定向），不做行复制分发。
//! - 块级容错：单块失败记 warn 并降级继续，全部失败返回最后一个错误。
//! - 写库失败为硬错误；关键词写回失败为非致命（记 warn 不阻塞）。

use ramaria_core::keyword::KeywordToken;
use ramaria_core::{LlmProviderTrait, MemoryL1, RamariaError, RamariaResult, StorageBackend};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::utt::UttChunk;

use super::build_prior_context;
use super::config::L1SummarizerConfig;

// =========================================================
// 常量
// =========================================================

/// 生成行：`(投影目标, 摘要, 关键词列表)`（非投影行为 `None` 目标）。
type GeneratedRow = (Option<String>, MemoryL1, Vec<KeywordToken>);

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
    /// 目标相对投影（`target_projection` 开启，社会通道会话）：
    /// - 切分走群聊口径（多发言者，全部块保留），每块按目标集合逐目标生成一行；
    /// - 逐目标行 persona 定向该目标，不做行复制分发；块间上文不注入上一 L1
    ///   （多目标下无语义，长块走原文截断降级）。
    ///
    /// 块级容错：
    /// - 某块 / 某目标 LLM 调用或解析失败 → 记 warn、该目标不产出 L1，
    ///   同块其余目标与后续块照常生成（不阻塞整体）。
    /// - 全部块零行 → 返回最后一个错误（与 v1.4 失败语义一致）。
    /// - 成功行统一写库；写库失败为硬错误直接返回。
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
        //    - 多画像分发 / 目标相对投影 → 群聊口径切分（多发言者，全部块保留）
        //    - 否则配置了 utt_splitter → split_messages 切分（目标 persona 为 config.persona_uid）
        //    - 未配置 → 整会话一块
        //    - 切分结果为空（如纯用户消息块被丢弃）→ 回退整会话一块，
        //      保证至少产出一条摘要的语义一致。
        let chunks = match &self.config.utt_splitter {
            Some(splitter_cfg) if self.config.fanout_others || self.config.target_projection => {
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

        // 4. 逐块 × 逐目标生成（内存收集，全部成功后统一写库）
        //    - generated[i] = 块 i 的生成行列表（每项 (target, L1, 关键词列表)；
        //      非投影时每块至多 1 项且 target=None；空列表 = 该块全部失败，降级）
        //    - 非投影：块 i 的上文来自块 i-1 的生成结果（内存传递，只注入最近 1 块，不链式）
        //    - 投影：多目标下「上一块的哪一行」无语义，prev_l1 传 None——
        //      长块走既有降级路径（注入上一块原文并按 prior_context_max_chars 截断）
        let mut generated: Vec<Vec<GeneratedRow>> = Vec::with_capacity(chunks.len());
        let mut last_error: Option<RamariaError> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            // 块级目标集合：投影 → 逐目标；非投影 → 单目标（None）
            let mut targets: Vec<Option<String>> = if self.config.target_projection {
                self.projection_targets(chunk)
                    .into_iter()
                    .map(Some)
                    .collect()
            } else {
                vec![None]
            };
            if targets.is_empty() {
                // 空块防御：目标集合为空时按非投影单目标生成一次，绝不出现零次生成
                targets.push(None);
            }

            let mut rows: Vec<GeneratedRow> = Vec::new();
            for target in targets {
                // 构建上一块上文（混合形态，§6.3）：
                // - 上一块消息数 ≤ prior_context_threshold → 注入 L0 原文
                // - 长块 → 注入上一 L1 摘要 + 结构化线索（上一 L1 缺失 → 原文截断）
                let prior_context = if i == 0 {
                    None
                } else {
                    let prev_l1 = if self.config.target_projection {
                        None
                    } else {
                        generated[i - 1].first().map(|(_, l1, _)| l1)
                    };
                    Some(build_prior_context(
                        &chunks[i - 1],
                        prev_l1,
                        &self.config,
                        &self.config.user_prefix,
                        &self.config.assistant_prefix,
                        &descriptions,
                        target.as_deref(),
                    ))
                };

                match self
                    .generate_chunk_l1(
                        session_id,
                        chunk,
                        prior_context.as_deref(),
                        &descriptions,
                        target.as_deref(),
                    )
                    .await
                {
                    Ok((l1, keywords)) => {
                        debug!(
                            %session_id,
                            block_index = i,
                            target = target.as_deref().unwrap_or("none"),
                            "块 {} L1 生成成功",
                            i
                        );
                        rows.push((target, l1, keywords));
                    }
                    Err(e) => {
                        // 目标级降级：不阻塞同块其余目标与后续块，块内全部失败时该块无行
                        warn!(
                            %session_id,
                            block_index = i,
                            target = target.as_deref().unwrap_or("none"),
                            error=%e,
                            "L1 块目标生成失败，该目标无摘要（降级继续）"
                        );
                        last_error = Some(e);
                    }
                }
            }
            generated.push(rows);
        }

        // 5. 统一写库（成功行）+ 写回关键词
        //    词池快照读取一次供全部块复用；读取失败降级为空快照（全部按规范词写回）
        let pool_rows = match self.storage.list_keyword_pool_entries().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(%session_id, error=%e, "词池快照读取失败，关键词写回降级为直接规范词");
                Vec::new()
            }
        };
        let mut saved_last: Option<MemoryL1> = None;
        let mut saved_rows = 0usize;
        for (idx, rows) in generated.iter().enumerate() {
            for (_target, l1, keywords) in rows {
                if self.config.target_projection {
                    // 投影：每行独立生成、persona 已定向，直接落库（不做行复制分发）
                    self.storage.save_memory_l1(l1).await.map_err(|e| {
                        warn!(%session_id, l1_id = %l1.id, error=%e, "写入 memory_l1 失败");
                        RamariaError::storage(format!("写入 session {session_id} L1 摘要失败: {e}"))
                    })?;
                    self.write_back_keywords(session_id, keywords, &pool_rows)
                        .await;
                    saved_last = Some(l1.clone());
                    saved_rows += 1;
                } else {
                    // 多画像分发：参与者非空时按参与者复制多行落库，否则原行落库
                    for row in self.fanout_rows(l1, &chunks[idx]) {
                        self.storage.save_memory_l1(&row).await.map_err(|e| {
                            warn!(%session_id, l1_id = %row.id, error=%e, "写入 memory_l1 失败");
                            RamariaError::storage(format!(
                                "写入 session {session_id} L1 摘要失败: {e}"
                            ))
                        })?;
                        self.write_back_keywords(session_id, keywords, &pool_rows)
                            .await;
                        saved_last = Some(row);
                        saved_rows += 1;
                    }
                }
            }
        }

        // 6. 返回
        match saved_last {
            Some(l1) => {
                info!(
                    %session_id,
                    l1_id = %l1.id,
                    total_blocks = chunks.len(),
                    success_blocks = generated.iter().filter(|rows| !rows.is_empty()).count(),
                    saved_rows,
                    "L1 摘要生成完成（按块）"
                );
                Ok(l1)
            }
            None => Err(last_error.unwrap_or_else(|| {
                RamariaError::validation(format!("session {session_id} 全部 L1 块生成失败"))
            })),
        }
    }
}

mod helpers;
mod progressive;
