//! crates/ramaria-testkit/src/mock/storage/message/queries.rs - MockStorage 的消息查询与统计
//!
//! 设计特点:
//! - 时间类查询按会话或画像聚合，无数据返回 None
//! - 未读口径只计本地助手消息（无导入指纹）且晚于已读时间
//! - 统计类查询按会话维度汇总，零消息会话不出现在结果中

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::ProactiveDeliveryPair;
use ramaria_core::types::{Message, MessageKey, MessageRole};
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
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
}
