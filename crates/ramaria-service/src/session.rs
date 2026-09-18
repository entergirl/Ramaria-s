//! crates/ramaria-service/src/session.rs - 会话读取用例（chat_history）
//!
//! 设计特点:
//! - 两条定位路径：显式 `session_id` 优先；否则取该人格最近一条消息所属会话
//! - 分页语义：存储层按 `created_at DESC` 取页（最新优先），返回前翻正为页内时间正序
//! - `total` 为该会话（或该人格消息）的总条数，供调用方判断是否还有更多历史
//! - 纯读取：不修改任何状态；无数据时返回结构完整的空结果（不报错）

use chrono::{DateTime, Utc};
use ramaria_core::error::RamariaResult;
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{HistoryMessageView, HistoryRequest, HistoryResult};

/// 读取会话历史（分页）。
///
/// 流程:
/// 1. `session_id` 提供 → 直接按会话分页读取；
/// 2. 否则按 `persona` 定位该人格最近一条消息所属会话（无消息 → 空结果）；
/// 3. 两条路径都缺失 → 空结果；
/// 4. 页内消息翻正为时间正序（便于调用方直接展示为对话流）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 历史请求（session_id 与 persona 二选一；limit / offset 可缺省）。
///
/// 返回:
/// - `session_id`（实际读取的会话，无数据时为 None）、`messages`、`total`。
pub(crate) async fn history(engine: &Engine, req: HistoryRequest) -> RamariaResult<HistoryResult> {
    let storage = engine.storage_ref();
    let limit = req.effective_limit() as i64;
    let offset = req.effective_offset() as i64;

    // ---- 1. 定位目标会话 ----
    let target: Option<Uuid> = match req.session_id {
        Some(sid) => Some(sid),
        None => match normalize_persona(req.persona.as_deref()) {
            Some(uid) => match storage.list_messages_by_persona_paginated(&uid, 1, 0).await {
                Ok(list) => list.first().map(|m| m.session_id),
                Err(e) => {
                    tracing::warn!(persona = %uid, error = %e, "定位近期会话失败，返回空历史");
                    None
                }
            },
            None => None,
        },
    };

    let Some(session_id) = target else {
        tracing::debug!("历史请求无可用会话定位，返回空结果");
        return Ok(HistoryResult {
            session_id: None,
            messages: Vec::new(),
            total: 0,
        });
    };

    // ---- 2. 分页读取（DESC 取页）+ 总条数 ----
    let mut page = storage
        .list_messages_paginated(session_id, limit, offset)
        .await?;
    let total = storage.count_messages(session_id).await? as usize;

    // 页内翻正为时间正序（存储层 DESC → 展示与消费按对话流顺序）
    page.reverse();

    let messages: Vec<HistoryMessageView> = page
        .iter()
        .map(|m| HistoryMessageView {
            role: m.role,
            content: m.content.clone(),
            time: DateTime::from_timestamp_millis(m.created_at)
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
            persona_uid: m.persona_uid.clone(),
        })
        .collect();

    tracing::debug!(
        %session_id,
        limit,
        offset,
        returned = messages.len(),
        total,
        "会话历史读取完成"
    );

    Ok(HistoryResult {
        session_id: Some(session_id),
        messages,
        total,
    })
}

/// 归一化人格 uid（空串视为未提供）。
fn normalize_persona(persona: Option<&str>) -> Option<String> {
    persona
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{engine_with_db, seed_messages, seed_persona};
    use crate::types::ChatRole;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::MessageRole;
    use ramaria_storage::SqliteStorage;
    use uuid::Uuid;

    /// 造会话 + N 条消息（created_at 自 base 起逐条 +1）。
    async fn seed_session(
        storage: &SqliteStorage,
        persona: &str,
        count: usize,
        base_ts: i64,
    ) -> Uuid {
        let session = storage
            .create_session(Some(persona))
            .await
            .expect("创建会话");
        seed_messages(storage, session.id, persona, count, base_ts).await;
        session.id
    }

    /// 按 session_id 分页：页内时间正序、total 正确、offset 越界为空。
    #[tokio::test]
    async fn history_pages_within_session() {
        let (engine, storage, dir) = engine_with_db("history").await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session(&storage, "char-0001", 7, 1_000).await;

        // 第一页：最新 3 条（时间正序 = 1004..1006）
        let page1 = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: Some(3),
                offset: None,
                persona: None,
            })
            .await
            .expect("历史读取成功");
        assert_eq!(page1.session_id, Some(session_id));
        assert_eq!(page1.total, 7);
        assert_eq!(page1.messages.len(), 3);
        assert_eq!(page1.messages[0].content, "消息内容 4", "页内时间正序");
        assert_eq!(page1.messages[2].content, "消息内容 6");

        // 第二页：offset 3 → 1001..1003
        let page2 = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: Some(3),
                offset: Some(3),
                persona: None,
            })
            .await
            .expect("历史读取成功");
        assert_eq!(page2.messages[0].content, "消息内容 1");

        // 越界 → 空页但 total 保留
        let beyond = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: Some(3),
                offset: Some(99),
                persona: None,
            })
            .await
            .expect("历史读取成功");
        assert!(beyond.messages.is_empty());
        assert_eq!(beyond.total, 7);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 按 persona 定位：取该人格最近消息所属会话。
    #[tokio::test]
    async fn history_resolves_latest_session_by_persona() {
        let (engine, storage, dir) = engine_with_db("history-persona").await;
        seed_persona(&storage, "char-0001").await;
        let first = seed_session(&storage, "char-0001", 2, 1_000).await;
        let latest = seed_session(&storage, "char-0001", 2, 2_000).await;

        let result = engine
            .history(HistoryRequest {
                session_id: None,
                persona: Some("char-0001".to_string()),
                limit: None,
                offset: None,
            })
            .await
            .expect("历史读取成功");

        assert_eq!(result.session_id, Some(latest), "应定位最近会话");
        assert_ne!(result.session_id, Some(first));
        assert_eq!(result.total, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空请求 / 无数据人格 → 空结构（不报错）。
    #[tokio::test]
    async fn history_empty_inputs_return_empty() {
        let (engine, storage, dir) = engine_with_db("history-empty").await;
        seed_persona(&storage, "char-0001").await;

        let no_input = engine
            .history(HistoryRequest::default())
            .await
            .expect("历史读取成功");
        assert!(no_input.session_id.is_none());
        assert!(no_input.messages.is_empty());
        assert_eq!(no_input.total, 0);

        let no_messages = engine
            .history(HistoryRequest {
                session_id: None,
                persona: Some("char-0001".to_string()),
                limit: None,
                offset: None,
            })
            .await
            .expect("历史读取成功");
        assert!(no_messages.session_id.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 消息视图字段完整（角色 / 内容 / 时间 / 发言人）。
    #[tokio::test]
    async fn history_message_view_fields() {
        let (engine, storage, dir) = engine_with_db("history-fields").await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session(&storage, "char-0001", 2, 5_000).await;

        let result = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: None,
                offset: None,
                persona: None,
            })
            .await
            .expect("历史读取成功");

        assert_eq!(result.messages[0].role, MessageRole::User);
        assert_eq!(result.messages[0].persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(
            result.messages[0].time.timestamp_millis(),
            5_000,
            "时间应与存储一致"
        );
        // ChatRole 与内核角色的映射保持可用（供上层判断"最后一条用户消息"）
        assert_eq!(ChatRole::User.as_str(), "user");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
