//! crates/ramaria-service/src/test_support/storage.rs - Ramaria 服务层测试用可失败存储包装模块
//!
//! 设计特点:
//! - 包装真实 `SqliteStorage`，对 `list_personas` 提供"打开开关即返回存储错误"的能力；
//! - 用于验证索引重建失败路径（告警位置位、旧索引保持可用）；
//! - 其余方法原样转发真实实现，未覆写的方法走 trait 默认实现；
//! - 仅为索引重建路径用例提供失败注入，不承载完整存储语义。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::keyword::PendingAliasRow;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, Persona,
    PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, ProfileField, Session,
    TraitEvidence, TraitStatus,
};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

// =========================================================
// 可失败存储包装（索引重建失败路径用例）
// =========================================================

/// 可注入失败的存储包装（索引重建路径用例专用）。
///
/// 职责:
/// - 包装真实 `SqliteStorage`，对 `list_personas` 提供"打开开关即返回存储错误"的能力，
///   用于验证重建失败路径（告警位置位、旧索引保持可用）；
/// - 其余方法原样转发真实实现；未覆写的方法走 trait 默认实现。
///
/// 边界:
/// - 仅为索引重建路径用例提供失败注入，不承载完整存储语义；
/// - 造数与结果断言使用同一库文件上的真实存储句柄（见 [`super::engine_with_failable_storage`]）。
pub(crate) struct FailableStorage {
    /// 真实存储（转发目标）。
    inner: Arc<SqliteStorage>,
    /// `list_personas` 失败开关（true = 返回存储错误）。
    fail_list_personas: AtomicBool,
    /// `list_keyword_statuses` 调用次数（热路径开销口径断言用）。
    keyword_status_queries: AtomicUsize,
}

impl FailableStorage {
    /// 包装真实存储（失败开关初始关闭）。
    pub(crate) fn new(inner: Arc<SqliteStorage>) -> Self {
        Self {
            inner,
            fail_list_personas: AtomicBool::new(false),
            keyword_status_queries: AtomicUsize::new(0),
        }
    }

    /// 设置 `list_personas` 失败开关（true = 该查询返回存储错误）。
    pub(crate) fn set_fail_list_personas(&self, fail: bool) {
        self.fail_list_personas.store(fail, Ordering::Release);
    }

