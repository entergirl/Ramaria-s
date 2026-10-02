//! crates/ramaria-cli/tests/common/storage_crud.rs - MockStorage 的 StoreCrud 实现
//!
//! 设计特点:
//! - 内存 HashMap 实现的 StoreCrud 契约（会话 / 消息 / L1 / 人格 / 事件 / 规则 / 事实等）
//! - 作为 common 的子模块，可直接访问 MockStorage 私有字段（零放宽）
//! - 仅服务 CLI 集成测试，不触碰真实存储

use super::*;
use async_trait::async_trait;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, Persona, PersonaExample,
    PersonaFact, PersonaStyleStats, PersonalityTrait, ProfileField, Session, TraitEvidence,
    TraitStatus,
};
use uuid::Uuid;

#[async_trait]
impl StoreCrud for MockStorage {
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        let session = Session {
            id: Uuid::new_v4(),
            started_at: 1_717_977_600_000,
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
        if let Some(s) = self.sessions.lock().unwrap().get_mut(&session_id) {
            s.ended_at = Some(1_717_986_240_000);
        }
        Ok(())
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
        Ok(())
    }

    /// 级联删除：本 mock 持有的关联数据已由 `delete_session` 一并清理，
    /// 故显式委托（trait 默认实现为 Unsupported，需实现方显式 opt-in）。
    async fn delete_session_cascade(&self, session_id: Uuid) -> RamariaResult<()> {
        self.delete_session(session_id).await
    }

