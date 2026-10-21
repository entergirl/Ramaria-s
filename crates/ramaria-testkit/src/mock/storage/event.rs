//! crates/ramaria-testkit/src/mock/storage/event.rs - MockStorage 的 L2 事件域实现
//!
//! 设计特点:
//! - 事件按 id 存储 + persona 维度索引；吸收时置标记并从索引移除
//! - 显著性与时间窗查询按真实口径排序（salience/end/id 降序）
//! - 批次写入先校验下标，再一次性落事件、来源、关系与 L1 吸收标记
//! - 事件所属会话经溯源权重最高者反查

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{
    EventBatchWrite, EventRelation, EventSource, MemoryEvent, PersonaEventAggregate,
};
use std::collections::HashMap;
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> {
        self.guard("save_event")?;
        let id = self
            .event_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = event.clone();
        stored.id = id;
        self.events
            .lock()
            .expect("事件锁")
            .insert(id, stored.clone());
        self.events_by_persona
            .lock()
            .expect("事件索引锁")
            .entry(stored.persona_uid)
            .or_default()
            .push(id);
        Ok(id)
    }

    pub(crate) async fn get_event(&self, id: i64) -> RamariaResult<Option<MemoryEvent>> {
        self.guard("get_event")?;
        Ok(self.events.lock().expect("事件锁").get(&id).cloned())
    }

    pub(crate) async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        offset: i64,
        limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        self.guard("list_events_by_persona")?;
        let ids = self
            .events_by_persona
            .lock()
            .expect("事件索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let events = self.events.lock().expect("事件锁");
        let rows: Vec<MemoryEvent> = ids
            .iter()
            .filter_map(|id| events.get(id).cloned())
            .collect();
        let start = offset.max(0) as usize;
        let rows = rows.into_iter().skip(start);
        if limit < 0 {
            return Ok(rows.collect());
        }
        Ok(rows.take(limit as usize).collect())
    }

    pub(crate) async fn list_unabsorbed_events(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        self.guard("list_unabsorbed_events")?;
        self.list_events_by_persona(persona_uid, 0, -1).await
    }

    pub(crate) async fn list_events_by_salience(
        &self,
        persona_uid: &str,
        since_ms: i64,
        min_salience: f64,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        self.guard("list_events_by_salience")?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let ids = self
            .events_by_persona
            .lock()
            .expect("事件索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let events = self.events.lock().expect("事件锁");
        let mut rows: Vec<MemoryEvent> = ids
            .iter()
            .filter_map(|id| events.get(id).cloned())
            .filter(|event| event.end >= since_ms && event.salience >= min_salience)
            .collect();
        rows.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.end.cmp(&a.end))
                .then_with(|| b.id.cmp(&a.id))
        });
        rows.truncate(limit as usize);
        Ok(rows)
    }

    pub(crate) async fn list_events_since(
        &self,
        persona_uid: &str,
        since_ms: i64,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        self.guard("list_events_since")?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let ids = self
            .events_by_persona
            .lock()
            .expect("事件索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let events = self.events.lock().expect("事件锁");
        let mut rows: Vec<MemoryEvent> = ids
            .iter()
            .filter_map(|id| events.get(id).cloned())
            .filter(|event| event.end >= since_ms)
            .collect();
        rows.sort_by(|a, b| b.end.cmp(&a.end).then_with(|| b.id.cmp(&a.id)));
        rows.truncate(limit as usize);
        Ok(rows)
    }

    pub(crate) async fn count_events_by_persona(&self, persona_uid: &str) -> RamariaResult<u64> {
        self.guard("count_events_by_persona")?;
        Ok(self
            .events_by_persona
            .lock()
            .expect("事件索引锁")
            .get(persona_uid)
            .map(|ids| ids.len() as u64)
            .unwrap_or(0))
    }

    pub(crate) async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()> {
        self.guard("mark_events_absorbed")?;
        {
            let mut events = self.events.lock().expect("事件锁");
            for id in event_ids {
                if let Some(event) = events.get_mut(id) {
                    event.absorbed = 1;
                }
            }
        }
        for ids in self
            .events_by_persona
            .lock()
            .expect("事件索引锁")
            .values_mut()
        {
            ids.retain(|id| !event_ids.contains(id));
        }
        Ok(())
    }

    pub(crate) async fn aggregate_persona_event_priors(
        &self,
        _exclude_persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaEventAggregate>> {
        self.guard("aggregate_persona_event_priors")?;
        if let Some(message) = self
            .persona_event_aggregate_error
            .lock()
            .expect("聚合锁")
            .clone()
        {
            return Err(RamariaError::storage(message));
        }
        Ok(self
            .persona_event_aggregates
            .lock()
            .expect("聚合锁")
            .clone()
            .unwrap_or_default())
    }

    pub(crate) async fn save_event_relation(&self, rel: &EventRelation) -> RamariaResult<i64> {
        self.guard("save_event_relation")?;
        let id = self
            .relation_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = rel.clone();
        stored.id = id;
        self.event_relations.lock().expect("关系锁").push(stored);
        Ok(id)
    }

    pub(crate) async fn list_event_relations_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<EventRelation>> {
        self.guard("list_event_relations_by_persona")?;
        let events = self.events.lock().expect("事件锁");
        Ok(self
            .event_relations
            .lock()
            .expect("关系锁")
            .iter()
            .filter(|rel| {
                events
                    .get(&rel.from_id)
                    .is_some_and(|event| event.persona_uid == persona_uid)
            })
            .cloned()
            .collect())
    }

    pub(crate) async fn save_event_source(
        &self,
        event_id: i64,
        l1_id: Uuid,
        weight: f64,
    ) -> RamariaResult<()> {
        self.guard("save_event_source")?;
        let id = self
            .event_source_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.event_sources
            .lock()
            .expect("来源锁")
            .push(EventSource {
                id,
                event_id,
                l1_id,
                weight,
            });
        Ok(())
    }

    pub(crate) async fn save_event_batch(
        &self,
        batch: &EventBatchWrite,
    ) -> RamariaResult<Vec<i64>> {
        self.guard("save_event_batch")?;
        for (index, _, _) in &batch.sources {
            if *index >= batch.events.len() {
                return Err(RamariaError::validation(format!(
                    "事件来源下标越界: {index} >= {}",
                    batch.events.len()
                )));
            }
        }
        for (from, to, _, _) in &batch.relations {
            if *from >= batch.events.len() || *to >= batch.events.len() {
                return Err(RamariaError::validation(format!(
                    "事件关系下标越界: {from}/{to} >= {}",
                    batch.events.len()
                )));
            }
        }
        let mut ids = Vec::with_capacity(batch.events.len());
        for event in &batch.events {
            ids.push(self.save_event(event).await?);
        }
        for (index, l1_id, weight) in &batch.sources {
            self.save_event_source(ids[*index], *l1_id, *weight).await?;
        }
        for (from, to, kind, weight) in &batch.relations {
            let mut relation = EventRelation::new(ids[*from], ids[*to], *kind);
            relation.weight = *weight;
            self.save_event_relation(&relation).await?;
        }
        self.mark_l1_absorbed(&batch.absorbed_l1_ids).await?;
        Ok(ids)
    }

    pub(crate) async fn list_event_sources_by_event(
        &self,
        event_id: i64,
    ) -> RamariaResult<Vec<EventSource>> {
        self.guard("list_event_sources_by_event")?;
        Ok(self
            .event_sources
            .lock()
            .expect("来源锁")
            .iter()
            .filter(|source| source.event_id == event_id)
            .cloned()
            .collect())
    }

    pub(crate) async fn list_event_session_map(
        &self,
        event_ids: &[i64],
    ) -> RamariaResult<HashMap<i64, Uuid>> {
        self.guard("list_event_session_map")?;
        let sources = self.event_sources.lock().expect("来源锁");
        let l1_list = self.l1_list.lock().expect("L1 列表锁");
        let mut map = HashMap::new();
        for event_id in event_ids {
            let mut candidates: Vec<&EventSource> = sources
                .iter()
                .filter(|source| source.event_id == *event_id)
                .collect();
            candidates.sort_by(|a, b| {
                b.weight
                    .partial_cmp(&a.weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.l1_id.cmp(&b.l1_id))
            });
            if let Some(source) = candidates.first() {
                let session = l1_list
                    .iter()
                    .find(|(_, entries)| entries.iter().any(|entry| entry.id == source.l1_id))
                    .map(|(session_id, _)| *session_id);
                if let Some(session_id) = session {
                    map.insert(*event_id, session_id);
                }
            }
        }
        Ok(map)
    }
}
