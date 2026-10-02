//! crates/ramaria-storage/src/backend/infrastructure.rs - Ramaria 存储后端 StoreInfrastructure 实现模块
//!
//! 设计特点:
//! - 实现 `StoreInfrastructure`：关键词倒排引用、隐私确认、后端配置、schema/索引版本、后台任务、设置、L2 指纹、行为规则、反馈日志
//! - 纯代理：调用转发到 `repo` 对应子模块，SQL 逻辑集中在一处
//! - 版本与设置方法保持查询-写入分离，供索引一致性检查复用
//! - 所有可恢复错误由 repo 层统一转换为 RamariaError::Storage

use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{IndexCorpusStamp, StoreInfrastructure};
use ramaria_core::types::{BackendConfig, MemoryEvent, PrivacyConsent, now_ms};

use super::SqliteStorage;
use crate::repo;

#[async_trait::async_trait]
impl StoreInfrastructure for SqliteStorage {
    // =========================================================
    // Keyword Refs（关键词倒排索引）
    // =========================================================
    async fn insert_keyword_ref(
        &self,
        keyword_id: &str,
        doc_type: &str,
        doc_id: &str,
        persona_uid: &str,
        weight: f64,
    ) -> RamariaResult<()> {
        repo::keyword::insert_ref(
            &self.pool,
            keyword_id,
            doc_type,
            doc_id,
            persona_uid,
            weight,
        )
        .await
    }

    // =========================================================
    // Privacy Consent（隐私确认）
    // =========================================================
    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()> {
        repo::privacy_consent::save(&self.pool, consent).await
    }
    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        repo::privacy_consent::get_by_provider(&self.pool, provider, base_url).await
    }

    // =========================================================
    // Backend Config（后端配置）
    // =========================================================
    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()> {
        repo::backend_config::upsert(&self.pool, config).await
    }
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        repo::backend_config::get(&self.pool).await
    }

    // =========================================================
    // 索引一致性（schema / index 版本）
    // =========================================================
    async fn get_schema_version(&self) -> RamariaResult<i32> {
        repo::schema_meta::get_schema_version(&self.pool).await
    }
    async fn get_index_version(&self) -> RamariaResult<i32> {
        repo::schema_meta::get_index_version(&self.pool).await
    }
    async fn set_index_version(&self, version: i32) -> RamariaResult<()> {
        repo::schema_meta::set_index_version(&self.pool, version).await
    }
    async fn index_corpus_stamp(&self) -> RamariaResult<Option<IndexCorpusStamp>> {
        Ok(Some(
            repo::schema_meta::index_corpus_stamp(&self.pool).await?,
        ))
    }

    // =========================================================
    // Background Jobs（后台任务）
    // =========================================================
    async fn create_background_job(
        &self,
        job_type: &str,
        payload: Option<&str>,
    ) -> RamariaResult<i64> {
        repo::background_jobs::create(&self.pool, job_type, payload).await
    }
    async fn update_job_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> RamariaResult<()> {
        repo::background_jobs::update_status(&self.pool, id, status, error).await
    }
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        repo::background_jobs::list_pending(&self.pool).await
    }

    async fn claim_pending_job(&self, id: i64) -> RamariaResult<bool> {
        repo::background_jobs::claim_pending(&self.pool, id).await
    }

    // =========================================================
    // Settings（全局运行配置）
    // =========================================================
    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> {
        repo::settings::get(&self.pool, key).await
    }
    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> {
        repo::settings::set(&self.pool, key, value).await
    }
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        repo::settings::list_all(&self.pool).await
    }

    // =========================================================
    // L2 聚类去重指纹（v1.5 三层生成缓存 C）
    // =========================================================

    async fn l2_fingerprint_exists(
        &self,
        persona_uid: &str,
        fingerprint: &str,
    ) -> RamariaResult<bool> {
        repo::l2_fingerprint::exists(&self.pool, persona_uid, fingerprint).await
    }

    async fn save_l2_fingerprint(&self, persona_uid: &str, fingerprint: &str) -> RamariaResult<()> {
        repo::l2_fingerprint::insert(&self.pool, persona_uid, fingerprint, now_ms()).await
    }

    /// 查询 persona 最近事件（按 created_at 倒序，供相似度去重比对）。
    async fn list_recent_events(
        &self,
        persona_uid: &str,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        repo::events::list_recent_by_persona(&self.pool, persona_uid, limit).await
    }

    // =========================================================
    // 行为规则
    // =========================================================

    async fn save_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<i64> {
        repo::behavior_rules::save(&self.pool, rule).await
    }

    async fn get_behavior_rule(&self, id: i64) -> RamariaResult<Option<BehaviorRule>> {
        repo::behavior_rules::get(&self.pool, id).await
    }

    async fn list_behavior_rules_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<BehaviorRule>> {
        repo::behavior_rules::list_by_persona(&self.pool, persona_uid).await
    }

    async fn update_behavior_rule(&self, rule: &BehaviorRule) -> RamariaResult<()> {
        repo::behavior_rules::update(&self.pool, rule).await
    }

    async fn delete_behavior_rule(&self, id: i64) -> RamariaResult<()> {
        repo::behavior_rules::delete(&self.pool, id).await
    }

    async fn set_rule_enabled(&self, id: i64, enabled: bool) -> RamariaResult<()> {
        repo::behavior_rules::set_enabled(&self.pool, id, enabled).await
    }

    // =========================================================
    // 反馈日志
    // =========================================================

    async fn save_feedback_log(&self, log: &FeedbackLog) -> RamariaResult<i64> {
        repo::feedback_log::save(&self.pool, log).await
    }

    async fn list_feedback_logs_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<FeedbackLog>> {
        repo::feedback_log::list_by_persona(&self.pool, persona_uid).await
    }
}