    async fn save_message(&self, message: &Message) -> RamariaResult<()> {
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

    async fn save_memory_l1(&self, _memory: &MemoryL1) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_memory_l1(&self, _session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        Ok(self
            .l1_list
            .lock()
            .unwrap()
            .get(&_session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn get_memory_l1(&self, _id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        Ok(None)
    }

    async fn mark_l1_absorbed(&self, _l1_ids: &[Uuid]) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_unabsorbed_l1(&self, _persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        // 返回所有 L1（MockStorage 不做 absorb 标记区分）
        let all: Vec<MemoryL1> = self
            .l1_list
            .lock()
            .unwrap()
            .values()
            .flatten()
            .cloned()
            .collect();
        Ok(all)
    }

    async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> {
        // MockStorage 不做 absorb/persona 区分：返回全部 L1（与 list_unabsorbed_l1 一致）
        self.list_unabsorbed_l1("").await
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
        uid: &str,
        name: &str,
        avatar: Option<&str>,
        config: Option<&str>,
        _description: Option<&str>,
    ) -> RamariaResult<()> {
        let mut personas = self.personas.lock().unwrap();
        if let Some(p) = personas.get_mut(uid) {
            p.name = name.to_string();
            if let Some(av) = avatar {
                p.avatar = Some(av.to_string());
            }
            if let Some(cfg) = config {
                p.config = Some(cfg.to_string());
            }
            tracing::debug!(%uid, "MockStorage: persona 已更新");
            Ok(())
        } else {
            Err(RamariaError::storage(format!("persona 不存在: uid={uid}")))
        }
    }

    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> {
        let id = self
            .event_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut ev = event.clone();
        ev.id = id;
        self.events
            .lock()
            .unwrap()
            .entry(ev.persona_uid.clone())
            .or_default()
            .push(ev);
        Ok(id)
    }

    async fn get_event(&self, id: i64) -> RamariaResult<Option<MemoryEvent>> {
        let events = self.events.lock().unwrap();
        Ok(events.values().flatten().find(|e| e.id == id).cloned())
    }

    async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        _offset: i64,
        _limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        Ok(self
            .events
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default())
    }

    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        self.list_events_by_persona(persona_uid, 0, i64::MAX).await
    }

    async fn mark_events_absorbed(&self, _event_ids: &[i64]) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_event_relation(&self, _rel: &EventRelation) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn save_event_source(
        &self,
        _event_id: i64,
        _l1_id: Uuid,
        _weight: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64> {
        let id = self
            .fact_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut f = fact.clone();
        f.id = id;
        self.facts.lock().unwrap().push(f);
        Ok(id)
    }

    async fn list_facts_by_persona(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        // 语义: 返回该字段的**全部**版本（含 superseded/candidate），供版本链展示。
        Ok(self
            .facts
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.persona_uid == persona_uid && f.field == field)
            .cloned()
            .collect())
    }

    async fn list_active_facts_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(self
            .facts
            .lock()
            .unwrap()
            .iter()
            .filter(|f| {
                f.persona_uid == persona_uid && f.status == ramaria_core::types::FactStatus::Active
            })
            .cloned()
            .collect())
    }

    async fn list_active_facts_by_field(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(self
            .facts
            .lock()
            .unwrap()
            .iter()
            .filter(|f| {
                f.persona_uid == persona_uid
                    && f.field == field
                    && f.status == ramaria_core::types::FactStatus::Active
            })
            .cloned()
            .collect())
    }

    async fn list_all_facts_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(self
            .facts
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.persona_uid == persona_uid)
            .cloned()
            .collect())
    }

    async fn get_fact_by_id(&self, id: i64) -> RamariaResult<Option<PersonaFact>> {
        Ok(self
            .facts
            .lock()
            .unwrap()
            .iter()
            .find(|f| f.id == id)
            .cloned())
    }

    async fn save_fact_with_version(
        &self,
        old: &PersonaFact,
        f: &PersonaFact,
    ) -> RamariaResult<i64> {
        Ok(self.add_fact_with_version(old, f.clone()))
    }

    async fn list_fact_versions(&self, seed_id: i64) -> RamariaResult<Vec<PersonaFact>> {
        let facts = self.facts.lock().unwrap();
        let seed = match facts.iter().find(|f| f.id == seed_id) {
            Some(s) => s.clone(),
            None => return Ok(Vec::new()),
        };
        // 沿 version_of 链回溯（与真实 repo 一致：链头最早在前，需 reverse）
        let mut chain = vec![seed];
        let mut current = chain[0].version_of;
        let mut guard = 0u32;
        while let Some(pid) = current {
            if guard >= 64 {
                break;
            }
            guard += 1;
            match facts.iter().find(|f| f.id == pid) {
                Some(f) => {
                    current = f.version_of;
                    chain.push(f.clone());
                }
                None => break,
            }
        }
        chain.reverse();
        Ok(chain)
    }

    async fn save_trait(&self, _t: &PersonalityTrait) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        Ok(self
            .traits
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default())
    }

    async fn update_trait_confidence(
        &self,
        _id: i64,
        _confidence: f64,
        _evidence: f64,
        _consistency: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn update_trait_status(&self, _id: i64, _status: TraitStatus) -> RamariaResult<()> {
        Ok(())
    }

    async fn save_evidence(&self, _e: &TraitEvidence) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn list_evidence_by_trait(&self, _trait_id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        Ok(Vec::new())
    }

    async fn save_example(&self, _e: &PersonaExample) -> RamariaResult<i64> {
        Ok(1)
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
            .unwrap_or_default())
    }

    async fn save_cluster_snapshot(&self, _s: &ClusterSnapshot) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn get_current_snapshots(
        &self,
        _persona_uid: &str,
        _category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        Ok(Vec::new())
    }

    async fn upsert_keyword(&self, _keyword: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(Vec::new())
    }

    // -- 表达层风格统计（persona_style_stats，CLI style 命令测试覆写默认实现） --

    /// 按 persona 单行 upsert（内存 HashMap，persona_uid 主键语义）。
    async fn upsert_style_stats(&self, stats: &PersonaStyleStats) -> RamariaResult<()> {
        self.style_stats
            .lock()
            .unwrap()
            .insert(stats.persona_uid.clone(), stats.clone());
        Ok(())
    }

    /// 按 persona 查询风格统计（无记录返回 None）。
    async fn get_style_stats(&self, persona_uid: &str) -> RamariaResult<Option<PersonaStyleStats>> {
        Ok(self.style_stats.lock().unwrap().get(persona_uid).cloned())
    }
}
