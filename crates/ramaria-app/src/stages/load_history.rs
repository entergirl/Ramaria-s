//! crates/ramaria-app/src/stages/load_history.rs - Stage 4: 加载历史消息 + L1 上下文
//!
//! 设计特点:
//! - 对应 send_message 管线 Step 4 + Step 4.5
//! - 按 token 预算倒序分页加载消息（每页 20 条），避免长会话全量内存加载
//! - 将 Message 转换为 ChatMessage 格式供后续 TokenBudget / BuildRequest 使用
//! - 预加载近期 L1 摘要（跨 session 上下文注入），无条件注入 Block C1
//! - 格式化 L1 摘要为可读文本行
//! - 提取最后活跃时间字符串
//! - 空 session 不报错（新对话无历史消息为正常场景）

use async_trait::async_trait;
use ramaria_core::traits::ChatMessage;
use ramaria_core::types::MemoryL1;
use ramaria_memory::decay::DecayConfig;
use ramaria_memory::retriever::SearchResult;

use crate::pipeline::{PipelineContext, PipelineData, PipelineError, PipelineStage};

/// Stage 4: 加载历史消息 + 近期 L1 摘要。
///
/// 职责:
/// - 读取 PipelineData.session（由 Stage 3 设置），按 token 预算倒序分页加载消息
/// - 每页 20 条，从最新消息倒序加载，直到达到消息上限或 token 预算（粗糙字符估算）
/// - 将 Message 转换为 ChatMessage 格式供后续 TokenBudget / BuildRequest 使用
/// - 按 persona_uid 预加载近期 3 条 L1 摘要（跨 session 上下文）
/// - 将 L1 摘要格式化为上下文文本行
/// - 从最近 L1 提取最后活跃时间字符串
///
/// 分页加载替代全量加载。
/// - 每页 20 条，从最新消息倒序加载
/// - 达到消息数量上限（200 条）或粗略字符预算时停止
/// - 加载完成后反转为时间正序供后续 Stage 使用
///
/// 降级策略:
/// - L1 加载失败 → warn 日志 + 空列表（不阻塞对话）
/// - 空 session（无消息）→ 空历史列表，正常继续
/// - list_messages_paginated 不可用 → 回退到 list_messages（通过 trait 默认实现）
///
/// 安全约束:
/// - 消息原文只在内存中，不记日志
/// - L1 摘要最多截断前 120 字符到 DEBUG 日志
pub struct StageLoadHistory;

/// 每页加载的消息数。
const HISTORY_PAGE_SIZE: i64 = 20;

/// 最大加载消息数量（安全上限，防止内存无限增长）。
const HISTORY_MAX_MESSAGES: i64 = 200;

/// 粗略字符预算上限（消息内容 + role 标记的总字符数）。
/// 约对应 4K token 上下文（中文 ~1.5 chars/token）。
const HISTORY_CHAR_BUDGET: usize = 6000;

impl StageLoadHistory {
    /// 创建 StageLoadHistory 实例。
    pub fn new() -> Self {
        Self
    }
}

impl Default for StageLoadHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PipelineStage for StageLoadHistory {
    type Input = PipelineData;
    type Output = PipelineData;

