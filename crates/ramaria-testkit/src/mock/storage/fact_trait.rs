//! crates/ramaria-testkit/src/mock/storage/fact_trait.rs - MockStorage 的事实与性格域实现
//!
//! 设计特点:
//! - 事实版本链：旧 active 置 superseded，新版本写链指针后插入
//! - 字段查询返回全部版本（含 superseded/candidate），active 查询单独收口
//! - 性格标签按 id 存储 + persona 索引；置信度 / 状态 / 来源可单独改写
//! - 表达层风格统计按 persona 单行 upsert

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{
    FactStatus, PersonaFact, PersonaStyleStats, PersonalityTrait, ProfileField, TraitEvidence,
    TraitSource, TraitStatus,
};

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64> {
        self.guard("save_fact")?;
        let id = self
            .fact_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = fact.clone();
        stored.id = id;
        let persona = stored.persona_uid.clone();
        self.facts.lock().expect("事实锁").insert(id, stored);
        self.facts_by_persona
            .lock()
            .expect("事实索引锁")
            .entry(persona)
            .or_default()
            .push(id);
        Ok(id)
    }

    pub(crate) async fn list_facts_by_persona(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        self.guard("list_facts_by_persona")?;
        let ids = self
            .facts_by_persona
            .lock()
            .expect("事实索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let facts = self.facts.lock().expect("事实锁");
        Ok(ids
            .iter()
            .filter_map(|id| facts.get(id).cloned())
            .filter(|fact| fact.field == field)
            .collect())
    }

    pub(crate) async fn list_active_facts_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        self.guard("list_active_facts_by_persona")?;
        let ids = self
            .facts_by_persona
            .lock()
            .expect("事实索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let facts = self.facts.lock().expect("事实锁");
        Ok(ids
            .iter()
            .filter_map(|id| facts.get(id).cloned())
            .filter(|fact| matches!(fact.status, FactStatus::Active))
            .collect())
    }

    pub(crate) async fn list_active_facts_by_field(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        self.guard("list_active_facts_by_field")?;
        Ok(self
            .list_active_facts_by_persona(persona_uid)
            .await?
            .into_iter()
            .filter(|fact| fact.field == field)
            .collect())
    }

    pub(crate) async fn list_all_facts_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        self.guard("list_all_facts_by_persona")?;
        let ids = self
            .facts_by_persona
            .lock()
            .expect("事实索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let facts = self.facts.lock().expect("事实锁");
        Ok(ids.iter().filter_map(|id| facts.get(id).cloned()).collect())
    }

    pub(crate) async fn get_fact_by_id(&self, id: i64) -> RamariaResult<Option<PersonaFact>> {
        self.guard("get_fact_by_id")?;
        Ok(self.facts.lock().expect("事实锁").get(&id).cloned())
    }

    pub(crate) async fn save_fact_with_version(
        &self,
        old: &PersonaFact,
        fresh: &PersonaFact,
    ) -> RamariaResult<i64> {
        self.guard("save_fact_with_version")?;
        let new_id = self
            .fact_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = fresh.clone();
        stored.id = new_id;
        stored.status = FactStatus::Active;
        stored.version_of = Some(old.id);
        {
            let mut facts = self.facts.lock().expect("事实锁");
            if let Some(previous) = facts.get_mut(&old.id) {
                previous.status = FactStatus::Superseded;
                previous.updated_at = stored.updated_at;
            }
            facts.insert(new_id, stored.clone());
        }
        self.facts_by_persona
            .lock()
            .expect("事实索引锁")
            .entry(stored.persona_uid)
            .or_default()
            .push(new_id);
        Ok(new_id)
    }

    pub(crate) async fn list_fact_versions(&self, seed_id: i64) -> RamariaResult<Vec<PersonaFact>> {
        self.guard("list_fact_versions")?;
        let facts = self.facts.lock().expect("事实锁");
        let Some(seed) = facts.get(&seed_id).cloned() else {
            return Ok(Vec::new());
        };
        let mut chain = vec![seed];
        let mut current = chain[0].version_of;
        let mut guard = 0u32;
        while let Some(previous_id) = current {
            if guard >= 64 {
                break;
            }
            guard += 1;
            match facts.get(&previous_id) {
                Some(previous) => {
                    current = previous.version_of;
                    chain.push(previous.clone());
                }
                None => break,
            }
        }
        chain.reverse();
        Ok(chain)
    }

    pub(crate) async fn count_all_facts_for_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<(ProfileField, usize)>> {
        self.guard("count_all_facts_for_persona")?;
        let fields = [
            ProfileField::BasicInfo,
            ProfileField::PersonalStatus,
            ProfileField::Interests,
            ProfileField::Social,
            ProfileField::History,
            ProfileField::RecentContext,
            ProfileField::SpeakingStyle,
        ];
        let ids = self
            .facts_by_persona
            .lock()
            .expect("事实索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let facts = self.facts.lock().expect("事实锁");
        Ok(fields
            .into_iter()
            .map(|field| {
                let count = ids
                    .iter()
                    .filter_map(|id| facts.get(id))
                    .filter(|fact| fact.field == field)
                    .count();
                (field, count)
            })
            .collect())
    }

    pub(crate) async fn upsert_style_stats(&self, stats: &PersonaStyleStats) -> RamariaResult<()> {
        self.guard("upsert_style_stats")?;
        self.style_stats
            .lock()
            .expect("风格统计锁")
            .insert(stats.persona_uid.clone(), stats.clone());
        Ok(())
    }

    pub(crate) async fn get_style_stats(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Option<PersonaStyleStats>> {
        self.guard("get_style_stats")?;
        Ok(self
            .style_stats
            .lock()
            .expect("风格统计锁")
            .get(persona_uid)
            .cloned())
    }

    pub(crate) async fn save_trait(&self, t: &PersonalityTrait) -> RamariaResult<i64> {
        self.guard("save_trait")?;
        if t.id > 0 {
            let persona = t.persona_uid.clone();
            self.traits.lock().expect("性格锁").insert(t.id, t.clone());
            let mut index = self.traits_by_persona.lock().expect("性格索引锁");
            let ids = index.entry(persona).or_default();
            if !ids.contains(&t.id) {
                ids.push(t.id);
            }
            return Ok(t.id);
        }
        let id = self
            .trait_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = t.clone();
        stored.id = id;
        let persona = stored.persona_uid.clone();
        self.traits.lock().expect("性格锁").insert(id, stored);
        self.traits_by_persona
            .lock()
            .expect("性格索引锁")
            .entry(persona)
            .or_default()
            .push(id);
        Ok(id)
    }

    pub(crate) async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        self.guard("list_traits_by_persona")?;
        let ids = self
            .traits_by_persona
            .lock()
            .expect("性格索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let traits = self.traits.lock().expect("性格锁");
        Ok(ids
            .iter()
            .filter_map(|id| traits.get(id).cloned())
            .collect())
    }

    pub(crate) async fn update_trait_confidence(
        &self,
        id: i64,
        confidence: f64,
        evidence: f64,
        consistency: f64,
    ) -> RamariaResult<()> {
        self.guard("update_trait_confidence")?;
        if let Some(t) = self.traits.lock().expect("性格锁").get_mut(&id) {
            t.confidence = confidence;
            t.evidence = evidence;
            t.consistency = consistency;
        }
        Ok(())
    }

    pub(crate) async fn update_trait_status(
        &self,
        id: i64,
        status: TraitStatus,
    ) -> RamariaResult<()> {
        self.guard("update_trait_status")?;
        if let Some(t) = self.traits.lock().expect("性格锁").get_mut(&id) {
            t.status = status;
        }
        Ok(())
    }

    pub(crate) async fn update_trait_source(
        &self,
        id: i64,
        source: TraitSource,
    ) -> RamariaResult<()> {
        self.guard("update_trait_source")?;
        if let Some(t) = self.traits.lock().expect("性格锁").get_mut(&id) {
            t.source = source;
        }
        Ok(())
    }

    pub(crate) async fn save_evidence(&self, evidence: &TraitEvidence) -> RamariaResult<i64> {
        self.guard("save_evidence")?;
        let id = self
            .evidence_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = evidence.clone();
        stored.id = id;
        self.evidence
            .lock()
            .expect("证据锁")
            .entry(stored.trait_id)
            .or_default()
            .push(stored);
        Ok(id)
    }

    pub(crate) async fn list_evidence_by_trait(
        &self,
        trait_id: i64,
    ) -> RamariaResult<Vec<TraitEvidence>> {
        self.guard("list_evidence_by_trait")?;
        Ok(self
            .evidence
            .lock()
            .expect("证据锁")
            .get(&trait_id)
            .cloned()
            .unwrap_or_default())
    }
}
