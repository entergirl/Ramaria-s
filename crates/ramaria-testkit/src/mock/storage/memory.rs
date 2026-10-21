//! crates/ramaria-testkit/src/mock/storage/memory.rs - MockStorage 的 L1 记忆域实现
//!
//! 设计特点:
//! - 会话维度与 persona 维度双索引，写入时同步维护
//! - 吸收标记同时作用于两份索引；无主 L1 只按会话维度存放
//! - 未吸收查询保持服务层 mock 口径（空列表）：索引加载走无主通道，避免有主 L1 污染检索
//! - 最近 L1 按 persona 索引顺序截断，供上下文注入的回退路径使用

use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemoryL1;
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()> {
        self.guard("save_memory_l1")?;
        self.l1_list
            .lock()
            .expect("L1 列表锁")
            .entry(memory.session_id)
            .or_default()
            .push(memory.clone());
        if let Some(uid) = memory.persona_uid.clone() {
            self.l1_by_persona
                .lock()
                .expect("L1 索引锁")
                .entry(uid)
                .or_default()
                .push(memory.clone());
        }
        Ok(())
    }

    pub(crate) async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        self.guard("list_memory_l1")?;
        Ok(self
            .l1_list
            .lock()
            .expect("L1 列表锁")
            .get(&session_id)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        self.guard("get_memory_l1")?;
        {
            let by_persona = self.l1_by_persona.lock().expect("L1 索引锁");
            if let Some(found) = by_persona.values().flatten().find(|l| l.id == id).cloned() {
                return Ok(Some(found));
            }
        }
        let list = self.l1_list.lock().expect("L1 列表锁");
        Ok(list.values().flatten().find(|l| l.id == id).cloned())
    }

    pub(crate) async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()> {
        self.guard("mark_l1_absorbed")?;
        for list in self.l1_list.lock().expect("L1 列表锁").values_mut() {
            for entry in list.iter_mut() {
                if l1_ids.contains(&entry.id) {
                    entry.absorbed = true;
                }
            }
        }
        for list in self.l1_by_persona.lock().expect("L1 索引锁").values_mut() {
            for entry in list.iter_mut() {
                if l1_ids.contains(&entry.id) {
                    entry.absorbed = true;
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn delete_memory_l1_by_session(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<usize> {
        self.guard("delete_memory_l1_by_session")?;
        let removed: Vec<MemoryL1> = {
            let mut list = self.l1_list.lock().expect("L1 列表锁");
            let mut kept = Vec::new();
            let mut removed = Vec::new();
            if let Some(entries) = list.remove(&session_id) {
                for entry in entries {
                    if entry.persona_uid.is_none() {
                        removed.push(entry);
                    } else {
                        kept.push(entry);
                    }
                }
            }
            if !kept.is_empty() {
                list.insert(session_id, kept);
            }
            removed
        };
        if !removed.is_empty() {
            let ids: Vec<Uuid> = removed.iter().map(|entry| entry.id).collect();
            for list in self.l1_by_persona.lock().expect("L1 索引锁").values_mut() {
                list.retain(|entry| !ids.contains(&entry.id));
            }
        }
        Ok(removed.len())
    }

    pub(crate) async fn list_unabsorbed_l1(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<MemoryL1>> {
        self.guard("list_unabsorbed_l1")?;
        Ok(Vec::new())
    }

    pub(crate) async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> {
        self.guard("list_unabsorbed_l1_unbound")?;
        Ok(self
            .l1_list
            .lock()
            .expect("L1 列表锁")
            .values()
            .flatten()
            .filter(|entry| entry.persona_uid.is_none())
            .cloned()
            .collect())
    }

    pub(crate) async fn assign_l1_persona_uid(
        &self,
        l1_ids: &[Uuid],
        persona_uid: &str,
    ) -> RamariaResult<usize> {
        self.guard("assign_l1_persona_uid")?;
        let mut updated = 0;
        let mut assigned: Vec<MemoryL1> = Vec::new();
        {
            let mut list = self.l1_list.lock().expect("L1 列表锁");
            for entries in list.values_mut() {
                for entry in entries.iter_mut() {
                    if l1_ids.contains(&entry.id) && entry.persona_uid.is_none() && !entry.absorbed
                    {
                        entry.persona_uid = Some(persona_uid.to_string());
                        assigned.push(entry.clone());
                        updated += 1;
                    }
                }
            }
        }
        if !assigned.is_empty() {
            self.l1_by_persona
                .lock()
                .expect("L1 索引锁")
                .entry(persona_uid.to_string())
                .or_default()
                .extend(assigned);
        }
        Ok(updated)
    }

    pub(crate) async fn list_recent_l1_by_persona(
        &self,
        persona_uid: &str,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryL1>> {
        self.guard("list_recent_l1_by_persona")?;
        Ok(self
            .l1_by_persona
            .lock()
            .expect("L1 索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit as usize)
            .collect())
    }

    pub(crate) async fn touch_l1(&self, l1_ids: &[Uuid], now_ms: i64) -> RamariaResult<()> {
        self.guard("touch_l1")?;
        for list in self.l1_list.lock().expect("L1 列表锁").values_mut() {
            for entry in list.iter_mut() {
                if l1_ids.contains(&entry.id) {
                    entry.last_accessed_at = Some(now_ms);
                }
            }
        }
        for list in self.l1_by_persona.lock().expect("L1 索引锁").values_mut() {
            for entry in list.iter_mut() {
                if l1_ids.contains(&entry.id) {
                    entry.last_accessed_at = Some(now_ms);
                }
            }
        }
        Ok(())
    }
}
