//! crates/ramaria-cli/tests/common/storage_infra.rs - MockStorage 的 StoreInfrastructure 实现
//!
//! 设计特点:
//! - 基础设施契约：迁移 / 索引版本 / 设置 / 后端配置 / 隐私同意等
//! - 作为 common 的子模块，可直接访问 MockStorage 私有字段（零放宽）
//! - 仅服务 CLI 集成测试，不触碰真实存储

use super::*;
use async_trait::async_trait;
use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StoreInfrastructure;
use ramaria_core::types::{BackendConfig, PrivacyConsent};

#[async_trait]
impl StoreInfrastructure for MockStorage {
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
        _job_type: &str,
        _payload: Option<&str>,
    ) -> RamariaResult<i64> {
        Ok(1)
    }

    async fn update_job_status(
        &self,
        _id: i64,
        _status: &str,
        _error: Option<&str>,
    ) -> RamariaResult<()> {
        Ok(())
    }

    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        Ok(Vec::new())
    }

    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> {
        Ok(self.settings.lock().unwrap().get(key).cloned())
    }

    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> {
        self.settings
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        Ok(self
            .settings
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    // =========================================================
    // Keyword Refs (Mock 空实现)
    // =========================================================

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

    // -- 行为规则（CLI 测试） --

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