    fn name(&self) -> &'static str {
        "LoadHistory"
    }

    /// 执行历史消息加载和 L1 上下文预加载。
    ///
    /// 参数:
    /// - `ctx`: 共享管线上下文（读取 storage）。
    /// - `input`: 管线数据，读取 `session` 和 `persona_uid` 字段。
    ///
    /// 返回:
    /// - `Ok(data)`: 加载成功，`history_messages`、`recent_summaries`、`last_active_at` 已填充。
    /// - `Err(Fatal)`: `session` 为 None（Stage 3 未执行或失败）。
    async fn execute(
        &self,
        ctx: &PipelineContext,
        mut input: Self::Input,
    ) -> Result<Self::Output, PipelineError> {
        let session_id = input
            .session
            .as_ref()
            .ok_or_else(|| {
                PipelineError::fatal(
                    "LoadHistory",
                    ramaria_core::error::RamariaError::validation(
                        "PipelineData.session 未设置——Stage 3 ResolveSession 必须先执行",
                    ),
                )
            })?
            .id;

        // ---- Step 4: 按 token 预算倒序分页加载历史消息 ----
        // 从最新消息倒序加载，每页 20 条，直到达到安全上限或粗略字符预算
        let mut all_messages_reversed: Vec<ramaria_core::types::Message> = Vec::new();
        let mut total_chars: usize = 0;
        let mut offset: i64 = 0;

        loop {
            // 检查是否已超过上限
            if all_messages_reversed.len() as i64 >= HISTORY_MAX_MESSAGES {
                tracing::debug!(
                    session_id = %session_id,
                    loaded = all_messages_reversed.len(),
                    max = HISTORY_MAX_MESSAGES,
                    "历史消息已达到加载上限"
                );
                break;
            }
            if total_chars >= HISTORY_CHAR_BUDGET {
                tracing::debug!(
                    session_id = %session_id,
                    total_chars,
                    budget = HISTORY_CHAR_BUDGET,
                    "历史消息已达到字符预算"
                );
                break;
            }

            let page = ctx
                .storage
                .list_messages_paginated(session_id, HISTORY_PAGE_SIZE, offset)
                .await
                .map_err(|e| {
                    PipelineError::fatal(
                        "LoadHistory",
                        ramaria_core::error::RamariaError::storage_with_source(
                            format!("分页加载 session {session_id} 的消息失败"),
                            e,
                        ),
                    )
                })?;

            if page.is_empty() {
                break; // 没有更多消息
            }

            let page_len = page.len() as i64;

            // 累计字符数（粗略估算：role 名称 + content）
            for msg in &page {
                total_chars += msg.content.chars().count() + 16; // 16 ≈ role 标记开销
            }

            all_messages_reversed.extend(page);
            offset += HISTORY_PAGE_SIZE;

            // 如果返回的页不满，说明已是最后一批
            if page_len < HISTORY_PAGE_SIZE {
                break;
            }
        }

        // 按 created_at 升序排列（时间正序），替代简单 reverse()
        // 原因: list_messages_paginated 返回 DESC，reverse() 在相同时间戳时可能错误翻转
        all_messages_reversed.sort_by_key(|m| m.created_at);

        let history_messages: Vec<ChatMessage> = all_messages_reversed
            .iter()
            .map(|m| ChatMessage {
                role: m.role,
                content: m.content.clone(),
            })
            .collect();

        tracing::debug!(
            session_id = %session_id,
            message_count = history_messages.len(),
            total_chars,
            "历史消息已加载"
        );

        input.history_messages = history_messages;

        // ---- Step 4.5: 预加载近期 L1 摘要（跨 session 上下文注入） ----
        // 不依赖关键词匹配——近期摘要无条件注入 System Prompt Block C1。
        // 解决新 session 发"你好"时 LLM 完全不知道上次聊了什么的问题。
        //
        // v1.7 B4（决策 D-V17-006）：脉络加权注入——开启 `[retrieval] narrative_weighted`
        // 时，以当前用户消息为话题依据，按"时间（衰减 × 访问加成）× 话题相关性"融合排序，
        // 使"刚聊过 / 相关的话题"优先进入脉络；关闭或检索无结果时回退 v1.6
        // "无条件取最近 N 条"（`list_recent_l1_by_persona`）。
        //
        // 探针消融（脉络闸门，F4/B0/B1/S_*）：`ctx.config.injection.narrative=false`
        // 时整段跳过（不查询 retriever/storage），recent_summaries/last_active_at 为空。
        let actual_uid = input.persona_uid.as_deref().unwrap_or("rama-0001");
        let narrative_top_k = ctx.config.retrieval.narrative_top_k.max(1);
        let recent_l1 = if !ctx.config.injection.narrative {
            tracing::debug!(
                persona_uid = actual_uid,
                "脉络注入闸门关闭（探针消融），跳过近期 L1 摘要加载"
            );
            Vec::new()
        } else if ctx.config.retrieval.narrative_weighted {
            let now = ramaria_core::types::now_ms();
            let decay_config = DecayConfig::from_core(&ctx.config.decay, "l1");
            let narrative_results = match ctx.retriever.read() {
                Ok(retriever) => retriever.search_narrative(
                    &input.user_input,
                    actual_uid,
                    narrative_top_k as usize,
                    now,
                    &decay_config,
                ),
                Err(e) => {
                    tracing::error!(error = %e, "Retriever lock poisoned during narrative search");
                    Vec::new()
                }
            };
            if !narrative_results.is_empty() {
                // 加权命中 → 转回 MemoryL1（脉络行格式与 v1.6 一致，缺 time_period/atmosphere
                // 时显示纯摘要——加权优先保证话题相关性，展示次要）
                narrative_results
                    .iter()
                    .filter_map(search_result_to_memory_l1)
                    .collect::<Vec<MemoryL1>>()
            } else {
                // 检索无结果（无 L1 或 query 无相关性命中）→ 回退最近 N 条（v1.6 语义）
                ctx.storage
                    .list_recent_l1_by_persona(actual_uid, narrative_top_k)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            persona_uid = actual_uid,
                            error = %e,
                            "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                        );
                        Vec::new()
                    })
            }
        } else {
            ctx.storage
                .list_recent_l1_by_persona(actual_uid, narrative_top_k)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        persona_uid = actual_uid,
                        error = %e,
                        "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                    );
                    Vec::new()
                })
        };

        // 格式化近期摘要为可读文本行
        let recent_summaries: Vec<String> =
            recent_l1.iter().map(format_l1_as_context_line).collect();

        // 从最近一条 L1 的创建时间提取最后活跃时间
        let last_active_at: Option<String> = recent_l1.first().map(|l1| {
            let secs = l1.created_at / 1000;
            match chrono::DateTime::from_timestamp(secs, 0) {
                Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
                None => String::new(),
            }
        });

        tracing::debug!(
            persona_uid = actual_uid,
            l1_count = recent_l1.len(),
            has_last_active = last_active_at.is_some(),
            "近期 L1 摘要已加载"
        );

        input.recent_summaries = recent_summaries;
        input.last_active_at = last_active_at;

        Ok(input)
    }
}

