//! rust/crates/ramaria-app/src/stages/load_history.rs - Stage 4: 加载历史消息 + L1 上下文
//!
//! 设计特点:
//! - 对应 send_message 管线 Step 4 + Step 4.5
//! - v1.3 (P-6): 按 token 预算倒序分页加载消息（每页 20 条），避免长会话全量内存加载
//! - 将 Message 转换为 ChatMessage 格式供后续 TokenBudget / BuildRequest 使用
//! - 预加载近期 L1 摘要（跨 session 上下文注入），无条件注入 Block C1
//! - 格式化 L1 摘要为可读文本行
//! - 提取最后活跃时间字符串
//! - 空 session 不报错（新对话无历史消息为正常场景）

use async_trait::async_trait;
use ramaria_core::traits::ChatMessage;
use ramaria_core::types::MemoryL1;

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
/// v1.3 (P-6): 分页加载替代全量加载。
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

        // ---- Step 4: v1.3 (P-6) 按 token 预算倒序分页加载历史消息 ----
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
            "历史消息已加载（v1.3 分页模式）"
        );

        input.history_messages = history_messages;

        // ---- Step 4.5: 预加载近期 L1 摘要（跨 session 上下文注入） ----
        // 不依赖关键词匹配——近期摘要无条件注入 System Prompt Block C1。
        // 解决新 session 发"你好"时 LLM 完全不知道上次聊了什么的问题。
        let actual_uid = input.persona_uid.as_deref().unwrap_or("rama-0001");
        let recent_l1 = ctx
            .storage
            .list_recent_l1_by_persona(actual_uid, 3)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    persona_uid = actual_uid,
                    error = %e,
                    "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                );
                Vec::new()
            });

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

    // 截断到 120 字符
    if base.chars().count() > 120 {
        let truncated: String = base.chars().take(120).collect();
        truncated + "…"
    } else {
        base
    }
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
