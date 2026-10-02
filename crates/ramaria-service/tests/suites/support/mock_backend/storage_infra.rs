//! crates/ramaria-service/tests/suites/support/mock_backend/storage_infra.rs - MockStorage 的 StoreInfrastructure 实现
//!
//! 设计特点:
//! - 覆盖基础设施面：最近事件、隐私确认、后端配置、索引版本、后台任务、setting、关键词引用
//! - 隐私确认按 (provider, base_url) 倒序取最近一条，与线上 provider 确认口径一致
//! - 后台任务以三元组 (job_type, payload, status) 存于内存，`list_pending_jobs` 按 id 升序返回
//! - 行为规则维护 persona 维度索引，删除时同步清理索引项
//! - 反馈日志仅支持追加与按 persona 过滤读取，不提供变更语义

use std::sync::atomic::Ordering;

use async_trait::async_trait;
use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{BackendConfig, MemoryEvent, PrivacyConsent};

use super::MockStorage;

#[async_trait]
impl StoreInfrastructure for MockStorage {
    async fn list_recent_events(
        &self,
        persona_uid: &str,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        let mut events = self
            .list_events_by_persona(persona_uid, 0, i64::MAX)
            .await?;
        events.sort_by_key(|e| std::cmp::Reverse(e.id));
        events.truncate(limit as usize);
        Ok(events)
    }

    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()> {
        self.privacy_consents.lock().unwrap().push(consent.clone());
        Ok(())
    }

    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        Ok(self
            .privacy_consents
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|c| c.provider.as_str() == provider && c.base_url == base_url)
            .cloned())
    }

    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()> {
        *self.backend_config.lock().unwrap() = Some(config.clone());
        Ok(())
    }

    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        Ok(self.backend_config.lock().unwrap().clone())
    }

    async fn get_schema_version(&self) -> RamariaResult<i32> {
        Ok(1)
    }

    async fn get_index_version(&self) -> RamariaResult<i32> {
        Ok(*self.index_version.lock().unwrap())
    }

    async fn set_index_version(&self, version: i32) -> RamariaResult<()> {
        *self.index_version.lock().unwrap() = version;
        Ok(())
    }

    // ---- 基础设施方法 ----

    async fn create_background_job(
        &self,
        job_type: &str,
        payload: Option<&str>,
    ) -> RamariaResult<i64> {
        let id = self.job_seq.fetch_add(1, Ordering::Relaxed);
        self.jobs.lock().unwrap().insert(
            id,
            (
                job_type.to_string(),
                payload.map(|p| p.to_string()),
                "pending".to_string(),
            ),
        );
        Ok(id)
    }

    async fn update_job_status(
        &self,
        id: i64,
        status: &str,
        _error: Option<&str>,
    ) -> RamariaResult<()> {
        if let Some(entry) = self.jobs.lock().unwrap().get_mut(&id) {
            entry.2 = status.to_string();
        }
        Ok(())
    }

    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        let mut pending: Vec<(i64, String, Option<String>)> = self
            .jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (_, _, status))| status == "pending")
            .map(|(id, (job_type, payload, _))| (*id, job_type.clone(), payload.clone()))
            .collect();
        pending.sort_by_key(|(id, _, _)| *id);
        Ok(pending)
    }

    async fn get_setting(&self, _key: &str) -> RamariaResult<Option<String>> {
        Ok(None)
    }

    async fn set_setting(&self, _key: &str, _value: &str) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn insert_keyword_ref(
        &self,
        _keyword_id: &str,
        _doc_type: &str,
        _doc_id: &str,
        _persona_uid: &str,
        _weight: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }

    // -- 行为规则 --

    async fn save_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<i64> {
        let id = self
            .rule_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut r = rule.clone();
        r.id = id;
        self.behavior_rules.lock().unwrap().insert(id, r.clone());
        self.rules_by_persona
            .lock()
            .unwrap()
            .entry(r.persona_uid.clone())
            .or_default()
            .push(id);
        Ok(id)
    }

    async fn get_behavior_rule(&self, id: i64) -> RamariaResult<Option<BehaviorRule>> {
        Ok(self.behavior_rules.lock().unwrap().get(&id).cloned())
    }

    async fn list_behavior_rules_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<BehaviorRule>> {
        let ids = self
            .rules_by_persona
            .lock()
            .unwrap()
            .get(persona_uid)
            .cloned()
            .unwrap_or_default();
        let rules = self.behavior_rules.lock().unwrap();
        Ok(ids.iter().filter_map(|id| rules.get(id).cloned()).collect())
    }

    async fn update_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<()> {
        self.behavior_rules
            .lock()
            .unwrap()
            .insert(rule.id, rule.clone());
        Ok(())
    }

    async fn delete_behavior_rule(&self, id: i64) -> RamariaResult<()> {
        let removed = self.behavior_rules.lock().unwrap().remove(&id);
        if let Some(rule) = removed {
            let mut by_persona = self.rules_by_persona.lock().unwrap();
            if let Some(ids) = by_persona.get_mut(&rule.persona_uid) {
                ids.retain(|&x| x != id);
            }
        }
        Ok(())
    }

    async fn set_rule_enabled(&self, id: i64, enabled: bool) -> RamariaResult<()> {
        let mut rules = self.behavior_rules.lock().unwrap();
        if let Some(rule) = rules.get_mut(&id) {
            rule.enabled = enabled;
        }
        Ok(())
    }

    // -- 反馈日志 --

    async fn save_feedback_log(&self, log: &FeedbackLog) -> RamariaResult<i64> {
        let id = self
            .feedback_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut l = log.clone();
        l.id = id;
        self.feedback_logs.lock().unwrap().push(l);
        Ok(id)
    }

    async fn list_feedback_logs_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<FeedbackLog>> {
        Ok(self
            .feedback_logs
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.persona_uid == persona_uid)
            .cloned()
            .collect())
    }
}
