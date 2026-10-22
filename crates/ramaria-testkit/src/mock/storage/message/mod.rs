//! crates/ramaria-testkit/src/mock/storage/message/mod.rs - MockStorage 的消息域实现
//!
//! 设计特点:
//! - 只读约束：已关闭会话写入新消息返回校验错误（与真实存储对齐）
//! - 分页查询按创建时间降序，与 SQL 口径一致
//! - 按职责拆分：消息读写与时间口径在本文件，统计查询见 `queries`，附件见 `attachments`

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{Message, MessageRole};
use uuid::Uuid;

use super::MockStorage;

mod attachments;
mod queries;

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
}
