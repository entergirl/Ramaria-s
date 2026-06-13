//! rust/crates/ramaria-storage/src/lib.rs - Ramaria SQLite 存储层（v1.0 完整版）
//!
//! 设计特点:
//! - 封装 SqlitePool，实现 `StorageBackend` trait 的全部方法（覆盖 23 张表）
//! - Repository 模式：每个子模块负责一类实体的 SQL 操作与行映射
//! - 所有可恢复错误统一转换为 RamariaError::Storage
//! - 手动行映射避免 sqlx derive 侵入 core 层，保持零 I/O 约束
//! - 公共 API 与 `StorageBackend` trait 一致，供 app/memory 层依赖注入使用
//! - ID 类型对齐: TEXT 主键表用 Uuid，INTEGER AUTOINCREMENT 表用 i64

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, Persona,
    PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, ProfileField, Session,
    TraitEvidence, TraitStatus,
};
use sqlx::SqlitePool;
use uuid::Uuid;

pub mod database;
pub mod repo;

/// SQLite 存储后端。
pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl StorageBackend for SqliteStorage {
    // =========================================================
    // Session 管理（会话生命周期）
    // =========================================================
    async fn create_session(&self) -> RamariaResult<Session> {
        repo::sessions::create(&self.pool).await
    }
    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> {
        repo::sessions::close(&self.pool, session_id).await
    }
    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>> {
        repo::sessions::get(&self.pool, session_id).await
    }
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        repo::sessions::list_active(&self.pool).await
    }
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        repo::sessions::list_all(&self.pool).await
    }
    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        repo::sessions::delete(&self.pool, session_id).await
    }

    // =========================================================
    // Message（L0 原始消息）
    // =========================================================
    async fn save_message(&self, message: &Message) -> RamariaResult<()> {
        repo::messages::save(&self.pool, message).await
    }
    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>> {
        repo::messages::list_by_session(&self.pool, session_id).await
    }
    async fn list_messages_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<Message>> {
        repo::messages::list_by_persona(&self.pool, persona_uid).await
    }
    async fn find_message_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> RamariaResult<Option<Message>> {
        repo::messages::find_by_fingerprint(&self.pool, fingerprint).await
    }
    async fn get_last_message_time(&self, session_id: Uuid) -> RamariaResult<Option<i64>> {
        repo::messages::get_last_message_time(&self.pool, session_id).await
    }

    // =========================================================
    // Memory L1（单次会话摘要）
    // =========================================================
    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()> {
        repo::memory_l1::save(&self.pool, memory).await
    }
    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        repo::memory_l1::list_by_session(&self.pool, session_id).await
    }
    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>> {
        repo::memory_l1::get(&self.pool, id).await
    }
    async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()> {
        repo::memory_l1::mark_absorbed(&self.pool, l1_ids).await
    }
    async fn list_unabsorbed_l1(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        repo::memory_l1::list_unabsorbed(&self.pool, persona_uid).await
    }

    // =========================================================
    // Persona（人格注册）
    // =========================================================
    async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64> {
        repo::personas::create(&self.pool, persona).await
    }
    async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>> {
        repo::personas::get_by_uid(&self.pool, uid).await
    }
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        repo::personas::list_all(&self.pool).await
    }
    async fn update_persona(
        &self,
        uid: &str,
        name: &str,
        avatar: Option<&str>,
        config: Option<&str>,
    ) -> RamariaResult<()> {
        repo::personas::update(&self.pool, uid, name, avatar, config).await
    }

    // =========================================================
    // Memory Events（L2 事件层）
    // =========================================================
    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> {
        repo::events::save_event(&self.pool, event).await
    }
    async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        offset: i64,
        limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        repo::events::list_events_by_persona(&self.pool, persona_uid, offset, limit).await
    }
    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        repo::events::list_unabsorbed_events(&self.pool, persona_uid).await
    }

    // =========================================================
    // Event Relations（事件关系）+ Event Sources（事件溯源）
    // =========================================================
    async fn save_event_relation(&self, rel: &EventRelation) -> RamariaResult<i64> {
        repo::events::save_relation(&self.pool, rel).await
    }

    async fn save_event_source(
        &self,
        event_id: i64,
        l1_id: Uuid,
        weight: f64,
    ) -> RamariaResult<()> {
        repo::events::save_source(&self.pool, event_id, l1_id, weight).await
    }

    // =========================================================
    // Persona Facts（人物事实）
    // =========================================================
    async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64> {
        repo::facts::save(&self.pool, fact).await
    }
    async fn list_facts_by_persona(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        repo::facts::list_by_persona(&self.pool, persona_uid, field).await
    }

    // =========================================================
    // Personality Traits（L3 性格层）+ Trait Evidence（证据链）
    // =========================================================
    async fn save_trait(&self, t: &PersonalityTrait) -> RamariaResult<i64> {
        repo::traits::save_trait(&self.pool, t).await
    }
    async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>> {
        repo::traits::list_traits_by_persona(&self.pool, persona_uid).await
    }
    async fn update_trait_confidence(
        &self,
        id: i64,
        confidence: f64,
        evidence: f64,
        consistency: f64,
    ) -> RamariaResult<()> {
        repo::traits::update_confidence(&self.pool, id, confidence, evidence, consistency).await
    }
    async fn update_trait_status(&self, id: i64, status: TraitStatus) -> RamariaResult<()> {
        repo::traits::update_status(&self.pool, id, status).await
    }

    async fn save_evidence(&self, e: &TraitEvidence) -> RamariaResult<i64> {
        repo::traits::save_evidence(&self.pool, e).await
    }
    async fn list_evidence_by_trait(&self, trait_id: i64) -> RamariaResult<Vec<TraitEvidence>> {
        repo::traits::list_evidence_by_trait(&self.pool, trait_id).await
    }

    // =========================================================
    // Persona Examples（Few-shot 示例）
    // =========================================================
    async fn save_example(&self, e: &PersonaExample) -> RamariaResult<i64> {
        repo::examples::save(&self.pool, e).await
    }
    async fn list_selected_examples(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaExample>> {
        repo::examples::list_selected(&self.pool, persona_uid).await
    }

    // =========================================================
    // Cluster Snapshots（聚类快照）
    // =========================================================
    async fn save_cluster_snapshot(&self, s: &ClusterSnapshot) -> RamariaResult<i64> {
        repo::cluster::save(&self.pool, s).await
    }
    async fn get_current_snapshots(
        &self,
        persona_uid: &str,
        category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        repo::cluster::get_current(&self.pool, persona_uid, category).await
    }

    // =========================================================
    // Keyword Pool（关键词词典）
    // =========================================================
    async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()> {
        repo::keyword::upsert(&self.pool, keyword).await
    }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        repo::keyword::list_all(&self.pool).await
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

    // =========================================================
    // Conflict Queue（冲突队列）
    // =========================================================
    async fn create_conflict(
        &self,
        field: &str,
        conflict_type: &str,
        old_content: Option<&str>,
        new_content: Option<&str>,
        desc: Option<&str>,
    ) -> RamariaResult<i64> {
        repo::conflict_queue::create(
            &self.pool,
            field,
            conflict_type,
            old_content,
            new_content,
            desc,
        )
        .await
    }
    async fn list_pending_conflicts(&self) -> RamariaResult<Vec<(i64, String, String, String)>> {
        repo::conflict_queue::list_pending(&self.pool).await
    }
    async fn resolve_conflict(&self, id: i64) -> RamariaResult<()> {
        repo::conflict_queue::resolve(&self.pool, id).await
    }

    // =========================================================
    // Pending Push（待推送消息）
    // =========================================================
    async fn create_push(&self, content: &str) -> RamariaResult<i64> {
        repo::pending_push::create(&self.pool, content).await
    }
    async fn list_pending_pushes(&self) -> RamariaResult<Vec<(i64, String)>> {
        repo::pending_push::list_pending(&self.pool).await
    }
    async fn mark_push_sent(&self, id: i64) -> RamariaResult<()> {
        repo::pending_push::mark_sent(&self.pool, id).await
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
    // BM25 Index（全文索引）
    // =========================================================
    async fn save_bm25(&self, doc_id: i64, layer: &str, tokens_json: &str) -> RamariaResult<()> {
        repo::bm25_index::save(&self.pool, doc_id, layer, tokens_json).await
    }
    async fn list_bm25_by_doc(&self, doc_id: i64) -> RamariaResult<Vec<(String, String)>> {
        repo::bm25_index::list_by_doc(&self.pool, doc_id).await
    }
    async fn delete_bm25_by_doc(&self, doc_id: i64) -> RamariaResult<()> {
        repo::bm25_index::delete_by_doc(&self.pool, doc_id).await
    }

    // =========================================================
    // Graph（知识图谱）
    // =========================================================
    async fn insert_graph_node(
        &self,
        entity_name: &str,
        entity_type: &str,
        source_l1_id: Option<Uuid>,
    ) -> RamariaResult<i64> {
        repo::graph::insert_node(&self.pool, entity_name, entity_type, source_l1_id).await
    }
    async fn get_graph_node(
        &self,
        entity_name: &str,
    ) -> RamariaResult<Option<(i64, String, String)>> {
        repo::graph::get_node(&self.pool, entity_name).await
    }
    async fn insert_graph_edge(
        &self,
        source_id: i64,
        target_id: i64,
        relation_type: &str,
        detail: Option<&str>,
        source_l1_id: Option<Uuid>,
    ) -> RamariaResult<i64> {
        repo::graph::insert_edge(
            &self.pool,
            source_id,
            target_id,
            relation_type,
            detail,
            source_l1_id,
        )
        .await
    }
    async fn list_graph_edges(
        &self,
        source_id: i64,
    ) -> RamariaResult<Vec<(i64, i64, i64, String)>> {
        repo::graph::list_edges(&self.pool, source_id).await
    }
}

// =========================================================
// 集成测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{
        EventRelationKind, EvidenceDirection, FactSource, MessageRole, MessageSource, PersonaKind,
        TraitLayer, TraitSource, TraitStatus, now_ms,
    };

    async fn setup() -> SqliteStorage {
        let pool = database::init_test_pool()
            .await
            .expect("测试数据库初始化失败");
        SqliteStorage::new(pool)
    }

    #[tokio::test]
    async fn session_crud() {
        let storage = setup().await;
        let session = storage.create_session().await.unwrap();
        assert!(session.ended_at.is_none());

        let got = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(got.id, session.id);

        storage.close_session(session.id).await.unwrap();
        let closed = storage.get_session(session.id).await.unwrap().unwrap();
        assert!(closed.ended_at.is_some());
    }

    #[tokio::test]
    async fn message_crud() {
        let storage = setup().await;
        let session = storage.create_session().await.unwrap();
        let msg = Message::new(
            session.id,
            MessageRole::User,
            "测试消息".into(),
            MessageSource::Local,
        );
        storage.save_message(&msg).await.unwrap();

        let msgs = storage.list_messages(session.id).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "测试消息");
    }

    #[tokio::test]
    async fn message_with_persona_uid() {
        let storage = setup().await;
        // 先创建 persona，否则 FK 约束会失败
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        let session = storage.create_session().await.unwrap();
        let mut msg = Message::new(
            session.id,
            MessageRole::User,
            "你好".into(),
            MessageSource::Local,
        );
        msg.persona_uid = Some("user-0001".into());
        storage.save_message(&msg).await.unwrap();

        let msgs = storage.list_messages(session.id).await.unwrap();
        assert_eq!(msgs[0].persona_uid.as_deref(), Some("user-0001"));
    }

    #[tokio::test]
    async fn memory_l1_crud() {
        let storage = setup().await;
        let session = storage.create_session().await.unwrap();
        let l1 = MemoryL1::new(session.id, "摘要".into(), Some("上午".into()));
        storage.save_memory_l1(&l1).await.unwrap();

        let list = storage.list_memory_l1(session.id).await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn persona_crud() {
        let storage = setup().await;
        let p = Persona::new(
            "user-0001".into(),
            "测试用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        let id = storage.create_persona(&p).await.unwrap();
        assert!(id > 0, "INSERT 后应返回有效的自增 id");

        let got = storage
            .get_persona_by_uid("user-0001")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.name, "测试用户");
        assert_eq!(got.id, id);

        let all = storage.list_personas().await.unwrap();
        assert!(!all.is_empty());
    }

    #[tokio::test]
    async fn memory_event_crud() {
        let storage = setup().await;
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        let now = now_ms();
        let ev = MemoryEvent::new(
            "user-0001".into(),
            "事件".into(),
            "描述".into(),
            now - 1000,
            now,
        );
        let ev_id = storage.save_event(&ev).await.unwrap();
        assert!(ev_id > 0);

        let events = storage
            .list_events_by_persona("user-0001", 0, 10)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "事件");
        assert_eq!(events[0].id, ev_id);
    }

    #[tokio::test]
    async fn event_relation_crud() {
        let storage = setup().await;
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();
        let now = now_ms();
        let e1 = MemoryEvent::new("user-0001".into(), "A".into(), "desc".into(), now, now);
        let e2 = MemoryEvent::new("user-0001".into(), "B".into(), "desc".into(), now, now);
        let id1 = storage.save_event(&e1).await.unwrap();
        let id2 = storage.save_event(&e2).await.unwrap();

        let rel = EventRelation::new(id1, id2, EventRelationKind::CausedBy);
        let rel_id = storage.save_event_relation(&rel).await.unwrap();
        assert!(rel_id > 0);
    }

    #[tokio::test]
    async fn event_source_crud() {
        let storage = setup().await;
        let session = storage.create_session().await.unwrap();
        let l1 = MemoryL1::new(session.id, "摘要".into(), None);
        storage.save_memory_l1(&l1).await.unwrap();
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();
        let now = now_ms();
        let ev = MemoryEvent::new("user-0001".into(), "E".into(), "desc".into(), now, now);
        let ev_id = storage.save_event(&ev).await.unwrap();

        storage.save_event_source(ev_id, l1.id, 1.0).await.unwrap();
    }

    #[tokio::test]
    async fn persona_fact_crud() {
        let storage = setup().await;
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        let fact = PersonaFact::new(
            "user-0001".into(),
            ramaria_core::types::ProfileField::BasicInfo,
            "姓名：小明".into(),
            FactSource::L1,
        );
        let fact_id = storage.save_fact(&fact).await.unwrap();
        assert!(fact_id > 0);

        let facts = storage
            .list_facts_by_persona("user-0001", ramaria_core::types::ProfileField::BasicInfo)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].id, fact_id);
    }

    #[tokio::test]
    async fn personality_trait_crud() {
        let storage = setup().await;
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        let pt = PersonalityTrait::new(
            "user-0001".into(),
            TraitLayer::Base,
            "温和".into(),
            "待人温和".into(),
            TraitSource::Inferred,
            0,
        );
        let pt_id = storage.save_trait(&pt).await.unwrap();
        assert!(pt_id > 0);

        let traits = storage.list_traits_by_persona("user-0001").await.unwrap();
        assert_eq!(traits.len(), 1);
        assert_eq!(traits[0].trait_label, "温和");
        assert_eq!(traits[0].id, pt_id);

        // 更新置信度
        storage
            .update_trait_confidence(pt_id, 0.8, 5.0, 0.9)
            .await
            .unwrap();
        // 更新状态
        storage
            .update_trait_status(pt_id, TraitStatus::Deprecated)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn trait_evidence_crud() {
        let storage = setup().await;
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();
        let pt = PersonalityTrait::new(
            "user-0001".into(),
            TraitLayer::Base,
            "温和".into(),
            "待人温和".into(),
            TraitSource::Inferred,
            0,
        );
        let pt_id = storage.save_trait(&pt).await.unwrap();
        let now = now_ms();
        let ev = MemoryEvent::new("user-0001".into(), "事件".into(), "描述".into(), now, now);
        let ev_id = storage.save_event(&ev).await.unwrap();

        let evidence = TraitEvidence::new(pt_id, ev_id, EvidenceDirection::Support, 0.8);
        let evd_id = storage.save_evidence(&evidence).await.unwrap();
        assert!(evd_id > 0);

        let list = storage.list_evidence_by_trait(pt_id).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, evd_id);
        assert_eq!(list[0].trait_id, pt_id);
        assert_eq!(list[0].event_id, ev_id);
    }

    #[tokio::test]
    async fn keyword_upsert() {
        let storage = setup().await;
        storage.upsert_keyword("工作").await.unwrap();
        storage.upsert_keyword("工作").await.unwrap();
        let keywords = storage.list_keywords().await.unwrap();
        assert!(keywords.contains(&"工作".to_string()));
    }

    #[tokio::test]
    async fn schema_version() {
        let storage = setup().await;
        let v = storage.get_schema_version().await.unwrap();
        assert!(v >= 1);
    }

    #[tokio::test]
    async fn privacy_consent_crud() {
        let storage = setup().await;
        let consent = PrivacyConsent::new(
            ramaria_core::types::LlmProvider::DeepSeek,
            "https://api.deepseek.com/v1".into(),
            true,
        );
        storage.save_privacy_consent(&consent).await.unwrap();
        let got = storage
            .get_privacy_consent("deepseek", "https://api.deepseek.com/v1")
            .await
            .unwrap();
        assert!(got.is_some());
        assert_eq!(
            got.unwrap().provider,
            ramaria_core::types::LlmProvider::DeepSeek
        );
    }

    #[tokio::test]
    async fn settings_crud() {
        let storage = setup().await;
        storage.set_setting("profile_mode", "full").await.unwrap();
        let val = storage.get_setting("profile_mode").await.unwrap();
        assert_eq!(val.as_deref(), Some("full"));
        let all = storage.list_settings().await.unwrap();
        assert!(!all.is_empty());
    }

    #[tokio::test]
    async fn graph_node_and_edge() {
        let storage = setup().await;
        let nid = storage
            .insert_graph_node("Python", "module", None)
            .await
            .unwrap();
        assert!(nid > 0);

        let nid2 = storage
            .insert_graph_node("Rust", "module", None)
            .await
            .unwrap();

        let eid = storage
            .insert_graph_edge(nid, nid2, "RelatedTo", None, None)
            .await
            .unwrap();
        assert!(eid > 0);

        let edges = storage.list_graph_edges(nid).await.unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].2, nid2);
    }

    // =========================================================
    // T-FIX-006: list_unabsorbed_events & update_persona 补充测试
    // =========================================================

    /// 辅助：创建含 persona 和 L1 的完整测试上下文。
    async fn setup_with_persona() -> (SqliteStorage, String, i64, uuid::Uuid) {
        let storage = setup().await;
        let p = Persona::new(
            "user-test".into(),
            "测试角色".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        let persona_id = storage.create_persona(&p).await.unwrap();

        let session = storage.create_session().await.unwrap();
        let l1 = MemoryL1::new(session.id, "测试摘要".into(), Some("上午".into()));
        storage.save_memory_l1(&l1).await.unwrap();

        (storage, "user-test".to_string(), persona_id, l1.id)
    }

    /// 辅助：创建 MemoryEvent 并关联到 persona。
    async fn create_test_event(storage: &SqliteStorage, persona_uid: &str, title: &str) -> i64 {
        let now = now_ms();
        let ev = MemoryEvent::new(
            persona_uid.into(),
            title.into(),
            "测试描述".into(),
            now - 1000,
            now,
        );
        storage.save_event(&ev).await.unwrap()
    }

    #[tokio::test]
    async fn list_unabsorbed_events_empty() {
        // 新建 persona 尚未有任何事件
        let (storage, persona_uid, _, _) = setup_with_persona().await;

        let events = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
        assert!(events.is_empty(), "新 persona 应该没有未吸收事件");
    }

    #[tokio::test]
    async fn list_unabsorbed_events_some() {
        let (storage, persona_uid, _, _) = setup_with_persona().await;

        // 创建 3 个事件
        let id1 = create_test_event(&storage, &persona_uid, "事件A").await;
        let id2 = create_test_event(&storage, &persona_uid, "事件B").await;
        let id3 = create_test_event(&storage, &persona_uid, "事件C").await;

        let events = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
        assert_eq!(events.len(), 3, "应返回全部 3 个未吸收事件");
        let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
        assert!(ids.contains(&id3));
    }

    #[tokio::test]
    async fn list_unabsorbed_events_only_matching_persona() {
        let (storage, persona_uid, _, _) = setup_with_persona().await;

        // 创建第二个 persona
        let p2 = Persona::new(
            "char-test".into(),
            "角色".into(),
            PersonaKind::Char,
            1,
            "local".into(),
        );
        storage.create_persona(&p2).await.unwrap();

        create_test_event(&storage, &persona_uid, "用户事件").await;
        create_test_event(&storage, "char-test", "角色事件").await;

        let events = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
        assert_eq!(events.len(), 1, "只应返回 user-test 的事件");
        assert_eq!(events[0].title, "用户事件");
    }

    #[tokio::test]
    async fn update_persona_name() {
        let (storage, persona_uid, _, _) = setup_with_persona().await;

        // 更新名称
        storage
            .update_persona(&persona_uid, "新名称", None, None)
            .await
            .unwrap();

        let updated = storage
            .get_persona_by_uid(&persona_uid)
            .await
            .unwrap()
            .expect("persona 应存在");
        assert_eq!(updated.name, "新名称");
    }

    #[tokio::test]
    async fn update_persona_avatar_and_config() {
        let (storage, persona_uid, _, _) = setup_with_persona().await;

        // 更新头像和 config JSON
        storage
            .update_persona(
                &persona_uid,
                "测试角色", // name 不变
                Some("avatar_url_here"),
                Some(r#"{"description":"更新后的描述"}"#),
            )
            .await
            .unwrap();

        let updated = storage
            .get_persona_by_uid(&persona_uid)
            .await
            .unwrap()
            .expect("persona 应存在");
        assert_eq!(updated.avatar.as_deref(), Some("avatar_url_here"));
        assert!(updated.config.is_some());
        assert!(updated.config.unwrap().contains("更新后的描述"));
    }

    #[tokio::test]
    async fn update_persona_partial_fields() {
        // 只更新部分字段，验证未指定的字段不被覆盖
        let (storage, persona_uid, _, _) = setup_with_persona().await;

        // 先设置头像
        storage
            .update_persona(&persona_uid, "测试角色", Some("old_avatar"), None)
            .await
            .unwrap();

        // 再只更新 config，头像应保持不变
        storage
            .update_persona(
                &persona_uid,
                "测试角色",
                None, // avatar 传 None 不更新
                Some(r#"{"key":"value"}"#),
            )
            .await
            .unwrap();

        let updated = storage
            .get_persona_by_uid(&persona_uid)
            .await
            .unwrap()
            .expect("persona 应存在");
        assert_eq!(
            updated.avatar.as_deref(),
            Some("old_avatar"),
            "未传入 avatar 时应保持旧值"
        );
        assert!(updated.config.is_some());
    }

    // =========================================================
    // T-FIX-014: mark_absorbed 批次边界测试
    // =========================================================
    // 验证 BATCH_SIZE=100 的分批逻辑在所有边界条件下正确工作。
    // 由于 mark_absorbed 内部以 100 条为单位分批，需要确保:
    // - 恰好 100 条 → 单批次
    // - 101 条 → 两个批次（100 + 1）
    // - 200 条 → 两个批次（100 + 100）

    /// 创建 N 条 L1 记忆并返回它们的 ID 列表。
    async fn create_n_l1(
        storage: &SqliteStorage,
        session_id: uuid::Uuid,
        persona_uid: &str,
        n: usize,
    ) -> Vec<uuid::Uuid> {
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            let l1 = MemoryL1::new(
                session_id,
                format!("测试摘要 #{i}"),
                Some(format!("时段-{i}")),
            );
            // 手动设置 persona_uid（MemoryL1::new 不支持该字段）
            let mut l1_with_persona = l1;
            // 通过直接构造覆盖 persona_uid 字段
            // MemoryL1 结构体的字段为 pub，可以直接赋值
            l1_with_persona.persona_uid = Some(persona_uid.to_string());
            storage.save_memory_l1(&l1_with_persona).await.unwrap();
            ids.push(l1_with_persona.id);
        }
        ids
    }

    /// 辅助：创建 persona + session 用于 mark_absorbed 测试。
    async fn setup_for_absorb() -> (SqliteStorage, String, uuid::Uuid) {
        let storage = setup().await;
        let persona_uid = "absorb-test".to_string();
        let p = Persona::new(
            persona_uid.clone(),
            "吸收测试".into(),
            PersonaKind::User,
            100,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();
        let session = storage.create_session().await.unwrap();
        (storage, persona_uid, session.id)
    }

    #[tokio::test]
    async fn mark_absorbed_empty_slice_is_noop() {
        // 空切片应直接返回 Ok，不产生错误
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let _l1_ids = create_n_l1(&storage, session_id, &persona_uid, 3).await;

        // 标记空切片，应成功
        storage.mark_l1_absorbed(&[]).await.unwrap();

        // 原有记录应仍未吸收
        let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
        assert_eq!(remaining.len(), 3);
    }

    #[tokio::test]
    async fn mark_absorbed_single_item() {
        // 单条记录吸收
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 1).await;
        let target = &[l1_ids[0]];

        storage.mark_l1_absorbed(target).await.unwrap();

        let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
        assert!(
            remaining.is_empty(),
            "吸收后应无未吸收记录，实际: {remaining:?}"
        );
    }

    #[tokio::test]
    async fn mark_absorbed_exactly_100_items() {
        // 恰好 100 条（单批次边界值，BATCH_SIZE = 100）
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 100).await;

        storage.mark_l1_absorbed(&l1_ids).await.unwrap();

        let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
        assert!(
            remaining.is_empty(),
            "100 条应全部吸收，实际剩余: {}",
            remaining.len()
        );
    }

    #[tokio::test]
    async fn mark_absorbed_101_items_crosses_batch_boundary() {
        // 101 条，跨越批次边界（100 + 1），验证事务中多批次原子性
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 101).await;

        storage.mark_l1_absorbed(&l1_ids).await.unwrap();

        let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
        assert!(
            remaining.is_empty(),
            "101 条跨批次应全部吸收，实际剩余: {}",
            remaining.len()
        );
    }

    #[tokio::test]
    async fn mark_absorbed_200_items_two_full_batches() {
        // 200 条，恰好两个完整批次（100 + 100）
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 200).await;

        storage.mark_l1_absorbed(&l1_ids).await.unwrap();

        let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
        assert!(
            remaining.is_empty(),
            "200 条应全部吸收，实际剩余: {}",
            remaining.len()
        );
    }

    #[tokio::test]
    async fn mark_absorbed_only_absorbs_specified_ids() {
        // 仅指定 ID 被吸收，未指定的不受影响
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 5).await;

        // 只吸收前 3 条
        storage.mark_l1_absorbed(&l1_ids[..3]).await.unwrap();

        let remaining = storage.list_unabsorbed_l1(&persona_uid).await.unwrap();
        assert_eq!(remaining.len(), 2, "应剩余 2 条未吸收");
    }

    // =========================================================
    // T-FIX-014: background_jobs CRUD 集成测试
    // =========================================================

    #[tokio::test]
    async fn background_job_create_and_list_pending() {
        let storage = setup().await;

        // 创建两个不同类型、不同 payload 的 job
        let id1 = storage
            .create_background_job("l2_extraction", Some(r#"{"session_id":"abc"}"#))
            .await
            .unwrap();
        let id2 = storage
            .create_background_job("personality_inference", Some(r#"{"persona_uid":"u1"}"#))
            .await
            .unwrap();

        assert!(id1 > 0, "job ID 应为正整数");
        assert!(id2 > 0, "job ID 应为正整数");
        assert_ne!(id1, id2, "不同 job 应有不同 ID");

        // list_pending 应包含两个 job
        let pending = storage.list_pending_jobs().await.unwrap();
        assert_eq!(pending.len(), 2);

        // 验证 job_type 和 payload 正确返回
        let job1 = pending.iter().find(|(id, _, _)| *id == id1).unwrap();
        assert_eq!(job1.1, "l2_extraction");
        assert_eq!(job1.2.as_deref(), Some(r#"{"session_id":"abc"}"#));

        let job2 = pending.iter().find(|(id, _, _)| *id == id2).unwrap();
        assert_eq!(job2.1, "personality_inference");
    }

    #[tokio::test]
    async fn background_job_update_status_removes_from_pending() {
        let storage = setup().await;

        let id = storage
            .create_background_job("reindex", None)
            .await
            .unwrap();

        // 更新状态为 running，job 应从 pending 列表中移除
        storage
            .update_job_status(id, "running", None)
            .await
            .unwrap();

        let pending = storage.list_pending_jobs().await.unwrap();
        assert!(
            !pending.iter().any(|(jid, _, _)| *jid == id),
            "running 状态的 job 不应出现在 pending 列表中"
        );
    }

    #[tokio::test]
    async fn background_job_with_error() {
        let storage = setup().await;

        let id = storage
            .create_background_job("data_migration", Some(r#"{"version":2}"#))
            .await
            .unwrap();

        // 更新为 failed 并记录错误信息
        storage
            .update_job_status(id, "failed", Some("磁盘空间不足"))
            .await
            .unwrap();

        // failed 的 job 也不应在 pending 中
        let pending = storage.list_pending_jobs().await.unwrap();
        assert!(
            !pending.iter().any(|(jid, _, _)| *jid == id),
            "failed 状态的 job 不应出现在 pending 列表中"
        );
    }

    #[tokio::test]
    async fn background_job_empty_payload() {
        let storage = setup().await;

        let id = storage
            .create_background_job("health_check", None)
            .await
            .unwrap();

        let pending = storage.list_pending_jobs().await.unwrap();
        let job = pending.iter().find(|(jid, _, _)| *jid == id).unwrap();
        assert_eq!(job.1, "health_check");
        assert!(job.2.is_none(), "无 payload 时应为 None");
    }
}
