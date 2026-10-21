//! crates/ramaria-testkit/src/mock/storage/message.rs - MockStorage 的消息与附件域实现
//!
//! 设计特点:
//! - 只读约束：已关闭会话写入新消息返回校验错误（与真实存储对齐）
//! - 分页查询按创建时间降序，与 SQL 口径一致
//! - 未读口径只计本地助手消息（无导入指纹）且晚于已读时间
//! - 附件按状态扫描、按 md5 批量回填，保持与写入口径一致

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::ProactiveDeliveryPair;
use ramaria_core::types::{AttachmentStatus, Message, MessageAttachment, MessageKey, MessageRole};
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    fn last_message_time_internal(&self, session_id: Uuid) -> Option<i64> {
        self.messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .and_then(|list| list.iter().map(|m| m.created_at).max())
    }

    fn session_ids_of_persona(&self, persona_uid: &str) -> Vec<Uuid> {
        self.sessions
            .lock()
            .expect("会话锁")
            .values()
            .filter(|s| s.persona_uid.as_deref() == Some(persona_uid))
            .map(|s| s.id)
            .collect()
    }
}

impl MockStorage {
    pub(crate) async fn save_message(&self, message: &Message) -> RamariaResult<()> {
        self.guard("save_message")?;
        {
            let sessions = self.sessions.lock().expect("会话锁");
            if let Some(session) = sessions.get(&message.session_id) {
                if session.ended_at.is_some() {
                    return Err(RamariaError::validation(format!(
                        "session {} 已关闭，不可写入新消息",
                        message.session_id
                    )));
                }
            }
        }
        self.messages
            .lock()
            .expect("消息锁")
            .entry(message.session_id)
            .or_default()
            .push(message.clone());
        Ok(())
    }

    pub(crate) async fn get_message(&self, id: Uuid) -> RamariaResult<Option<Message>> {
        self.guard("get_message")?;
        Ok(self
            .messages
            .lock()
            .expect("消息锁")
            .values()
            .flatten()
            .find(|m| m.id == id)
            .cloned())
    }

    pub(crate) async fn find_message_by_platform_id(
        &self,
        session_id: Uuid,
        platform_message_id: &str,
    ) -> RamariaResult<Option<Message>> {
        self.guard("find_message_by_platform_id")?;
        Ok(self
            .messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .and_then(|list| {
                list.iter()
                    .find(|m| m.platform_message_id.as_deref() == Some(platform_message_id))
            })
            .cloned())
    }

    pub(crate) async fn mark_message_recalled(
        &self,
        session_id: Uuid,
        platform_message_id: &str,
        recalled_at: i64,
    ) -> RamariaResult<u64> {
        self.guard("mark_message_recalled")?;
        let mut messages = self.messages.lock().expect("消息锁");
        let Some(list) = messages.get_mut(&session_id) else {
            return Ok(0);
        };
        let mut count = 0;
        for message in list.iter_mut() {
            if message.platform_message_id.as_deref() == Some(platform_message_id)
                && message.recalled_at.is_none()
            {
                message.recalled_at = Some(recalled_at);
                count += 1;
            }
        }
        Ok(count)
    }

