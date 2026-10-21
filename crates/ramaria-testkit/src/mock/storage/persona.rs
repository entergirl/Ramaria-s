//! crates/ramaria-testkit/src/mock/storage/persona.rs - MockStorage 的人格与画像域实现
//!
//! 设计特点:
//! - 人格按 uid 主键存于内存，id 自增回填
//! - 更新只覆盖传入的可选字段，未传保持旧值
//! - 示例按 persona 分组，selected 与全量查询分开
//! - 快照写入 current 前归档同 (persona, category) 旧 current

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::types::{ClusterSnapshot, Persona, PersonaExample};

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64> {
        self.guard("create_persona")?;
        let id = {
            let mut seq = self.persona_seq.lock().expect("人格序号锁");
            *seq += 1;
            *seq
        };
        let mut stored = persona.clone();
        stored.id = id;
        self.personas
            .lock()
            .expect("人格锁")
            .insert(persona.uid.clone(), stored);
        Ok(id)
    }

    pub(crate) async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>> {
        self.guard("get_persona_by_uid")?;
        Ok(self.personas.lock().expect("人格锁").get(uid).cloned())
    }

    pub(crate) async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        self.guard("list_personas")?;
        Ok(self
            .personas
            .lock()
            .expect("人格锁")
            .values()
            .cloned()
            .collect())
    }

    pub(crate) async fn update_persona(
        &self,
        uid: &str,
        name: &str,
        avatar: Option<&str>,
        config: Option<&str>,
        description: Option<&str>,
    ) -> RamariaResult<()> {
        self.guard("update_persona")?;
        let mut personas = self.personas.lock().expect("人格锁");
        match personas.get_mut(uid) {
            Some(persona) => {
                persona.name = name.to_string();
                if let Some(avatar) = avatar {
                    persona.avatar = Some(avatar.to_string());
                }
                if let Some(config) = config {
                    persona.config = Some(config.to_string());
                }
                if let Some(description) = description {
                    persona.description = Some(description.to_string());
                }
                Ok(())
            }
            None => Err(RamariaError::storage(format!("persona 不存在: uid={uid}"))),
        }
    }

    pub(crate) async fn save_example(&self, example: &PersonaExample) -> RamariaResult<i64> {
        self.guard("save_example")?;
        let mut id = example.id;
        if id <= 0 {
            id = self
                .examples
                .lock()
                .expect("示例锁")
                .values()
                .map(Vec::len)
                .sum::<usize>() as i64
                + 1;
        }
        let mut stored = example.clone();
        stored.id = id;
        self.examples
            .lock()
            .expect("示例锁")
            .entry(stored.persona_uid.clone())
            .or_default()
            .push(stored);
        Ok(id)
    }

    pub(crate) async fn list_selected_examples(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaExample>> {
        self.guard("list_selected_examples")?;
        Ok(self
            .examples
            .lock()
            .expect("示例锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|example| example.selected)
            .collect())
    }

    pub(crate) async fn list_all_examples(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaExample>> {
        self.guard("list_all_examples")?;
        Ok(self
            .examples
            .lock()
            .expect("示例锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) async fn find_example_by_pair(
        &self,
        persona_uid: &str,
        partner: &str,
        reply: &str,
    ) -> RamariaResult<Option<PersonaExample>> {
        self.guard("find_example_by_pair")?;
        Ok(self
            .examples
            .lock()
            .expect("示例锁")
            .get(persona_uid)
            .and_then(|list| {
                list.iter()
                    .find(|example| example.partner == partner && example.reply == reply)
            })
            .cloned())
    }

    pub(crate) async fn save_cluster_snapshot(
        &self,
        snapshot: &ClusterSnapshot,
    ) -> RamariaResult<i64> {
        self.guard("save_cluster_snapshot")?;
        let id = self
            .snapshot_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = snapshot.clone();
        stored.id = id;
        let mut snapshots = self.cluster_snapshots.lock().expect("快照锁");
        if stored.is_current {
            for old in snapshots.iter_mut() {
                if old.persona_uid == stored.persona_uid
                    && old.category == stored.category
                    && old.is_current
                {
                    old.is_current = false;
                }
            }
        }
        snapshots.push(stored);
        Ok(id)
    }

    pub(crate) async fn get_current_snapshots(
        &self,
        persona_uid: &str,
        category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        self.guard("get_current_snapshots")?;
        Ok(self
            .cluster_snapshots
            .lock()
            .expect("快照锁")
            .iter()
            .filter(|s| s.persona_uid == persona_uid && s.category == category && s.is_current)
            .cloned()
            .collect())
    }

    pub(crate) async fn get_all_snapshots_with_embeddings(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        self.guard("get_all_snapshots_with_embeddings")?;
        Ok(self
            .cluster_snapshots
            .lock()
            .expect("快照锁")
            .iter()
            .filter(|s| s.persona_uid == persona_uid && s.semantic_label_embedding.is_some())
            .cloned()
            .collect())
    }
}
