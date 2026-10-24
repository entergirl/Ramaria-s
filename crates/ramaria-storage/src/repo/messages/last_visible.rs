//! crates/ramaria-storage/src/repo/messages/last_visible.rs - Ramaria 会话最后一条可见消息查询模块
//!
//! 设计特点:
//! - 单条窗口函数查询：按会话分区取最新的一条可见消息，替代逐会话查询
//! - 可见口径与会话预览一致：未撤回且角色为 user / assistant（system / tool 不计）
//! - 单行解析失败记录 WARNING 并跳过，不阻塞会话列表

use std::collections::HashMap;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::Message;
use sqlx::SqlitePool;
use uuid::Uuid;

use super::MessageRow;
use crate::repo::StorageResultExt;

/// 聚合各会话最后一条可见消息（会话列表预览）。
///
/// 口径:
/// - 可见消息 = `recalled_at IS NULL` 且角色为 user / assistant；
/// - 同一会话按 `created_at DESC, rowid DESC` 取第一条（同刻消息以入库顺序靠后者为准）。
///
/// 返回:
/// - 会话 ID → 最后一条可见消息的映射；无可见消息的会话不出现在映射中
///   （空会话或全部消息已撤回，由调用方按无预览处理）。
///
/// 说明:
/// - 单行解析失败时记录 WARNING 并跳过（防御历史脏数据，不阻塞列表）。
pub async fn list_last_visible_messages(
    pool: &SqlitePool,
) -> RamariaResult<HashMap<Uuid, Message>> {
    let rows = sqlx::query_as::<_, MessageRow>(
        "SELECT id, session_id, role, content, created_at, source, import_fingerprint, persona_uid, \
                is_proactive, sender_ref, sender_name, platform_message_id, recalled_at \
         FROM ( \
             SELECT id, session_id, role, content, created_at, source, import_fingerprint, \
                    persona_uid, is_proactive, sender_ref, sender_name, platform_message_id, recalled_at, \
                    ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY created_at DESC, rowid DESC) AS rn \
             FROM messages \
             WHERE recalled_at IS NULL AND role IN ('user', 'assistant') \
         ) \
         WHERE rn = 1",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询会话最后一条可见消息失败")?;

    let mut last_visible = HashMap::with_capacity(rows.len());
    for row in rows {
        match row.into_message() {
            Ok(message) => {
                last_visible.insert(message.session_id, message);
            }
            Err(e) => {
                tracing::warn!(error = %e, "messages 行解析失败，会话预览已跳过该行");
            }
        }
    }
    Ok(last_visible)
}

#[cfg(test)]
mod tests;
