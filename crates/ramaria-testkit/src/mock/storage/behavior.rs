//! crates/ramaria-testkit/src/mock/storage/behavior.rs - MockStorage 的基础设施与行为规则域实现
//!
//! 设计特点:
//! - 隐私确认按 (provider, base_url) 倒序取最近一条，与线上 provider 确认口径一致
//! - 语料统计戳按真实口径即时聚合（L1 / 事件 / utt / persona 计数与最新时间）
//! - 记忆向量按 (doc_type, doc_id) 主键覆盖写，可按模型清理与孤儿回收
//! - 后台任务三元组存档，抢占为条件更新；setting 读写真落内存
//! - 行为规则维护 persona 索引，删除同步清理；反馈日志只追加

use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{IndexCorpusStamp, SETTING_BM25_INDEX_VERSION};
use ramaria_core::types::{BackendConfig, MemoryEvent, MemoryVector, PrivacyConsent};
use uuid::Uuid;

use super::MockStorage;

impl MockStorage {
    pub(crate) async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()> {
        self.guard("save_privacy_consent")?;
        self.privacy_consents
            .lock()
            .expect("隐私确认锁")
            .push(consent.clone());
        Ok(())
    }

    pub(crate) async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        self.guard("get_privacy_consent")?;
        Ok(self
            .privacy_consents
            .lock()
            .expect("隐私确认锁")
            .iter()
            .rev()
            .find(|consent| consent.provider.as_str() == provider && consent.base_url == base_url)
            .cloned())
    }

    pub(crate) async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()> {
        self.guard("save_backend_config")?;
        *self.backend_config.lock().expect("后端配置锁") = Some(config.clone());
        Ok(())
    }

    pub(crate) async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        self.guard("get_backend_config")?;
        Ok(self.backend_config.lock().expect("后端配置锁").clone())
    }

    pub(crate) async fn get_schema_version(&self) -> RamariaResult<i32> {
        self.guard("get_schema_version")?;
        Ok(1)
    }

    pub(crate) async fn index_corpus_stamp(&self) -> RamariaResult<Option<IndexCorpusStamp>> {
        self.guard("index_corpus_stamp")?;
        let (l1_count, l1_max_created_at) = {
            let list = self.l1_list.lock().expect("L1 列表锁");
            (
                list.values().map(Vec::len).sum::<usize>() as i64,
                list.values()
                    .flatten()
                    .map(|entry| entry.created_at)
                    .max()
                    .unwrap_or(0),
            )
        };
        let (event_count, event_max_created_at) = {
            let events = self.events.lock().expect("事件锁");
            (
                events.len() as i64,
                events
                    .values()
                    .map(|event| event.created_at)
                    .max()
                    .unwrap_or(0),
            )
        };
        let utt_count = self
            .utt_blocks
            .lock()
            .expect("utt 锁")
            .values()
            .map(Vec::len)
            .sum::<usize>() as i64;
        let persona_count = self.personas.lock().expect("人格锁").len() as i64;
        Ok(Some(IndexCorpusStamp {
            l1_count,
            l1_max_created_at,
            event_count,
            event_max_created_at,
            utt_count,
            persona_count,
        }))
    }

    pub(crate) async fn upsert_memory_vector(&self, vector: &MemoryVector) -> RamariaResult<()> {
        self.guard("upsert_memory_vector")?;
        self.memory_vectors.lock().expect("向量锁").insert(
            (vector.doc_type.clone(), vector.doc_id.clone()),
            vector.clone(),
        );
        Ok(())
    }

    pub(crate) async fn upsert_memory_vectors(
        &self,
        vectors: &[MemoryVector],
    ) -> RamariaResult<()> {
        self.guard("upsert_memory_vectors")?;
        let mut store = self.memory_vectors.lock().expect("向量锁");
        for vector in vectors {
            store.insert(
                (vector.doc_type.clone(), vector.doc_id.clone()),
                vector.clone(),
            );
        }
        Ok(())
    }

    pub(crate) async fn load_memory_vectors(
        &self,
        model: &str,
    ) -> RamariaResult<Vec<MemoryVector>> {
        self.guard("load_memory_vectors")?;
        Ok(self
            .memory_vectors
            .lock()
            .expect("向量锁")
            .values()
            .filter(|vector| vector.model == model)
            .cloned()
            .collect())
    }

    pub(crate) async fn delete_memory_vector(
        &self,
        doc_type: &str,
        doc_id: &str,
    ) -> RamariaResult<()> {
        self.guard("delete_memory_vector")?;
        self.memory_vectors
            .lock()
            .expect("向量锁")
            .remove(&(doc_type.to_string(), doc_id.to_string()));
        Ok(())
    }

    pub(crate) async fn delete_memory_vectors_by_model(&self, model: &str) -> RamariaResult<()> {
        self.guard("delete_memory_vectors_by_model")?;
        self.memory_vectors
            .lock()
            .expect("向量锁")
            .retain(|_, vector| vector.model != model);
        Ok(())
    }

    pub(crate) async fn prune_orphan_memory_vectors(&self) -> RamariaResult<u64> {
        self.guard("prune_orphan_memory_vectors")?;
        let l1_ids: Vec<Uuid> = self
            .l1_list
            .lock()
            .expect("L1 列表锁")
            .values()
            .flatten()
            .map(|entry| entry.id)
            .collect();
        let event_ids: Vec<i64> = self
            .events
            .lock()
            .expect("事件锁")
            .keys()
            .copied()
            .collect();
        let utt_ids: Vec<i64> = self
            .utt_blocks
            .lock()
            .expect("utt 锁")
            .values()
            .flatten()
            .map(|block| block.id)
            .collect();
        let mut vectors = self.memory_vectors.lock().expect("向量锁");
        let before = vectors.len();
        vectors.retain(|(doc_type, doc_id), _| match doc_type.as_str() {
            "l1" => Uuid::parse_str(doc_id)
                .map(|id| l1_ids.contains(&id))
                .unwrap_or(true),
            "l2" => doc_id
                .parse::<i64>()
                .map(|id| event_ids.contains(&id))
                .unwrap_or(true),
            "utt" => doc_id
                .parse::<i64>()
                .map(|id| utt_ids.contains(&id))
                .unwrap_or(true),
            _ => true,
        });
        Ok((before - vectors.len()) as u64)
    }

    pub(crate) async fn create_background_job(
        &self,
        job_type: &str,
        payload: Option<&str>,
    ) -> RamariaResult<i64> {
        self.guard("create_background_job")?;
        let id = self
            .job_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.jobs.lock().expect("任务锁").insert(
            id,
            (
                job_type.to_string(),
                payload.map(|p| p.to_string()),
                "pending".to_string(),
            ),
        );
        Ok(id)
    }

    pub(crate) async fn update_job_status(
        &self,
        id: i64,
        status: &str,
        _error: Option<&str>,
    ) -> RamariaResult<()> {
        self.guard("update_job_status")?;
        if let Some(entry) = self.jobs.lock().expect("任务锁").get_mut(&id) {
            entry.2 = status.to_string();
        }
        Ok(())
    }

    pub(crate) async fn list_pending_jobs(
        &self,
    ) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        self.guard("list_pending_jobs")?;
        let mut pending: Vec<(i64, String, Option<String>)> = self
            .jobs
            .lock()
            .expect("任务锁")
            .iter()
            .filter(|(_, (_, _, status))| status == "pending")
            .map(|(id, (job_type, payload, _))| (*id, job_type.clone(), payload.clone()))
            .collect();
        pending.sort_by_key(|(id, _, _)| *id);
        Ok(pending)
    }

    pub(crate) async fn claim_pending_job(&self, id: i64) -> RamariaResult<bool> {
        self.guard("claim_pending_job")?;
        let mut jobs = self.jobs.lock().expect("任务锁");
        match jobs.get_mut(&id) {
            Some(entry) if entry.2 == "pending" => {
                entry.2 = "running".to_string();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(crate) async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> {
        self.guard("get_setting")?;
        Ok(self.settings.lock().expect("设置锁").get(key).cloned())
    }

    pub(crate) async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> {
        self.guard("set_setting")?;
        self.settings
            .lock()
            .expect("设置锁")
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    pub(crate) async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        self.guard("list_settings")?;
        let settings = self.settings.lock().expect("设置锁");
        let mut list: Vec<(String, String)> = settings
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(list)
    }

    pub(crate) async fn get_bm25_index_version(&self) -> RamariaResult<i32> {
        self.guard("get_bm25_index_version")?;
        Ok(self.bm25_version.load(std::sync::atomic::Ordering::Acquire))
    }

    pub(crate) async fn set_bm25_index_version(&self, version: i32) -> RamariaResult<()> {
        self.guard("set_bm25_index_version")?;
        self.bm25_version
            .store(version, std::sync::atomic::Ordering::Release);
        self.settings
            .lock()
            .expect("设置锁")
            .insert(SETTING_BM25_INDEX_VERSION.to_string(), version.to_string());
        Ok(())
    }

    pub(crate) async fn l2_fingerprint_exists(
        &self,
        persona_uid: &str,
        fingerprint: &str,
    ) -> RamariaResult<bool> {
        self.guard("l2_fingerprint_exists")?;
        Ok(self
            .l2_fingerprints
            .lock()
            .expect("指纹锁")
            .contains(&(persona_uid.to_string(), fingerprint.to_string())))
    }

    pub(crate) async fn save_l2_fingerprint(
        &self,
        persona_uid: &str,
        fingerprint: &str,
    ) -> RamariaResult<()> {
        self.guard("save_l2_fingerprint")?;
        self.l2_fingerprints
            .lock()
            .expect("指纹锁")
            .insert((persona_uid.to_string(), fingerprint.to_string()));
        Ok(())
    }

    pub(crate) async fn list_recent_events(
        &self,
        persona_uid: &str,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        self.guard("list_recent_events")?;
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
            .collect();
        rows.sort_by_key(|event| std::cmp::Reverse(event.id));
        rows.truncate(limit as usize);
        Ok(rows)
    }

    pub(crate) async fn save_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<i64> {
        self.guard("save_behavior_rule")?;
        let id = self
            .rule_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = rule.clone();
        stored.id = id;
        self.behavior_rules
            .lock()
            .expect("规则锁")
            .insert(id, stored.clone());
        self.rules_by_persona
            .lock()
            .expect("规则索引锁")
            .entry(stored.persona_uid)
            .or_default()
            .push(id);
        Ok(id)
    }

    pub(crate) async fn get_behavior_rule(&self, id: i64) -> RamariaResult<Option<BehaviorRule>> {
        self.guard("get_behavior_rule")?;
        Ok(self
            .behavior_rules
            .lock()
            .expect("规则锁")
            .get(&id)
            .cloned())
    }

    pub(crate) async fn list_behavior_rules_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<BehaviorRule>> {
        self.guard("list_behavior_rules_by_persona")?;
        let ids = self
            .rules_by_persona
            .lock()
            .expect("规则索引锁")
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let rules = self.behavior_rules.lock().expect("规则锁");
        Ok(ids.iter().filter_map(|id| rules.get(id).cloned()).collect())
    }

    pub(crate) async fn update_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<()> {
        self.guard("update_behavior_rule")?;
        self.behavior_rules
            .lock()
            .expect("规则锁")
            .insert(rule.id, rule.clone());
        Ok(())
    }

    pub(crate) async fn delete_behavior_rule(&self, id: i64) -> RamariaResult<()> {
        self.guard("delete_behavior_rule")?;
        let removed = self.behavior_rules.lock().expect("规则锁").remove(&id);
        if let Some(rule) = removed {
            let mut index = self.rules_by_persona.lock().expect("规则索引锁");
            if let Some(ids) = index.get_mut(&rule.persona_uid) {
                ids.retain(|existing| *existing != id);
            }
        }
        Ok(())
    }

    pub(crate) async fn set_rule_enabled(&self, id: i64, enabled: bool) -> RamariaResult<()> {
        self.guard("set_rule_enabled")?;
        if let Some(rule) = self.behavior_rules.lock().expect("规则锁").get_mut(&id) {
            rule.enabled = enabled;
        }
        Ok(())
    }

    pub(crate) async fn save_feedback_log(&self, log: &FeedbackLog) -> RamariaResult<i64> {
        self.guard("save_feedback_log")?;
        let id = self
            .feedback_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut stored = log.clone();
        stored.id = id;
        self.feedback_logs.lock().expect("反馈日志锁").push(stored);
        Ok(id)
    }

    pub(crate) async fn list_feedback_logs_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<FeedbackLog>> {
        self.guard("list_feedback_logs_by_persona")?;
        Ok(self
            .feedback_logs
            .lock()
            .expect("反馈日志锁")
            .iter()
            .filter(|log| log.persona_uid == persona_uid)
            .cloned()
            .collect())
    }
}