// =========================================================
// 辅助函数: 脉络加权结果转换（v1.7 B4）
// =========================================================

/// 将脉络加权检索的 SearchResult 转换为 MemoryL1（供脉络行格式化）。
///
/// 说明:
/// - 仅接受 L1 层结果（DocId::L1）；其他层（L2/图谱）不是脉络注入目标。
/// - `time_period` / `atmosphere` 在 SearchResult 中不承载，置 None——
///   脉络行退化为纯摘要格式（加权路径优先保证话题相关性，展示次要）。
/// - `session_id` 置 nil：脉络行只用于上下文文本展示，不参与会话归属。
fn search_result_to_memory_l1(sr: &SearchResult) -> Option<MemoryL1> {
    let id = match &sr.doc_id {
        ramaria_memory::bm25::DocId::L1(id) => *id,
        _ => return None,
    };
    Some(MemoryL1 {
        id,
        session_id: uuid::Uuid::nil(),
        summary: sr.doc_summary.clone(),
        keywords: None,
        time_period: None,
        atmosphere: None,
        valence: 0.0,
        salience: 0.5,
        absorbed: false,
        created_at: sr.created_at,
        last_accessed_at: sr.last_accessed_at,
        persona_uid: sr.persona_uid.clone(),
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    })
}

// =========================================================
// 辅助函数: L1 上下文格式化
// =========================================================

