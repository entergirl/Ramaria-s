//! crates/ramaria-memory/src/l1/summarizer/pipeline/progressive.rs - 渐进式 L1 摘要
//!
//! 设计特点:
//! - 触发判断：未启用或未达阈值时回退整会话摘要
//! - 按 tail_msg_count 切段，逐段 × 逐目标生成，段间只注入上一段上文
//! - 段级降级：单段单目标失败不阻塞后续段，全部失败返回最后一个错误

use ramaria_core::{MemoryL1, RamariaError, RamariaResult};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::super::{L1Summarizer, build_prior_context, is_progressive_triggered};

impl<'a> L1Summarizer<'a> {
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
    /// - 目标相对投影开启时（`target_projection`），每段按目标集合逐目标生成行
    ///   （persona 定向，不做行复制分发）；段间不注入上一段 L1（prev_l1 传 None）。
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
        //    多画像分发 / 目标相对投影 → 群聊口径切分（多发言者，全部块保留）；
        //    否则按目标 persona（`config.persona_uid`）切分。
        let descriptions = self.load_attachment_descriptions(&messages).await;
        let splitter_cfg = crate::utt::UttSplitterConfig {
            theta_gap_minutes: 10,
            max_msgs_per_block: progressive.tail_msg_count.max(1),
        };
        let chunks = if self.config.fanout_others || self.config.target_projection {
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

        // 4. 逐段 × 逐目标生成（复用块级生成逻辑，非投影时块间注入上一段上文）
        //    词池快照读取一次供全部段复用；读取失败降级为空快照（全部按规范词写回）
        let pool_rows = match self.storage.list_keyword_pool_entries().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(%session_id, error=%e, "词池快照读取失败，关键词写回降级为直接规范词");
                Vec::new()
            }
        };
        // generated = 实际落库行（多画像分发开启时含全部复制行；投影开启时含逐目标行）
        let mut generated: Vec<MemoryL1> = Vec::with_capacity(chunks.len());
        // 上一成功段的代表行（供下一段上文注入；投影模式不使用，prev_l1 传 None）
        let mut last_segment_l1: Option<MemoryL1> = None;
        let mut last_error: Option<RamariaError> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            // 段级目标集合：投影 → 逐目标；非投影 → 单目标（None）
            let mut targets: Vec<Option<String>> = if self.config.target_projection {
                self.projection_targets(chunk)
                    .into_iter()
                    .map(Some)
                    .collect()
            } else {
                vec![None]
            };
            if targets.is_empty() {
                // 空段防御：目标集合为空时按非投影单目标生成一次，绝不出现零次生成
                targets.push(None);
            }

            for target in targets {
                let prior_context = if i == 0 {
                    None
                } else {
                    let prev_l1 = if self.config.target_projection {
                        None
                    } else {
                        last_segment_l1.as_ref()
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
                        // 投影：每行独立生成、persona 已定向，直接落库（不做行复制分发）；
                        // 非投影：多画像分发开启时按参与者复制多行落库，否则原行落库；
                        // 段 L1 写库（absorbed=false 入候选池），供 L2 封存触发提取
                        let rows: Vec<MemoryL1> = if self.config.target_projection {
                            vec![l1.clone()]
                        } else {
                            self.fanout_rows(&l1, chunk)
                        };
                        for row in &rows {
                            self.storage.save_memory_l1(row).await.map_err(|e| {
                                warn!(%session_id, l1_id = %row.id, error=%e, "渐进式段 L1 写库失败");
                                RamariaError::storage(format!(
                                    "渐进式摘要：session {session_id} 段 L1 写库失败: {e}"
                                ))
                            })?;
                            self.write_back_keywords(session_id, &keywords, &pool_rows)
                                .await;
                        }
                        debug!(
                            %session_id,
                            block_index = i,
                            target = target.as_deref().unwrap_or("none"),
                            row_count = rows.len(),
                            "渐进式段 {} L1 生成成功",
                            i
                        );
                        if !self.config.target_projection {
                            last_segment_l1 = Some(l1);
                        }
                        generated.extend(rows);
                    }
                    Err(e) => {
                        // 目标级降级：不阻塞同段其余目标与后续段
                        warn!(
                            %session_id,
                            block_index = i,
                            target = target.as_deref().unwrap_or("none"),
                            error=%e,
                            "渐进式段目标 L1 生成失败（降级继续）"
                        );
                        last_error = Some(e);
                    }
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
}