    pub(crate) async fn mark_message_recalled_by_id(
        &self,
        message_id: Uuid,
        recalled_at: i64,
    ) -> RamariaResult<bool> {
        self.guard("mark_message_recalled_by_id")?;
        let mut messages = self.messages.lock().expect("消息锁");
        for list in messages.values_mut() {
            if let Some(message) = list.iter_mut().find(|m| m.id == message_id) {
                if message.recalled_at.is_some() {
                    return Ok(false);
                }
                message.recalled_at = Some(recalled_at);
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) async fn has_assistant_message_after(
        &self,
        session_id: Uuid,
        after_ms: i64,
    ) -> RamariaResult<bool> {
        self.guard("has_assistant_message_after")?;
        Ok(self
            .messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .is_some_and(|list| {
                list.iter()
                    .any(|m| matches!(m.role, MessageRole::Assistant) && m.created_at > after_ms)
            }))
    }

    pub(crate) async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>> {
        self.guard("list_messages")?;
        Ok(self
            .messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) async fn list_messages_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<Message>> {
        self.guard("list_messages_by_persona")?;
        let mut messages: Vec<Message> = self
            .messages
            .lock()
            .expect("消息锁")
            .values()
            .flatten()
            .filter(|m| m.persona_uid.as_deref() == Some(persona_uid))
            .cloned()
            .collect();
        messages.sort_by_key(|m| m.created_at);
        Ok(messages)
    }

    pub(crate) async fn list_messages_paginated(
        &self,
        session_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> RamariaResult<Vec<Message>> {
        self.guard("list_messages_paginated")?;
        let mut all = self
            .messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        all.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        let start = offset.max(0) as usize;
        let take = limit.max(0) as usize;
        Ok(all.into_iter().skip(start).take(take).collect())
    }

    pub(crate) async fn list_messages_by_persona_paginated(
        &self,
        persona_uid: &str,
        limit: i64,
        offset: i64,
    ) -> RamariaResult<Vec<Message>> {
        self.guard("list_messages_by_persona_paginated")?;
        let mut all: Vec<Message> = self
            .messages
            .lock()
            .expect("消息锁")
            .values()
            .flatten()
            .filter(|m| m.persona_uid.as_deref() == Some(persona_uid))
            .cloned()
            .collect();
        all.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        let start = offset.max(0) as usize;
        let take = limit.max(0) as usize;
        Ok(all.into_iter().skip(start).take(take).collect())
    }

    pub(crate) async fn get_last_message_time(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Option<i64>> {
        self.guard("get_last_message_time")?;
        Ok(self.last_message_time_internal(session_id))
    }

    pub(crate) async fn last_message_time_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Option<i64>> {
        self.guard("last_message_time_by_persona")?;
        let ids = self.session_ids_of_persona(persona_uid);
        Ok(ids
            .iter()
            .filter_map(|id| self.last_message_time_internal(*id))
            .max())
    }

    pub(crate) async fn last_user_message_time_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Option<i64>> {
        self.guard("last_user_message_time_by_persona")?;
        let ids = self.session_ids_of_persona(persona_uid);
        let messages = self.messages.lock().expect("消息锁");
        Ok(ids
            .iter()
            .filter_map(|id| messages.get(id))
            .flatten()
            .filter(|m| matches!(m.role, MessageRole::User))
            .map(|m| m.created_at)
            .max())
    }

    pub(crate) async fn has_local_user_message_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<bool> {
        self.guard("has_local_user_message_by_persona")?;
        let ids = self.session_ids_of_persona(persona_uid);
        let messages = self.messages.lock().expect("消息锁");
        Ok(ids
            .iter()
            .filter_map(|id| messages.get(id))
            .flatten()
            .any(|m| matches!(m.role, MessageRole::User) && m.fingerprint.is_none()))
    }

    pub(crate) async fn mark_session_read(
        &self,
        session_id: Uuid,
        at_ms: i64,
    ) -> RamariaResult<()> {
        self.guard("mark_session_read")?;
        self.last_read_at
            .lock()
            .expect("已读锁")
            .insert(session_id, at_ms);
        Ok(())
    }

    pub(crate) async fn list_unread_counts(
        &self,
    ) -> RamariaResult<std::collections::HashMap<Uuid, u32>> {
        self.guard("list_unread_counts")?;
        let last_read = self.last_read_at.lock().expect("已读锁");
        let messages = self.messages.lock().expect("消息锁");
        let mut counts = std::collections::HashMap::new();
        for (session_id, list) in messages.iter() {
            let threshold = last_read.get(session_id).copied().unwrap_or(0);
            let count = list
                .iter()
                .filter(|m| matches!(m.role, MessageRole::Assistant))
                .filter(|m| m.fingerprint.is_none())
                .filter(|m| m.created_at > threshold)
                .count();
            if count > 0 {
                counts.insert(*session_id, count as u32);
            }
        }
        Ok(counts)
    }

    pub(crate) async fn last_message_time_by_session(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Option<i64>> {
        self.guard("last_message_time_by_session")?;
        Ok(self.last_message_time_internal(session_id))
    }

    pub(crate) async fn list_user_message_times_since(
        &self,
        persona_uid: &str,
        since_ms: i64,
    ) -> RamariaResult<Vec<i64>> {
        self.guard("list_user_message_times_since")?;
        let ids = self.session_ids_of_persona(persona_uid);
        let messages = self.messages.lock().expect("消息锁");
        let mut times: Vec<i64> = ids
            .iter()
            .filter_map(|id| messages.get(id))
            .flatten()
            .filter(|m| matches!(m.role, MessageRole::User))
            .map(|m| m.created_at)
            .filter(|t| *t >= since_ms)
            .collect();
        times.sort_unstable();
        Ok(times)
    }

    pub(crate) async fn list_persona_message_times_since(
        &self,
        persona_uid: &str,
        since_ms: i64,
    ) -> RamariaResult<Vec<i64>> {
        self.guard("list_persona_message_times_since")?;
        let mut times: Vec<i64> = self
            .messages
            .lock()
            .expect("消息锁")
            .values()
            .flatten()
            .filter(|m| m.persona_uid.as_deref() == Some(persona_uid))
            .map(|m| m.created_at)
            .filter(|t| *t >= since_ms)
            .collect();
        times.sort_unstable();
        Ok(times)
    }

    pub(crate) async fn list_proactive_delivery_pairs(
        &self,
        persona_uid: &str,
        response_window_ms: i64,
    ) -> RamariaResult<Vec<ProactiveDeliveryPair>> {
        self.guard("list_proactive_delivery_pairs")?;
        let ids = self.session_ids_of_persona(persona_uid);
        let messages = self.messages.lock().expect("消息锁");
        let mut sent: Vec<i64> = Vec::new();
        let mut replies: Vec<i64> = Vec::new();
        for id in &ids {
            let Some(list) = messages.get(id) else {
                continue;
            };
            for message in list {
                if message.is_proactive {
                    sent.push(message.created_at);
                }
                if matches!(message.role, MessageRole::User) && message.fingerprint.is_none() {
                    replies.push(message.created_at);
                }
            }
        }
        sent.sort_unstable();
        replies.sort_unstable();
        let mut pairs = Vec::with_capacity(sent.len());
        for sent_at in sent {
            let responded_at = replies.iter().copied().find(|at| {
                *at > sent_at && (response_window_ms <= 0 || *at <= sent_at + response_window_ms)
            });
            pairs.push(ProactiveDeliveryPair {
                sent_at,
                responded_at,
            });
        }
        Ok(pairs)
    }

    pub(crate) async fn count_messages(&self, session_id: Uuid) -> RamariaResult<u32> {
        self.guard("count_messages")?;
        Ok(self
            .messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .map(|list| list.len() as u32)
            .unwrap_or(0))
    }

    pub(crate) async fn count_messages_by_session(
        &self,
    ) -> RamariaResult<std::collections::HashMap<Uuid, u32>> {
        self.guard("count_messages_by_session")?;
        let messages = self.messages.lock().expect("消息锁");
        Ok(messages
            .iter()
            .filter(|(_, list)| !list.is_empty())
            .map(|(id, list)| (*id, list.len() as u32))
            .collect())
    }

    pub(crate) async fn find_message_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> RamariaResult<Option<Message>> {
        self.guard("find_message_by_fingerprint")?;
        Ok(self
            .messages
            .lock()
            .expect("消息锁")
            .values()
            .flatten()
            .find(|m| m.fingerprint.as_deref() == Some(fingerprint))
            .cloned())
    }

    pub(crate) async fn list_message_keys_by_channel_ref(
        &self,
        channel: &str,
        external_ref: Option<&str>,
    ) -> RamariaResult<Vec<MessageKey>> {
        self.guard("list_message_keys_by_channel_ref")?;
        let session_ids: Vec<Uuid> = self
            .sessions
            .lock()
            .expect("会话锁")
            .values()
            .filter(|s| s.channel == channel && s.external_ref.as_deref() == external_ref)
            .map(|s| s.id)
            .collect();
        let messages = self.messages.lock().expect("消息锁");
        let mut keys: Vec<(i64, MessageKey)> = Vec::new();
        for id in session_ids {
            if let Some(list) = messages.get(&id) {
                for message in list {
                    keys.push((
                        message.created_at,
                        MessageKey {
                            role: message.role,
                            content: message.content.clone(),
                        },
                    ));
                }
            }
        }
        keys.sort_by_key(|(at, _)| *at);
        Ok(keys.into_iter().map(|(_, key)| key).collect())
    }

    pub(crate) async fn insert_message_attachments(
        &self,
        attachments: &[MessageAttachment],
    ) -> RamariaResult<()> {
        self.guard("insert_message_attachments")?;
        let mut stored = self.attachments.lock().expect("附件锁");
        for attachment in attachments {
            let id = self
                .attachment_seq
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut row = attachment.clone();
            row.id = id;
            stored.insert(id, row);
        }
        Ok(())
    }

    pub(crate) async fn list_pending_attachments_by_session(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_pending_attachments_by_session")?;
        let message_ids: Vec<Uuid> = self
            .messages
            .lock()
            .expect("消息锁")
            .get(&session_id)
            .map(|list| list.iter().map(|m| m.id).collect())
            .unwrap_or_default();
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| row.status == AttachmentStatus::Pending)
            .filter(|row| message_ids.contains(&row.message_id))
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    pub(crate) async fn list_attachments_by_messages(
        &self,
        message_ids: &[Uuid],
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_attachments_by_messages")?;
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| message_ids.contains(&row.message_id))
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        Ok(rows)
    }

    pub(crate) async fn fill_attachment_done_by_md5(
        &self,
        md5: &str,
        description: &str,
        description_model: &str,
    ) -> RamariaResult<u64> {
        self.guard("fill_attachment_done_by_md5")?;
        let mut rows = self.attachments.lock().expect("附件锁");
        let mut count = 0;
        for row in rows.values_mut() {
            if row.md5.as_deref() == Some(md5) && row.status == AttachmentStatus::Pending {
                row.status = AttachmentStatus::Done;
                row.description = Some(description.to_string());
                row.description_model = Some(description_model.to_string());
                count += 1;
            }
        }
        Ok(count)
    }

    pub(crate) async fn mark_attachment_done(
        &self,
        id: i64,
        description: &str,
        description_model: &str,
    ) -> RamariaResult<()> {
        self.guard("mark_attachment_done")?;
        if let Some(row) = self.attachments.lock().expect("附件锁").get_mut(&id) {
            row.status = AttachmentStatus::Done;
            row.description = Some(description.to_string());
            row.description_model = Some(description_model.to_string());
        }
        Ok(())
    }

    pub(crate) async fn mark_attachment_status(
        &self,
        id: i64,
        status: AttachmentStatus,
    ) -> RamariaResult<()> {
        self.guard("mark_attachment_status")?;
        if let Some(row) = self.attachments.lock().expect("附件锁").get_mut(&id) {
            row.status = status;
        }
        Ok(())
    }

    pub(crate) async fn find_attachment_description_by_md5(
        &self,
        md5: &str,
    ) -> RamariaResult<Option<(String, String)>> {
        self.guard("find_attachment_description_by_md5")?;
        let rows = self.attachments.lock().expect("附件锁");
        let found = rows
            .values()
            .filter(|row| row.md5.as_deref() == Some(md5))
            .filter(|row| row.status == AttachmentStatus::Done)
            .max_by_key(|row| row.updated_at)
            .and_then(|row| {
                row.description.clone().map(|description| {
                    (
                        description,
                        row.description_model.clone().unwrap_or_default(),
                    )
                })
            });
        Ok(found)
    }

    pub(crate) async fn list_all_pending_attachments(
        &self,
        limit: u32,
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_all_pending_attachments")?;
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| row.status == AttachmentStatus::Pending)
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    pub(crate) async fn list_all_skipped_attachments(
        &self,
        limit: u32,
    ) -> RamariaResult<Vec<MessageAttachment>> {
        self.guard("list_all_skipped_attachments")?;
        let mut rows: Vec<MessageAttachment> = self
            .attachments
            .lock()
            .expect("附件锁")
            .values()
            .filter(|row| row.status == AttachmentStatus::Skipped)
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.id);
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }

    pub(crate) async fn reset_attachment_for_rescan(
        &self,
        id: i64,
        source_ref: Option<&str>,
    ) -> RamariaResult<()> {
        self.guard("reset_attachment_for_rescan")?;
        if let Some(row) = self.attachments.lock().expect("附件锁").get_mut(&id) {
            row.status = AttachmentStatus::Pending;
            if let Some(source_ref) = source_ref {
                row.source_ref = source_ref.to_string();
            }
        }
        Ok(())
    }
}