/// 将 MemoryL1 格式化为一行上下文文本，供 Block C1 [近期对话摘要] 使用。
///
/// 格式:
/// - 含时间段: "上午 — 讨论了Python异步编程的线程安全问题。氛围融洽。"
/// - 无时间段: "讨论了Python异步编程的线程安全问题。氛围融洽。"
///
/// 截断规则:
/// - 单条摘要最多 120 字符，超出加省略号。
///
/// 安全约束:
/// - 不记录完整摘要到 INFO 日志。
fn format_l1_as_context_line(l1: &MemoryL1) -> String {
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

    // 截断到 120 字符（统一字符边界工具，预算内含省略号）
    ramaria_core::text::truncate_chars(&base, 120)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::test_utils::{MockLlm, MockStorage, test_context};
    use ramaria_core::types::{AppState, MemoryL1, Message, MessageRole, MessageSource};
    use std::sync::Arc;

    fn make_data(session: Option<ramaria_core::types::Session>) -> PipelineData {
        let mut data = PipelineData::new(
            "test".into(),
            Some("rama-0001".into()),
            None,
            uuid::Uuid::new_v4(),
        )
        .with_app_state(AppState::Ready);
        data.session = session;
        data
    }

    #[tokio::test]
    async fn loads_history_messages() {
        let storage = Arc::new(MockStorage::new());
        let session_id = uuid::Uuid::new_v4();
        storage.add_active_session(session_id);
        storage.add_messages(
            session_id,
            vec![
                Message::new(
                    session_id,
                    MessageRole::User,
                    "你好".into(),
                    MessageSource::Local,
                ),
                Message::new(
                    session_id,
                    MessageRole::Assistant,
                    "你好！".into(),
                    MessageSource::Online,
                ),
            ],
        );

        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageLoadHistory::new();
        let data = make_data(Some(ramaria_core::types::Session {
            id: session_id,
            started_at: 1000,
            ended_at: None,
            persona_uid: None,
        }));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("should load messages");
        assert_eq!(output.history_messages.len(), 2);
        assert_eq!(output.history_messages[0].content, "你好");
        assert_eq!(output.history_messages[1].content, "你好！");
    }

    #[tokio::test]
    async fn empty_session_returns_empty_history() {
        let storage = Arc::new(MockStorage::new());
        let session_id = uuid::Uuid::new_v4();

        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageLoadHistory::new();
        let data = make_data(Some(ramaria_core::types::Session {
            id: session_id,
            started_at: 1000,
            ended_at: None,
            persona_uid: None,
        }));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("empty session should succeed");
        assert!(output.history_messages.is_empty());
    }

    #[tokio::test]
    async fn loads_recent_l1_summaries() {
        let storage = Arc::new(MockStorage::new());
        let session_id = uuid::Uuid::new_v4();

        let mut l1 = MemoryL1::new(session_id, "讨论了编程话题".into(), Some("下午".into()));
        l1.atmosphere = Some("融洽".into());
        l1.created_at = 1700000000000; // 固定时间戳

        storage.add_l1_summaries("rama-0001", vec![l1]);

        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageLoadHistory::new();
        let data = make_data(Some(ramaria_core::types::Session {
            id: session_id,
            started_at: 1000,
            ended_at: None,
            persona_uid: None,
        }));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("should load L1");
        assert_eq!(output.recent_summaries.len(), 1);
        assert!(output.recent_summaries[0].contains("讨论了编程话题"));
        assert!(output.recent_summaries[0].contains("下午"));
        assert!(output.recent_summaries[0].contains("融洽"));
    }

    #[tokio::test]
    async fn last_active_at_extracted_from_l1() {
        let storage = Arc::new(MockStorage::new());
        let session_id = uuid::Uuid::new_v4();

        let mut l1 = MemoryL1::new(session_id, "测试摘要".into(), None);
        l1.created_at = 1700000000000; // 2023-11-14 22:13:20 UTC

        storage.add_l1_summaries("rama-0001", vec![l1]);

        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageLoadHistory::new();
        let data = make_data(Some(ramaria_core::types::Session {
            id: session_id,
            started_at: 1000,
            ended_at: None,
            persona_uid: None,
        }));

        let result = stage.execute(&ctx, data).await;

        let output = result.expect("should succeed");
        assert!(output.last_active_at.is_some());
        assert!(!output.last_active_at.as_ref().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_l1_summaries_returns_empty() {
        let storage = Arc::new(MockStorage::new());
        let session_id = uuid::Uuid::new_v4();

        let ctx = test_context(storage, Arc::new(MockLlm::local()), None);
        let stage = StageLoadHistory::new();
        let data = make_data(Some(ramaria_core::types::Session {
            id: session_id,
            started_at: 1000,
            ended_at: None,
            persona_uid: None,
        }));

        let result = stage.execute(&ctx, data).await;

        let output = result.expect("should succeed");
        assert!(output.recent_summaries.is_empty());
        assert!(output.last_active_at.is_none());
    }

    // =========================================================
    // 脉络加权注入测试（v1.7 B4，决策 D-V17-006）
    // =========================================================

    /// narrative_weighted=true（默认）：以当前消息为话题依据，用 retriever.search_narrative
    /// 加权注入脉络（话题相关优先），而非无条件取最近 N 条。
    #[tokio::test]
    async fn narrative_weighted_uses_topic_relevance() {
        let storage = Arc::new(MockStorage::new());
        let ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        // 注入两条 L1：一条与查询相关、一条不相关（相关条目应排前/被优先注入）。
        // 使用真实时间戳（now - 天数）避免衰减下溢干扰话题排序。
        let now = ramaria_core::types::now_ms();
        {
            let mut retriever = ctx.retriever.write().expect("retriever 锁可用");
            retriever.index_l1(&ramaria_memory::retriever::L1DocView {
                id: uuid::Uuid::new_v4(),
                summary: "用户讨论了Rust异步编程".to_string(),
                keywords: Some("Rust,编程".to_string()),
                persona_uid: Some("rama-0001".to_string()),
                created_at: now - 3 * 86_400_000,
                salience: 0.5,
                last_accessed_at: None,
            });
            retriever.index_l1(&ramaria_memory::retriever::L1DocView {
                id: uuid::Uuid::new_v4(),
                summary: "用户和朋友去吃了火锅".to_string(),
                keywords: Some("社交,火锅".to_string()),
                persona_uid: Some("rama-0001".to_string()),
                created_at: now - 86_400_000,
                salience: 0.5,
                last_accessed_at: None,
            });
        }

        let stage = StageLoadHistory::new();
        let mut data = make_data(Some(ramaria_core::types::Session {
            id: uuid::Uuid::new_v4(),
            started_at: 1000,
            ended_at: None,
            persona_uid: Some("rama-0001".to_string()),
        }));
        // 当前消息话题：Rust
        data.user_input = "Rust 编程".to_string();

        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert!(!output.recent_summaries.is_empty(), "加权注入应有脉络结果");
        assert!(
            output.recent_summaries[0].contains("Rust"),
            "话题相关的 L1 应优先注入，got: {:?}",
            output.recent_summaries[0]
        );
    }

    /// narrative_weighted=false：回退 v1.6 行为——无条件取最近 N 条
    /// （list_recent_l1_by_persona，按创建时间降序）。
    #[tokio::test]
    async fn narrative_disabled_falls_back_to_recent_l1() {
        let storage = Arc::new(MockStorage::new());
        // storage 预填充最近 L1（v1.6 数据源）
        let mut recent = MemoryL1::new(
            uuid::Uuid::new_v4(),
            "最近的一次对话摘要".into(),
            Some("下午".into()),
        );
        recent.atmosphere = Some("轻松".into());
        recent.created_at = 1_700_000_000_000;
        storage.add_l1_summaries("rama-0001", vec![recent]);

        let mut ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        ctx.config.retrieval.narrative_weighted = false; // 关闭加权 → 回退 v1.6

        let stage = StageLoadHistory::new();
        let mut data = make_data(Some(ramaria_core::types::Session {
            id: uuid::Uuid::new_v4(),
            started_at: 1000,
            ended_at: None,
            persona_uid: Some("rama-0001".to_string()),
        }));
        data.user_input = "完全不相关的话题".to_string();

        let output = stage.execute(&ctx, data).await.expect("应成功");
        assert_eq!(output.recent_summaries.len(), 1, "回退最近 N 条");
        assert!(
            output.recent_summaries[0].contains("最近的一次对话摘要"),
            "v1.6 无条件取最近摘要"
        );
    }

    // =========================================================
    // 注入闸门测试（探针消融 F4/B0/B1：脉络闸门关闭跳过 L1 加载）
    // =========================================================

    /// 脉络闸门关闭（`injection.narrative=false`）→ 跳过近期 L1 摘要加载，
    /// recent_summaries / last_active_at 为空（即使 storage 有 L1 数据）。
    #[tokio::test]
    async fn injection_narrative_off_skips_l1_load() {
        let storage = Arc::new(MockStorage::new());
        let session_id = uuid::Uuid::new_v4();
        // 预置 L1：若闸门不生效将被加载
        let mut l1 = MemoryL1::new(session_id, "不应出现的摘要".into(), Some("下午".into()));
        l1.created_at = 1_700_000_000_000;
        storage.add_l1_summaries("rama-0001", vec![l1]);

        let mut ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        ctx.config.injection.narrative = false;
        let stage = StageLoadHistory::new();
        let data = make_data(Some(ramaria_core::types::Session {
            id: session_id,
            started_at: 1000,
            ended_at: None,
            persona_uid: None,
        }));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("应成功");
        assert!(
            output.recent_summaries.is_empty(),
            "脉络闸门关闭时应跳过 L1 摘要加载"
        );
        assert!(output.last_active_at.is_none(), "last_active_at 应为空");
    }

    /// 脉络闸门开启（默认）→ 正常加载近期 L1（回归红线：默认行为不变）。
    #[tokio::test]
    async fn injection_narrative_on_still_loads_l1() {
        let storage = Arc::new(MockStorage::new());
        let session_id = uuid::Uuid::new_v4();
        let mut l1 = MemoryL1::new(session_id, "应出现的摘要".into(), Some("上午".into()));
        l1.created_at = 1_700_000_000_000;
        storage.add_l1_summaries("rama-0001", vec![l1]);

        let ctx = test_context(storage.clone(), Arc::new(MockLlm::local()), None);
        let stage = StageLoadHistory::new();
        let data = make_data(Some(ramaria_core::types::Session {
            id: session_id,
            started_at: 1000,
            ended_at: None,
            persona_uid: None,
        }));

        let result = stage.execute(&ctx, data).await;

        assert!(result.is_ok());
        let output = result.expect("应成功");
        assert_eq!(output.recent_summaries.len(), 1, "闸门开启应正常加载 L1");
    }

    #[tokio::test]
    async fn missing_session_returns_fatal() {
        let ctx = test_context(
            Arc::new(MockStorage::new()),
            Arc::new(MockLlm::local()),
            None,
        );
        let stage = StageLoadHistory::new();
        let data = make_data(None);

        let result = stage.execute(&ctx, data).await;

        let err = match result {
            Ok(_) => panic!("missing session should fail"),
            Err(e) => e,
        };
        assert!(!err.is_retryable());
        assert_eq!(err.stage(), "LoadHistory");
        assert!(err.source_error().context().contains("session 未设置"));
    }

    #[tokio::test]
    async fn l1_truncated_to_120_chars() {
        let long_summary = "这是一段非常长的摘要".repeat(20);
        let l1 = MemoryL1::new(uuid::Uuid::new_v4(), long_summary, None);

        let formatted = format_l1_as_context_line(&l1);
        assert!(formatted.chars().count() <= 121); // 120 + 省略号
        assert!(formatted.ends_with('…'));
    }

    #[tokio::test]
    async fn l1_format_with_time_and_atmosphere() {
        let mut l1 = MemoryL1::new(
            uuid::Uuid::new_v4(),
            "讨论了编程".into(),
            Some("下午".into()),
        );
        l1.atmosphere = Some("轻松".into());

        let formatted = format_l1_as_context_line(&l1);
        assert!(formatted.contains("下午"));
        assert!(formatted.contains("讨论了编程"));
        assert!(formatted.contains("轻松"));
    }

    #[tokio::test]
    async fn stage_name_is_correct() {
        let stage = StageLoadHistory::new();
        assert_eq!(stage.name(), "LoadHistory");
    }
}