    /// 获取 `list_keyword_statuses` 调用次数（用于断言每 L1 一次批量查询的口径）。
    pub(crate) fn keyword_status_query_count(&self) -> usize {
        self.keyword_status_queries.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl StoreCrud for FailableStorage {
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        self.inner.create_session(persona_uid).await
    }

    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> {
        self.inner.close_session(session_id).await
    }

    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>> {
        self.inner.get_session(session_id).await
    }

    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        self.inner.list_active_sessions().await
    }

    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        self.inner.list_sessions().await
    }

    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        self.inner.delete_session(session_id).await
    }

    async fn save_message(&self, message: &Message) -> RamariaResult<()> {
        self.inner.save_message(message).await
    }

    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>> {
        self.inner.list_messages(session_id).await
    }

    async fn list_messages_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<Message>> {
        self.inner.list_messages_by_persona(persona_uid).await
    }

    async fn count_messages_by_session(&self) -> RamariaResult<HashMap<Uuid, u32>> {
        self.inner.count_messages_by_session().await
    }

    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()> {
        self.inner.save_memory_l1(memory).await
    }

    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        self.inner.list_memory_l1(session_id).await
    }

    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        self.inner.get_memory_l1(id).await
    }

    async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()> {
        self.inner.mark_l1_absorbed(l1_ids).await
    }

    async fn list_unabsorbed_l1(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        self.inner.list_unabsorbed_l1(persona_uid).await
    }

    async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64> {
        self.inner.create_persona(persona).await
    }

    async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>> {
        self.inner.get_persona_by_uid(uid).await
    }

    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        if self.fail_list_personas.load(Ordering::Acquire) {
            return Err(RamariaError::storage(
                "FailableStorage: list_personas 注入失败（索引重建失败路径用例）",
            ));
        }
        self.inner.list_personas().await
    }

    async fn update_persona(
        &self,
        uid: &str,
        name: &str,
        avatar: Option<&str>,
        config: Option<&str>,
        description: Option<&str>,
    ) -> RamariaResult<()> {
        self.inner
            .update_persona(uid, name, avatar, config, description)
            .await
    }

    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> {
        self.inner.save_event(event).await
    }

    async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        offset: i64,
        limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        self.inner
            .list_events_by_persona(persona_uid, offset, limit)
            .await
    }

    async fn count_events_by_persona(&self, persona_uid: &str) -> RamariaResult<u64> {
        self.inner.count_events_by_persona(persona_uid).await
    }

    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        self.inner.list_unabsorbed_events(persona_uid).await
    }

    async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()> {
        self.inner.mark_events_absorbed(event_ids).await
    }

    async fn save_event_relation(&self, rel: &EventRelation) -> RamariaResult<i64> {
        self.inner.save_event_relation(rel).await
    }

    async fn save_event_source(
        &self,
        event_id: i64,
        l1_id: Uuid,
        weight: f64,
    ) -> RamariaResult<()> {
        self.inner.save_event_source(event_id, l1_id, weight).await
    }

    async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64> {
        self.inner.save_fact(fact).await
    }

    async fn list_facts_by_persona(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        self.inner.list_facts_by_persona(persona_uid, field).await
    }

    async fn save_trait(&self, t: &PersonalityTrait) -> RamariaResult<i64> {
        self.inner.save_trait(t).await
    }

    async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        self.inner.list_traits_by_persona(persona_uid).await
    }

    async fn update_trait_confidence(
        &self,
        id: i64,
        confidence: f64,
        evidence: f64,
        consistency: f64,
    ) -> RamariaResult<()> {
        self.inner
            .update_trait_confidence(id, confidence, evidence, consistency)
            .await
    }

    async fn update_trait_status(&self, id: i64, status: TraitStatus) -> RamariaResult<()> {
        self.inner.update_trait_status(id, status).await
    }

    async fn save_evidence(&self, e: &TraitEvidence) -> RamariaResult<i64> {
        self.inner.save_evidence(e).await
    }

    async fn list_evidence_by_trait(&self, trait_id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        self.inner.list_evidence_by_trait(trait_id).await
    }

    async fn save_example(&self, e: &PersonaExample) -> RamariaResult<i64> {
        self.inner.save_example(e).await
    }

    async fn list_selected_examples(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaExample>> {
        self.inner.list_selected_examples(persona_uid).await
    }

    async fn save_cluster_snapshot(&self, s: &ClusterSnapshot) -> RamariaResult<i64> {
        self.inner.save_cluster_snapshot(s).await
    }

    async fn get_current_snapshots(
        &self,
        persona_uid: &str,
        category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        self.inner
            .get_current_snapshots(persona_uid, category)
            .await
    }

    async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()> {
        self.inner.upsert_keyword(keyword).await
    }

    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        self.inner.list_keywords().await
    }

    async fn list_pending_aliases(&self) -> RamariaResult<Vec<PendingAliasRow>> {
        self.inner.list_pending_aliases().await
    }

    async fn confirm_keyword_alias(&self, alias_id: i64) -> RamariaResult<bool> {
        self.inner.confirm_keyword_alias(alias_id).await
    }

    async fn reject_keyword_alias(&self, alias_id: i64) -> RamariaResult<bool> {
        self.inner.reject_keyword_alias(alias_id).await
    }

    /// 覆写为"计数 + 转发真实查询"（锁定"每 L1 一次批量状态查询"的开销口径）。
    async fn list_keyword_statuses(
        &self,
        keywords: &[String],
    ) -> RamariaResult<Vec<(String, Option<String>)>> {
        self.keyword_status_queries.fetch_add(1, Ordering::Relaxed);
        self.inner.list_keyword_statuses(keywords).await
    }
}

#[async_trait::async_trait]
impl StoreInfrastructure for FailableStorage {
    async fn insert_keyword_ref(
        &self,
        keyword_id: &str,
        doc_type: &str,
        doc_id: &str,
        persona_uid: &str,
        weight: f64,
    ) -> RamariaResult<()> {
        self.inner
            .insert_keyword_ref(keyword_id, doc_type, doc_id, persona_uid, weight)
            .await
    }

    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()> {
        self.inner.save_privacy_consent(consent).await
    }

    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>> {
        self.inner.get_privacy_consent(provider, base_url).await
    }

    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()> {
        self.inner.save_backend_config(config).await
    }

    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        self.inner.get_backend_config().await
    }

    async fn get_schema_version(&self) -> RamariaResult<i32> {
        self.inner.get_schema_version().await
    }

    async fn get_index_version(&self) -> RamariaResult<i32> {
        self.inner.get_index_version().await
    }

    async fn set_index_version(&self, version: i32) -> RamariaResult<()> {
        self.inner.set_index_version(version).await
    }

    async fn create_background_job(
        &self,
        job_type: &str,
        payload: Option<&str>,
    ) -> RamariaResult<i64> {
        self.inner.create_background_job(job_type, payload).await
    }

    async fn update_job_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> RamariaResult<()> {
        self.inner.update_job_status(id, status, error).await
    }

    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        self.inner.list_pending_jobs().await
    }

    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> {
        self.inner.get_setting(key).await
    }

    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> {
        self.inner.set_setting(key, value).await
    }

    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        self.inner.list_settings().await
    }
}
