//! crates/ramaria-testkit/src/mock/storage/session.rs - MockStorage 的会话域实现
//!
//! 设计特点:
//! - 会话关闭采用条件抢占语义：仅未关闭会话可被置结束时间
//! - 支持按通道创建与查找活跃会话，供外部入口续写定位
//! - 成员行按 (session, platform_ref) upsert，读取按首见时间排序
//! - 补跑候选只统计本地通道、已关闭、有消息且无任何 L1 的会话

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{ChannelBinding, Session, SessionMember};
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        self.guard("create_session")?;
        let session = Session {
            id: Uuid::new_v4(),
            started_at: 1000,
            ended_at: None,
            persona_uid: persona_uid.map(str::to_string),
            ..Session::default()
        };
        self.sessions
            .lock()
            .expect("会话锁")
            .insert(session.id, session.clone());
        Ok(session)
    }

    pub(crate) async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> {
        self.guard("close_session")?;
        if let Some(session) = self.sessions.lock().expect("会话锁").get_mut(&session_id) {
            session.ended_at = Some(2000);
        }
        Ok(())
    }

    pub(crate) async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>> {
        self.guard("get_session")?;
        Ok(self
            .sessions
            .lock()
            .expect("会话锁")
            .get(&session_id)
            .cloned())
    }

    pub(crate) async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        self.guard("list_active_sessions")?;
        Ok(self
            .sessions
            .lock()
            .expect("会话锁")
            .values()
            .filter(|s| s.ended_at.is_none())
            .cloned()
            .collect())
    }

    pub(crate) async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        self.guard("list_sessions")?;
        Ok(self
            .sessions
            .lock()
            .expect("会话锁")
            .values()
            .cloned()
            .collect())
    }

    pub(crate) async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        self.guard("delete_session")?;
        self.sessions.lock().expect("会话锁").remove(&session_id);
        self.messages.lock().expect("消息锁").remove(&session_id);
        self.utt_blocks.lock().expect("utt 锁").remove(&session_id);
        Ok(())
    }

    pub(crate) async fn delete_session_cascade(&self, session_id: Uuid) -> RamariaResult<()> {
        self.guard("delete_session_cascade")?;
        self.sessions.lock().expect("会话锁").remove(&session_id);
        self.messages.lock().expect("消息锁").remove(&session_id);
        self.utt_blocks.lock().expect("utt 锁").remove(&session_id);
        self.l1_list.lock().expect("L1 列表锁").remove(&session_id);
        Ok(())
    }

    pub(crate) async fn bind_session_persona_uid(
        &self,
        session_id: Uuid,
        persona_uid: &str,
    ) -> RamariaResult<()> {
        self.guard("bind_session_persona_uid")?;
        if let Some(session) = self.sessions.lock().expect("会话锁").get_mut(&session_id) {
            session.persona_uid = Some(persona_uid.to_string());
        }
        Ok(())
    }

    pub(crate) async fn create_session_in_channel(
        &self,
        persona_uid: Option<&str>,
        channel: &str,
        external_ref: Option<&str>,
    ) -> RamariaResult<Session> {
        self.guard("create_session_in_channel")?;
        let session = Session {
            id: Uuid::new_v4(),
            started_at: 1000,
            ended_at: None,
            persona_uid: persona_uid.map(str::to_string),
            channel: channel.to_string(),
            external_ref: external_ref.map(str::to_string),
        };
        self.sessions
            .lock()
            .expect("会话锁")
            .insert(session.id, session.clone());
        Ok(session)
    }

    pub(crate) async fn find_active_session_by_channel(
        &self,
        channel: &str,
        external_ref: Option<&str>,
    ) -> RamariaResult<Option<Session>> {
        self.guard("find_active_session_by_channel")?;
        let sessions = self.sessions.lock().expect("会话锁");
        let mut hits: Vec<Session> = sessions
            .values()
            .filter(|s| s.channel == channel && s.external_ref.as_deref() == external_ref)
            .filter(|s| s.ended_at.is_none())
            .cloned()
            .collect();
        hits.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(hits.into_iter().next())
    }

    pub(crate) async fn close_session_if_active(&self, session_id: Uuid) -> RamariaResult<bool> {
        self.guard("close_session_if_active")?;
        let mut sessions = self.sessions.lock().expect("会话锁");
        match sessions.get_mut(&session_id) {
            Some(session) if session.ended_at.is_none() => {
                session.ended_at = Some(2000);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(crate) async fn upsert_session_members(
        &self,
        members: &[SessionMember],
    ) -> RamariaResult<()> {
        self.guard("upsert_session_members")?;
        let mut stored = self.session_members.lock().expect("成员锁");
        for member in members {
            stored.insert(
                (member.session_id, member.platform_ref.clone()),
                member.clone(),
            );
        }
        Ok(())
    }

    pub(crate) async fn list_session_members(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Vec<SessionMember>> {
        self.guard("list_session_members")?;
        let stored = self.session_members.lock().expect("成员锁");
        let mut members: Vec<SessionMember> = stored
            .values()
            .filter(|m| m.session_id == session_id)
            .cloned()
            .collect();
        members.sort_by(|a, b| {
            (a.first_seen_at, a.platform_ref.clone())
                .cmp(&(b.first_seen_at, b.platform_ref.clone()))
        });
        Ok(members)
    }

    pub(crate) async fn list_closed_sessions_missing_l1(
        &self,
        limit: u32,
    ) -> RamariaResult<Vec<Uuid>> {
        self.guard("list_closed_sessions_missing_l1")?;
        let sessions = self.sessions.lock().expect("会话锁");
        let messages = self.messages.lock().expect("消息锁");
        let l1_list = self.l1_list.lock().expect("L1 列表锁");
        let mut candidates: Vec<Session> = sessions
            .values()
            .filter(|s| s.ended_at.is_some() && s.channel == "local")
            .filter(|s| messages.get(&s.id).is_some_and(|list| !list.is_empty()))
            .filter(|s| l1_list.get(&s.id).is_none_or(|list| list.is_empty()))
            .cloned()
            .collect();
        candidates.sort_by_key(|s| s.started_at);
        if limit > 0 {
            candidates.truncate(limit as usize);
        }
        Ok(candidates.into_iter().map(|s| s.id).collect())
    }

    pub(crate) async fn upsert_channel_binding(
        &self,
        channel: &str,
        external_ref: &str,
        persona_uid: Option<&str>,
        enabled: bool,
        allow_proactive: bool,
    ) -> RamariaResult<ChannelBinding> {
        self.guard("upsert_channel_binding")?;
        let key = (channel.to_string(), external_ref.to_string());
        let mut bindings = self.channel_bindings.lock().expect("绑定锁");
        let binding = match bindings.get_mut(&key) {
            Some(existing) => {
                existing.persona_uid = persona_uid.map(str::to_string);
                existing.enabled = enabled;
                existing.allow_proactive = allow_proactive;
                existing.updated_at = 2000;
                existing.clone()
            }
            None => {
                let id = self
                    .binding_seq
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let binding = ChannelBinding {
                    id,
                    channel: channel.to_string(),
                    external_ref: external_ref.to_string(),
                    persona_uid: persona_uid.map(str::to_string),
                    enabled,
                    allow_proactive,
                    created_at: 1000,
                    updated_at: 1000,
                };
                bindings.insert(key, binding.clone());
                binding
            }
        };
        Ok(binding)
    }

    pub(crate) async fn get_channel_binding(
        &self,
        channel: &str,
        external_ref: &str,
    ) -> RamariaResult<Option<ChannelBinding>> {
        self.guard("get_channel_binding")?;
        Ok(self
            .channel_bindings
            .lock()
            .expect("绑定锁")
            .get(&(channel.to_string(), external_ref.to_string()))
            .cloned())
    }

    pub(crate) async fn list_channel_bindings(&self) -> RamariaResult<Vec<ChannelBinding>> {
        self.guard("list_channel_bindings")?;
        let bindings = self.channel_bindings.lock().expect("绑定锁");
        let mut list: Vec<ChannelBinding> = bindings.values().cloned().collect();
        list.sort_by(|a, b| {
            (a.channel.clone(), a.external_ref.clone())
                .cmp(&(b.channel.clone(), b.external_ref.clone()))
        });
        Ok(list)
    }

    pub(crate) async fn delete_channel_binding(
        &self,
        channel: &str,
        external_ref: &str,
    ) -> RamariaResult<()> {
        self.guard("delete_channel_binding")?;
        self.channel_bindings
            .lock()
            .expect("绑定锁")
            .remove(&(channel.to_string(), external_ref.to_string()));
        Ok(())
    }
}
