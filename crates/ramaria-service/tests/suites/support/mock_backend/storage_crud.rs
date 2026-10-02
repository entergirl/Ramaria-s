//! crates/ramaria-service/tests/suites/support/mock_backend/storage_crud.rs - MockStorage 的 StoreCrud 实现
//!
//! 设计特点:
//! - 内存 HashMap 承载会话 / 消息 / L1 / persona / 事件 / 事实 / 人格推断等全部业务表
//! - 会话关闭语义：`close_session_if_active` 以原子的条件抢占表达（存在且未关闭才置结束时间）
//! - 只读约束：已关闭会话写入新消息返回校验错误（与真实存储对齐）
//! - 事实版本链：`save_fact_with_version` 旧 active 置 superseded 并写入链指针后插入新版本
//! - 吸收语义：`mark_events_absorbed` 以"从 persona 索引移除"等价真实实现的标记吸收

use std::sync::atomic::Ordering;

use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    ClusterSnapshot, EventRelation, EventSource, FactStatus, MemoryEvent, MemoryL1, Message,
    Persona, PersonaExample, PersonaFact, PersonalityTrait, ProfileField, Session, TraitEvidence,
    TraitStatus, UttBlock,
};
use uuid::Uuid;

use super::MockStorage;

#[async_trait]
impl StoreCrud for MockStorage {
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        let session = Session {
            id: Uuid::new_v4(),
            started_at: 1000,
            ended_at: None,
            persona_uid: persona_uid.map(|s| s.to_string()),
            ..Session::default()
        };
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id, session.clone());
        Ok(session)
    }

    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&session_id) {
            session.ended_at = Some(2000);
        }
        Ok(())
    }

    /// 条件关闭会话（原子抢占语义）：仅当会话存在且未关闭时置结束时间并返回 true。
    async fn close_session_if_active(&self, session_id: Uuid) -> RamariaResult<bool> {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get_mut(&session_id) {
            Some(session) if session.ended_at.is_none() => {
                session.ended_at = Some(2000);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>> {
        Ok(self.sessions.lock().unwrap().get(&session_id).cloned())
    }

    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        Ok(self
            .sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.ended_at.is_none())
            .cloned()
            .collect())
    }

    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        Ok(self.sessions.lock().unwrap().values().cloned().collect())
    }

    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        self.sessions.lock().unwrap().remove(&session_id);
        self.messages.lock().unwrap().remove(&session_id);
        self.utt_blocks.lock().unwrap().remove(&session_id);
        Ok(())
    }

    async fn bind_session_persona_uid(
        &self,
        session_id: Uuid,
        persona_uid: &str,
    ) -> RamariaResult<()> {
        if let Some(session) = self.sessions.lock().unwrap().get_mut(&session_id) {
            session.persona_uid = Some(persona_uid.to_string());
        }
        Ok(())
    }

    // -- Utt Blocks（桥接与封存链路测试支持） --

    async fn insert_utt_block(&self, block: &UttBlock) -> RamariaResult<i64> {
        let mut map = self.utt_blocks.lock().unwrap();
        let list = map.entry(block.session_id).or_default();
        let id = list.len() as i64 + 1;
        let mut b = block.clone();
        b.id = id;
        list.push(b);
        Ok(id)
    }

    async fn list_utt_blocks_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<UttBlock>> {
        Ok(self
            .utt_blocks
            .lock()
            .unwrap()
            .values()
            .flatten()
            .filter(|b| b.persona_uid == persona_uid)
            .cloned()
            .collect())
    }

    async fn get_latest_utt_block_by_session(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Option<UttBlock>> {
        Ok(self
            .utt_blocks
            .lock()
            .unwrap()
            .get(&session_id)
            .and_then(|list| list.last())
            .cloned())
    }

    async fn delete_utt_blocks_by_session(&self, session_id: Uuid) -> RamariaResult<usize> {
        Ok(self
            .utt_blocks
            .lock()
            .unwrap()
            .remove(&session_id)
            .map(|l| l.len())
            .unwrap_or(0))
    }

    async fn save_message(&self, message: &Message) -> RamariaResult<()> {
        // 只读约束——已关闭 session 不可写入新消息
        let sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(&message.session_id)
            && session.ended_at.is_some()
        {
            return Err(RamariaError::validation(format!(
                "session {} 已关闭，不可写入新消息",
                message.session_id
            )));
        }
        drop(sessions);

        self.messages
            .lock()
            .unwrap()
            .entry(message.session_id)
            .or_default()
            .push(message.clone());
        Ok(())
    }

    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>> {
        Ok(self
            .messages
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_messages_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<Message>> {
        Ok(Vec::new())
    }

    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()> {
        self.l1_list
            .lock()
            .unwrap()
            .entry(memory.session_id)
            .or_default()
            .push(memory.clone());
        // 同步维护 persona 维度索引（list_unabsorbed_l1 等查询依赖）
        if let Some(uid) = memory.persona_uid.clone() {
            self.l1_by_persona
                .lock()
                .unwrap()
                .entry(uid)
                .or_default()
                .push(memory.clone());
        }
        Ok(())
    }

    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        Ok(self
            .l1_list
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        // 扫描 persona 维度索引与 session 维度列表（add_l1_summaries 走 persona 索引）
        let l1_by_persona = self.l1_by_persona.lock().unwrap();
        if let Some(found) = l1_by_persona
            .values()
            .flatten()
            .find(|l| l.id == id)
            .cloned()
        {
            return Ok(Some(found));
        }
        let l1_list = self.l1_list.lock().unwrap();
        Ok(l1_list.values().flatten().find(|l| l.id == id).cloned())
    }

    async fn mark_l1_absorbed(&self, _l1_ids: &[Uuid]) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_unabsorbed_l1(&self, _persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }

    async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> {
        // 无主 L1：persona_uid IS NULL 的条目（导入产生的 L1 属此类）
        Ok(self
            .l1_list
            .lock()
            .unwrap()
            .values()
            .flatten()
            .filter(|m| m.persona_uid.is_none())
            .cloned()
            .collect())
    }

    async fn list_recent_l1_by_persona(
        &self,
        persona_uid: &str,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryL1>> {
        Ok(self
            .l1_by_persona
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit as usize)
            .collect())
    }

    async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64> {
        let mut seq = self.persona_seq.lock().unwrap();
        *seq += 1;
        let id = *seq;
        let mut p = persona.clone();
        p.id = id;
        self.personas.lock().unwrap().insert(persona.uid.clone(), p);
        Ok(id)
    }

    async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>> {
        Ok(self.personas.lock().unwrap().get(uid).cloned())
    }

    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        Ok(self.personas.lock().unwrap().values().cloned().collect())
    }

    async fn update_persona(
        &self,
        _uid: &str,
        _name: &str,
        _avatar: Option<&str>,
        _config: Option<&str>,
        _description: Option<&str>,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> {
        let id = self
            .event_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut ev = event.clone();
        ev.id = id;
        self.events.lock().unwrap().insert(id, ev.clone());
        self.events_by_persona
            .lock()
            .unwrap()
            .entry(ev.persona_uid.clone())
            .or_default()
            .push(id);
        Ok(id)
    }

    async fn get_event(&self, id: i64) -> RamariaResult<Option<MemoryEvent>> {
        Ok(self.events.lock().unwrap().get(&id).cloned())
    }

    async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        _offset: i64,
        _limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        let ids = self
            .events_by_persona
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let events = self.events.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| events.get(id).cloned())
            .collect())
    }

    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        self.list_events_by_persona(persona_uid, 0, i64::MAX).await
    }

    async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()> {
        // mock 语义：吸收 = 从列表移除（与真实实现"标记 absorbed"等效）
        let events = self.events.lock().unwrap();
        let mut by_persona = self.events_by_persona.lock().unwrap();
        for (_, ids) in by_persona.iter_mut() {
            ids.retain(|id| !event_ids.contains(id));
        }
        drop(events);
        Ok(())
    }

    async fn save_event_relation(&self, _rel: &EventRelation) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn save_event_source(
        &self,
        event_id: i64,
        l1_id: Uuid,
        weight: f64,
    ) -> RamariaResult<()> {
        let id = self
            .event_source_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.event_sources.lock().unwrap().push(EventSource {
            id,
            event_id,
            l1_id,
            weight,
        });
        Ok(())
    }

    async fn list_event_sources_by_event(&self, event_id: i64) -> RamariaResult<Vec<EventSource>> {
        Ok(self
            .event_sources
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.event_id == event_id)
            .cloned()
            .collect())
    }

    async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64> {
        let id = self.fact_seq.fetch_add(1, Ordering::SeqCst);
        let mut f = fact.clone();
        f.id = id;
        let persona = f.persona_uid.clone();
        self.facts.lock().unwrap().insert(id, f);
        self.facts_by_persona
            .lock()
            .unwrap()
            .entry(persona)
            .or_default()
            .push(id);
        Ok(id)
    }

    async fn save_fact_with_version(
        &self,
        old: &PersonaFact,
        fresh: &PersonaFact,
    ) -> RamariaResult<i64> {
        // 模拟真实 save_fact_with_version 的原子语义：旧 active → superseded + 新 insert + 链指针
        let new_id = self.fact_seq.fetch_add(1, Ordering::SeqCst);
        let mut f = fresh.clone();
        f.id = new_id;
        f.status = FactStatus::Active;
        f.version_of = Some(old.id);
        {
            let mut facts = self.facts.lock().unwrap();
            if let Some(o) = facts.get_mut(&old.id) {
                o.status = FactStatus::Superseded;
                o.updated_at = f.updated_at;
            }
            facts.insert(new_id, f);
        }
        self.facts_by_persona
            .lock()
            .unwrap()
            .entry(fresh.persona_uid.clone())
            .or_default()
            .push(new_id);
        Ok(new_id)
    }

    async fn list_facts_by_persona(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        let ids = self
            .facts_by_persona
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let facts = self.facts.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| facts.get(id).cloned())
            .filter(|f| f.field == field)
            .collect())
    }

    async fn list_active_facts_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        let ids = self
            .facts_by_persona
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let facts = self.facts.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| facts.get(id).cloned())
            .filter(|f| matches!(f.status, FactStatus::Active))
            .collect())
    }

    async fn list_active_facts_by_field(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(self
            .list_active_facts_by_persona(persona_uid)
            .await?
            .into_iter()
            .filter(|f| f.field == field)
            .collect())
    }

    async fn save_trait(&self, t: &PersonalityTrait) -> RamariaResult<i64> {
        let id = if t.id > 0 {
            // 更新已有 trait（replace）
            let mut traits = self.traits.lock().unwrap();
            let persona = t.persona_uid.clone();
            traits.insert(t.id, t.clone());
            // 确保索引中存在
            self.traits_by_persona
                .lock()
                .unwrap()
                .entry(persona)
                .or_default()
                .push(t.id);
            t.id
        } else {
            // 新增 trait
            let id = self.trait_seq.fetch_add(1, Ordering::SeqCst);
            let mut new_t = t.clone();
            new_t.id = id;
            let persona = new_t.persona_uid.clone();
            self.traits.lock().unwrap().insert(id, new_t);
            self.traits_by_persona
                .lock()
                .unwrap()
                .entry(persona)
                .or_default()
                .push(id);
            id
        };
        Ok(id)
    }

    async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        let by_persona = self.traits_by_persona.lock().unwrap();
        let trait_ids = by_persona.get(persona_uid).cloned().unwrap_or_default();
        let traits = self.traits.lock().unwrap();
        Ok(trait_ids
            .iter()
            .filter_map(|id| traits.get(id).cloned())
            .collect())
    }

    async fn update_trait_confidence(
        &self,
        id: i64,
        confidence: f64,
        evidence: f64,
        consistency: f64,
    ) -> RamariaResult<()> {
        if let Some(t) = self.traits.lock().unwrap().get_mut(&id) {
            t.confidence = confidence;
            t.evidence = evidence;
            t.consistency = consistency;
        }
        Ok(())
    }

    async fn update_trait_status(&self, id: i64, status: TraitStatus) -> RamariaResult<()> {
        if let Some(t) = self.traits.lock().unwrap().get_mut(&id) {
            t.status = status;
        }
        Ok(())
    }

    async fn save_evidence(&self, e: &TraitEvidence) -> RamariaResult<i64> {
        let id = self.evidence_seq.fetch_add(1, Ordering::SeqCst);
        let mut new_e = e.clone();
        new_e.id = id;
        self.evidence
            .lock()
            .unwrap()
            .entry(e.trait_id)
            .or_default()
            .push(new_e);
        Ok(id)
    }

    async fn list_evidence_by_trait(&self, trait_id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        Ok(self
            .evidence
            .lock()
            .unwrap()
            .get(&trait_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_example(&self, e: &PersonaExample) -> RamariaResult<i64> {
        // 实际存储（与 test_utils 对齐），
        // 使 examples 配置传播测试可端到端断言注入行为。
        let mut id = e.id;
        if id <= 0 {
            id = self.examples.lock().unwrap().len() as i64 + 1;
        }
        let mut ex = e.clone();
        ex.id = id;
        self.examples
            .lock()
            .unwrap()
            .entry(ex.persona_uid.clone())
            .or_default()
            .push(ex);
        Ok(id)
    }

    async fn list_selected_examples(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaExample>> {
        Ok(self
            .examples
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.selected)
            .collect())
    }

    async fn list_all_examples(&self, persona_uid: &str) -> RamariaResult<Vec<PersonaExample>> {
        Ok(self
            .examples
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_cluster_snapshot(&self, s: &ClusterSnapshot) -> RamariaResult<i64> {
        let id = self.snapshot_seq.fetch_add(1, Ordering::SeqCst);
        let mut new_s = s.clone();
        new_s.id = id;
        let mut snaps = self.cluster_snapshots.lock().unwrap();
        // 与 repo::cluster::save 对齐：写入 current 前先归档同 (persona, category) 旧 current。
        if new_s.is_current {
            for old in snaps.iter_mut() {
                if old.persona_uid == new_s.persona_uid
                    && old.category == new_s.category
                    && old.is_current
                {
                    old.is_current = false;
                }
            }
        }
        snaps.push(new_s);
        Ok(id)
    }

    async fn get_current_snapshots(
        &self,
        persona_uid: &str,
        category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        Ok(self
            .cluster_snapshots
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.persona_uid == persona_uid && s.category == category && s.is_current)
            .cloned()
            .collect())
    }

    async fn upsert_keyword(&self, _keyword: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(Vec::new())
    }
}
