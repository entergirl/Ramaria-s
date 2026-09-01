//! crates/ramaria-storage/src/lib.rs - Ramaria SQLite 存储层
//!
//! 设计特点:
//! - 封装 SqlitePool，实现 `StorageBackend` trait 的全部方法（覆盖 24 张表）
//! - Repository 模式：每个子模块负责一类实体的 SQL 操作与行映射
//! - 所有可恢复错误统一转换为 RamariaError::Storage
//! - 手动行映射避免 sqlx derive 侵入 core 层，保持零 I/O 约束
//! - 公共 API 与 `StorageBackend` trait 一致，供 app/memory 层依赖注入使用
//! - ID 类型对齐: TEXT 主键表用 Uuid，INTEGER AUTOINCREMENT 表用 i64

use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::config::CacheEviction;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    BackendConfig, ClusterSnapshot, EventRelation, EventSource, MemoryEvent, MemoryL1, Message,
    Persona, PersonaExample, PersonaFact, PersonaStyleStats, PersonalityTrait, PrivacyConsent,
    ProfileField, Session, TraitEvidence, TraitStatus, UttBlock, now_ms,
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
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        repo::sessions::create(&self.pool, persona_uid).await
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
    async fn bind_session_persona_uid(
        &self,
        session_id: Uuid,
        persona_uid: &str,
    ) -> RamariaResult<()> {
        repo::sessions::bind_persona_uid(&self.pool, session_id, persona_uid).await
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
    /// 覆写为高效 SQL 分页（`ORDER BY created_at DESC LIMIT ? OFFSET ?`）。
    async fn list_messages_paginated(
        &self,
        session_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> RamariaResult<Vec<Message>> {
        repo::messages::list_by_session_paginated(&self.pool, session_id, limit, offset).await
    }
    async fn list_messages_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<Message>> {
        repo::messages::list_by_persona(&self.pool, persona_uid).await
    }
    async fn get_last_message_time(&self, session_id: Uuid) -> RamariaResult<Option<i64>> {
        repo::messages::get_last_message_time(&self.pool, session_id).await
    }
    async fn count_messages(&self, session_id: Uuid) -> RamariaResult<u32> {
        repo::messages::count_by_session(&self.pool, session_id).await
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
    async fn touch_l1(&self, l1_ids: &[Uuid], now_ms: i64) -> RamariaResult<()> {
        repo::memory_l1::touch(&self.pool, l1_ids, now_ms).await
    }
    async fn delete_memory_l1_by_session(&self, session_id: Uuid) -> RamariaResult<usize> {
        repo::memory_l1::delete_by_session(&self.pool, session_id).await
    }
    async fn list_unabsorbed_l1(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> {
        repo::memory_l1::list_unabsorbed(&self.pool, persona_uid).await
    }

    async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> {
        repo::memory_l1::list_unabsorbed_unbound(&self.pool).await
    }

    async fn assign_l1_persona_uid(
        &self,
        l1_ids: &[Uuid],
        persona_uid: &str,
    ) -> RamariaResult<usize> {
        repo::memory_l1::assign_persona_uid(&self.pool, l1_ids, persona_uid).await
    }
    async fn list_recent_l1_by_persona(
        &self,
        persona_uid: &str,
        limit: u32,
    ) -> RamariaResult<Vec<MemoryL1>> {
        repo::memory_l1::list_recent_by_persona(&self.pool, persona_uid, limit).await
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
        description: Option<&str>,
    ) -> RamariaResult<()> {
        repo::personas::update(&self.pool, uid, name, avatar, config, description).await
    }

    // =========================================================
    // Memory Events（L2 事件层）
    // =========================================================
    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> {
        repo::events::save_event(&self.pool, event).await
    }
    async fn get_event(&self, id: i64) -> RamariaResult<Option<MemoryEvent>> {
        repo::events::get(&self.pool, id).await
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

    async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()> {
        repo::events::mark_absorbed(&self.pool, event_ids).await
    }

    // =========================================================
    // Event Relations（事件关系）+ Event Sources（事件溯源）
    // =========================================================
    async fn save_event_relation(&self, rel: &EventRelation) -> RamariaResult<i64> {
        repo::events::save_relation(&self.pool, rel).await
    }

    async fn list_event_relations_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<EventRelation>> {
        repo::events::list_relations_by_persona(&self.pool, persona_uid).await
    }

    async fn save_event_source(
        &self,
        event_id: i64,
        l1_id: Uuid,
        weight: f64,
    ) -> RamariaResult<()> {
        repo::events::save_source(&self.pool, event_id, l1_id, weight).await
    }

    async fn list_event_sources_by_event(&self, event_id: i64) -> RamariaResult<Vec<EventSource>> {
        repo::events::list_sources_by_event(&self.pool, event_id).await
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
    /// 使用 GROUP BY 单查询替代 N+1 循环。
    async fn count_all_facts_for_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<(ProfileField, usize)>> {
        repo::facts::count_by_persona_grouped(&self.pool, persona_uid).await
    }
    async fn list_active_facts_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        repo::facts::list_active_by_persona(&self.pool, persona_uid).await
    }
    async fn list_active_facts_by_field(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        repo::facts::list_active_by_field(&self.pool, persona_uid, field).await
    }
    async fn list_all_facts_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        repo::facts::list_all_by_persona(&self.pool, persona_uid).await
    }
    async fn get_fact_by_id(&self, id: i64) -> RamariaResult<Option<PersonaFact>> {
        repo::facts::get_by_id(&self.pool, id).await
    }
    async fn save_fact_with_version(
        &self,
        old: &PersonaFact,
        f: &PersonaFact,
    ) -> RamariaResult<i64> {
        repo::facts::save_with_version(&self.pool, old, f).await
    }
    async fn promote_fact_to_active(&self, id: i64) -> RamariaResult<()> {
        repo::facts::promote_to_active(&self.pool, id).await
    }
    async fn list_fact_versions(&self, seed_id: i64) -> RamariaResult<Vec<PersonaFact>> {
        repo::facts::list_versions(&self.pool, seed_id).await
    }
    async fn supersede_fact(&self, id: i64, at: i64) -> RamariaResult<()> {
        repo::facts::supersede(&self.pool, id, at).await
    }

    // =========================================================
    // Style Stats（persona_style_stats，表达层 A3）
    // =========================================================
    async fn upsert_style_stats(&self, stats: &PersonaStyleStats) -> RamariaResult<()> {
        repo::style_stats::upsert(&self.pool, stats).await
    }
    async fn get_style_stats(&self, persona_uid: &str) -> RamariaResult<Option<PersonaStyleStats>> {
        repo::style_stats::get(&self.pool, persona_uid).await
    }
    async fn list_style_stats(&self) -> RamariaResult<Vec<PersonaStyleStats>> {
        repo::style_stats::list_all(&self.pool).await
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
    async fn list_all_examples(&self, persona_uid: &str) -> RamariaResult<Vec<PersonaExample>> {
        repo::examples::list_all(&self.pool, persona_uid).await
    }
    async fn find_example_by_pair(
        &self,
        persona_uid: &str,
        partner: &str,
        reply: &str,
    ) -> RamariaResult<Option<PersonaExample>> {
        repo::examples::find_by_pair(&self.pool, persona_uid, partner, reply).await
    }

    // =========================================================
    // Utt Blocks（原文话语块，v1.4）
    // =========================================================
    async fn insert_utt_block(&self, block: &UttBlock) -> RamariaResult<i64> {
        repo::utt_blocks::insert(&self.pool, block).await
    }
    async fn list_utt_blocks_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<UttBlock>> {
        repo::utt_blocks::list_by_persona(&self.pool, persona_uid).await
    }
    async fn get_latest_utt_block_by_session(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Option<UttBlock>> {
        repo::utt_blocks::get_latest_block_by_session(&self.pool, session_id).await
    }
    async fn delete_utt_block(&self, id: i64) -> RamariaResult<()> {
        repo::utt_blocks::delete_by_id(&self.pool, id).await
    }
    async fn delete_utt_blocks_by_session(&self, session_id: Uuid) -> RamariaResult<usize> {
        repo::utt_blocks::delete_by_session(&self.pool, session_id).await
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
    async fn get_all_snapshots_with_embeddings(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        repo::cluster::get_all_with_embeddings(&self.pool, persona_uid).await
    }

    // =========================================================
    // Keyword Pool（关键词词典）
    // =========================================================
    async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()> {
        // 将 &str 转换为 KeywordToken 后委托 repo
        if let Some(token) = ramaria_core::keyword::KeywordToken::new(keyword) {
            repo::keyword::upsert(&self.pool, &token).await
        } else {
            tracing::warn!(keyword, "关键词无效，跳过 upsert");
            Ok(())
        }
    }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        let tokens = repo::keyword::list_all(&self.pool).await?;
        Ok(tokens.into_iter().map(|t| t.into_inner()).collect())
    }

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
    async fn find_refs_by_keyword(
        &self,
        keyword_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
        let rows = repo::keyword::find_refs_by_keyword(&self.pool, keyword_id).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.id,
                    r.keyword_id,
                    r.doc_type,
                    r.doc_id,
                    r.persona_uid,
                    r.weight,
                    r.created_at,
                )
            })
            .collect())
    }
    async fn find_refs_by_doc(
        &self,
        doc_type: &str,
        doc_id: &str,
    ) -> RamariaResult<Vec<(i64, String, String, String, String, f64, i64)>> {
        let rows = repo::keyword::find_refs_by_doc(&self.pool, doc_type, doc_id).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.id,
                    r.keyword_id,
                    r.doc_type,
                    r.doc_id,
                    r.persona_uid,
                    r.weight,
                    r.created_at,
                )
            })
            .collect())
    }
    async fn delete_refs_by_doc(&self, doc_type: &str, doc_id: &str) -> RamariaResult<u64> {
        repo::keyword::delete_refs_by_doc(&self.pool, doc_type, doc_id).await
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

// =========================================================
// SqliteLlmCache —— LlmResponseCache trait 实现
// =========================================================

/// SQLite 实现的 LLM 响应精确缓存。
///
/// 职责:
/// - 实现 `ramaria_core::traits::LlmResponseCache`，供 `ramaria-llm` 的
///   `ProviderBase` 注入使用（`llm_response_cache` 表）。
/// - 写入后按 `[cache].max_entries` 容量上限自动淘汰（LRU/FIFO），
///   防止表无限增长；淘汰失败仅记 warn（不阻塞主流程）。
///
/// 安全约束:
/// - 只存响应，不存原文输入（key 为哈希，见 migration 注释）。
pub struct SqliteLlmCache {
    pool: SqlitePool,
    /// 容量上限（条目数）；0 表示不限制（仅测试/特殊场景）。
    max_entries: u64,
    /// 淘汰策略：true = FIFO（按 created_at），false = LRU（按 last_accessed_at）。
    fifo: bool,
}

impl SqliteLlmCache {
    /// 创建缓存实例。
    ///
    /// 参数:
    /// - `pool`: 数据库连接池（与主存储共用，保证同库事务一致）。
    /// - `max_entries`: 容量上限（`[cache].max_entries`）。
    /// - `eviction`: 淘汰策略（`[cache].eviction`，lru | fifo）。
    pub fn new(pool: SqlitePool, max_entries: u64, eviction: CacheEviction) -> Self {
        Self {
            pool,
            max_entries,
            fifo: eviction == CacheEviction::Fifo,
        }
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::LlmResponseCache for SqliteLlmCache {
    async fn get(&self, key: &str) -> RamariaResult<Option<String>> {
        let now = now_ms();
        match repo::llm_response_cache::get(&self.pool, key, now).await? {
            Some(entry) => Ok(Some(entry.response)),
            None => Ok(None),
        }
    }

    async fn put(
        &self,
        key: &str,
        response: &str,
        model_id: &str,
        template_version: &str,
    ) -> RamariaResult<()> {
        let entry = repo::llm_response_cache::LlmCacheEntry {
            key: key.to_string(),
            response: response.to_string(),
            model_id: model_id.to_string(),
            template_version: template_version.to_string(),
            created_at: 0,
            last_accessed_at: 0,
            hit_count: 0,
        };
        repo::llm_response_cache::put(&self.pool, &entry, now_ms()).await?;

        // 容量自淘汰（v1.5）：写入后若超出上限，按配置策略淘汰最旧条目。
        // 淘汰失败仅记 warn——缓存淘汰是优化而非正确性约束，不阻塞响应返回。
        if self.max_entries > 0
            && let Err(e) =
                repo::llm_response_cache::evict_oldest(&self.pool, self.max_entries, self.fifo)
                    .await
        {
            tracing::warn!(error = %e, max_entries = self.max_entries, "LLM 响应缓存容量淘汰失败（非致命）");
        }
        Ok(())
    }

    async fn count(&self) -> RamariaResult<u64> {
        repo::llm_response_cache::count(&self.pool).await
    }

    async fn evict_oldest(&self, keep: u64) -> RamariaResult<u64> {
        // 使用实例配置的淘汰策略（来自 [cache].eviction）。
        repo::llm_response_cache::evict_oldest(&self.pool, keep, self.fifo).await
    }
}

// =========================================================
// 集成测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::traits::LlmResponseCache;
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
        let session = storage.create_session(None).await.unwrap();
        assert!(session.ended_at.is_none());

        let got = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(got.id, session.id);

        storage.close_session(session.id).await.unwrap();
        let closed = storage.get_session(session.id).await.unwrap().unwrap();
        assert!(closed.ended_at.is_some());
    }

    // =========================================================
    // Session-Persona 绑定测试
    // =========================================================

    /// 创建 session 时可传入 persona_uid，get 时正确返回。
    #[tokio::test]
    async fn session_with_persona_uid() {
        let storage = setup().await;
        let session = storage.create_session(Some("user-0001")).await.unwrap();

        assert_eq!(session.persona_uid.as_deref(), Some("user-0001"));
        assert!(session.ended_at.is_none());

        // get 应返回相同 persona_uid
        let got = storage.get_session(session.id).await.unwrap().unwrap();
        assert_eq!(got.persona_uid.as_deref(), Some("user-0001"));
    }

    /// 存量兼容：不传 persona_uid 时，session.persona_uid 为 None。
    #[tokio::test]
    async fn session_without_persona_uid_compatible() {
        let storage = setup().await;
        let session = storage.create_session(None).await.unwrap();

        assert!(session.persona_uid.is_none());
        assert!(session.ended_at.is_none());

        // get 应返回 None
        let got = storage.get_session(session.id).await.unwrap().unwrap();
        assert!(got.persona_uid.is_none());
    }

    /// 活跃 session 列表正确返回 persona_uid。
    #[tokio::test]
    async fn active_sessions_preserve_persona_uid() {
        let storage = setup().await;

        let s1 = storage.create_session(Some("char-0001")).await.unwrap();
        let s2 = storage.create_session(Some("char-0002")).await.unwrap();
        let _s3 = storage.create_session(None).await.unwrap();

        let active = storage.list_active_sessions().await.unwrap();
        // 所有 session 都是活跃的
        assert!(active.len() >= 3);

        let got1 = active.iter().find(|s| s.id == s1.id).unwrap();
        assert_eq!(got1.persona_uid.as_deref(), Some("char-0001"));

        let got2 = active.iter().find(|s| s.id == s2.id).unwrap();
        assert_eq!(got2.persona_uid.as_deref(), Some("char-0002"));
    }

    /// 全部 session 列表正确返回 persona_uid。
    #[tokio::test]
    async fn all_sessions_preserve_persona_uid() {
        let storage = setup().await;

        let s = storage.create_session(Some("rama-0001")).await.unwrap();
        storage.close_session(s.id).await.unwrap();

        let all = storage.list_sessions().await.unwrap();
        let got = all.iter().find(|x| x.id == s.id).unwrap();
        assert_eq!(got.persona_uid.as_deref(), Some("rama-0001"));
    }

    #[tokio::test]
    async fn message_crud() {
        let storage = setup().await;
        let session = storage.create_session(None).await.unwrap();
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

        let session = storage.create_session(None).await.unwrap();
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
        let session = storage.create_session(None).await.unwrap();
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
        let session = storage.create_session(None).await.unwrap();
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

    // =========================================================
    // persona_facts 版本化 repo 测试
    // =========================================================

    /// 事务化版本链覆盖写：旧事实置 superseded + 新事实写入（version_of 指向旧 id）。
    #[tokio::test]
    async fn fact_version_chain_overwrite_atomic() {
        use ramaria_core::types::FactStatus;
        let storage = setup().await;
        let p = Persona::new(
            "user-0002".into(),
            "用户二".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        let mut old = PersonaFact::new(
            "user-0002".into(),
            ramaria_core::types::ProfileField::PersonalStatus,
            "当前情绪：平静".into(),
            FactSource::Event,
        );
        let old_id = storage.save_fact(&old).await.unwrap();
        old.id = old_id;

        // 新事实覆盖旧事实：旧置 superseded、新写入且 version_of 指向旧
        let fresh = PersonaFact::new(
            "user-0002".into(),
            ramaria_core::types::ProfileField::PersonalStatus,
            "当前情绪：焦虑".into(),
            FactSource::Event,
        );
        let fresh_id = storage.save_fact_with_version(&old, &fresh).await.unwrap();
        assert!(fresh_id > old_id);

        // 旧事实已 superseded
        let old_now = storage.get_fact_by_id(old_id).await.unwrap().unwrap();
        assert_eq!(old_now.status, FactStatus::Superseded);

        // 新事实 active 且 version_of 指向旧 id
        let fresh_now = storage.get_fact_by_id(fresh_id).await.unwrap().unwrap();
        assert_eq!(fresh_now.status, FactStatus::Active);
        assert_eq!(fresh_now.version_of, Some(old_id));

        // 版本链：从新事实回溯到旧事实（链头最早在前）
        let chain = storage.list_fact_versions(fresh_id).await.unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id, old_id);
        assert_eq!(chain[1].id, fresh_id);

        // active 查询只返回新事实（不含 superseded 旧事实）
        let active = storage
            .list_active_facts_by_field(
                "user-0002",
                ramaria_core::types::ProfileField::PersonalStatus,
            )
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, fresh_id);
    }

    /// list_active_facts_by_persona：跨字段仅返回 active 事实。
    #[tokio::test]
    async fn fact_list_active_by_persona_excludes_superseded() {
        use ramaria_core::types::FactStatus;
        let storage = setup().await;
        let p = Persona::new(
            "user-0003".into(),
            "用户三".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        let f = PersonaFact::new(
            "user-0003".into(),
            ramaria_core::types::ProfileField::Interests,
            "喜欢摄影".into(),
            FactSource::Manual,
        );
        let id = storage.save_fact(&f).await.unwrap();

        // 置为 superseded
        storage
            .supersede_fact(id, ramaria_core::types::now_ms())
            .await
            .unwrap();

        let active = storage
            .list_active_facts_by_persona("user-0003")
            .await
            .unwrap();
        assert!(active.is_empty(), "superseded 事实不应出现在 active 查询中");

        // 全部查询（CLI/版本链统计）仍包含 superseded
        let all = storage
            .list_all_facts_by_persona("user-0003")
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, FactStatus::Superseded);
    }

    /// candidate → active 提升（互证通过后）。
    #[tokio::test]
    async fn fact_promote_candidate_to_active() {
        use ramaria_core::types::FactStatus;
        let storage = setup().await;
        let p = Persona::new(
            "user-0004".into(),
            "用户四".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        // 主观隐含事实：status=candidate, tier=stable, confidence=0.5
        let mut f = PersonaFact::new(
            "user-0004".into(),
            ramaria_core::types::ProfileField::Interests,
            "偏好事实：不喜欢加班".into(),
            FactSource::Event,
        );
        f.status = FactStatus::Candidate;
        f.confidence = 0.5;
        let id = storage.save_fact(&f).await.unwrap();

        // 未提升前 active 查询不含 candidate
        let active = storage
            .list_active_facts_by_persona("user-0004")
            .await
            .unwrap();
        assert!(active.is_empty());

        // 互证通过 → 提升 active
        storage.promote_fact_to_active(id).await.unwrap();
        let got = storage.get_fact_by_id(id).await.unwrap().unwrap();
        assert_eq!(got.status, FactStatus::Active);

        let active = storage
            .list_active_facts_by_persona("user-0004")
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
    }

    /// get_fact_by_id 对不存在 id 返回 None（CLI show 缺省兜底）。
    #[tokio::test]
    async fn fact_get_by_id_missing_returns_none() {
        let storage = setup().await;
        let got = storage.get_fact_by_id(99999).await.unwrap();
        assert!(got.is_none());
    }

    /// 验证 GROUP BY 查询正确统计各字段数量。
    #[tokio::test]
    async fn count_all_facts_for_persona_grouped() {
        let storage = setup().await;
        let p = Persona::new(
            "user-0001".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        // 写入 3 条不同字段的 fact
        let f1 = PersonaFact::new(
            "user-0001".into(),
            ramaria_core::types::ProfileField::BasicInfo,
            "姓名：小明".into(),
            FactSource::L1,
        );
        let f2 = PersonaFact::new(
            "user-0001".into(),
            ramaria_core::types::ProfileField::Interests,
            "喜欢编程".into(),
            FactSource::Manual,
        );
        let f3 = PersonaFact::new(
            "user-0001".into(),
            ramaria_core::types::ProfileField::Interests,
            "喜欢阅读".into(),
            FactSource::Manual,
        );
        storage.save_fact(&f1).await.unwrap();
        storage.save_fact(&f2).await.unwrap();
        storage.save_fact(&f3).await.unwrap();

        let counts = storage
            .count_all_facts_for_persona("user-0001")
            .await
            .unwrap();
        assert_eq!(counts.len(), 7, "应返回全部 7 个 ProfileField");

        // BasicInfo: 1 条
        let basic_count = counts
            .iter()
            .find(|(f, _)| *f == ramaria_core::types::ProfileField::BasicInfo)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(basic_count, 1);

        // Interests: 2 条
        let interests_count = counts
            .iter()
            .find(|(f, _)| *f == ramaria_core::types::ProfileField::Interests)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(interests_count, 2);

        // PersonalStatus: 0 条（未写入）
        let ps_count = counts
            .iter()
            .find(|(f, _)| *f == ramaria_core::types::ProfileField::PersonalStatus)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(ps_count, 0, "未写入的字段应返回 0");

        // 总计数应为 3
        let total: usize = counts.iter().map(|(_, c)| c).sum();
        assert_eq!(total, 3);
    }

    /// 验证无记录 persona 返回全 0。
    #[tokio::test]
    async fn count_all_facts_for_persona_empty() {
        let storage = setup().await;
        let p = Persona::new(
            "user-empty".into(),
            "用户".into(),
            PersonaKind::User,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();

        let counts = storage
            .count_all_facts_for_persona("user-empty")
            .await
            .unwrap();
        assert_eq!(counts.len(), 7);
        let total: usize = counts.iter().map(|(_, c)| c).sum();
        assert_eq!(total, 0, "无 fact 时总计应为 0");
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
    // list_unabsorbed_events & update_persona 补充测试
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

        let session = storage.create_session(None).await.unwrap();
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
            .update_persona(&persona_uid, "新名称", None, None, None)
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
                None, // description 保持旧值
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
            .update_persona(&persona_uid, "测试角色", Some("old_avatar"), None, None)
            .await
            .unwrap();

        // 再只更新 config，头像应保持不变
        storage
            .update_persona(
                &persona_uid,
                "测试角色",
                None, // avatar 传 None 不更新
                Some(r#"{"key":"value"}"#),
                None, // description 保持旧值
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
    // mark_absorbed 批次边界测试
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
        let session = storage.create_session(None).await.unwrap();
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
    // touch_l1 访问时间刷新测试（v1.7 touch 接线，决策 D-V17-006）
    // =========================================================

    #[tokio::test]
    async fn touch_l1_updates_last_accessed_at() {
        // 检索命中后 touch_l1 应刷新 last_accessed_at（激活 recent_boost_*）
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 2).await;

        let before = storage.get_memory_l1(l1_ids[0]).await.unwrap().unwrap();
        assert!(before.last_accessed_at.is_none(), "初始应无访问时间");

        let now = now_ms();
        storage.touch_l1(&l1_ids, now).await.unwrap();

        for id in &l1_ids {
            let l1 = storage.get_memory_l1(*id).await.unwrap().unwrap();
            assert_eq!(
                l1.last_accessed_at,
                Some(now),
                "touch 后 last_accessed_at 应刷新为 now"
            );
        }
    }

    #[tokio::test]
    async fn touch_l1_empty_slice_is_noop() {
        // 空列表应直接成功（不产生错误、不影响既有记录）
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 3).await;

        storage.touch_l1(&[], now_ms()).await.unwrap();

        for id in &l1_ids {
            let l1 = storage.get_memory_l1(*id).await.unwrap().unwrap();
            assert!(l1.last_accessed_at.is_none(), "空 touch 不应改动访问时间");
        }
    }

    #[tokio::test]
    async fn touch_l1_only_updates_specified_ids() {
        // 仅指定 ID 被刷新，未指定的保持原值
        let (storage, persona_uid, session_id) = setup_for_absorb().await;
        let l1_ids = create_n_l1(&storage, session_id, &persona_uid, 3).await;

        let now = now_ms();
        storage.touch_l1(&l1_ids[..2], now).await.unwrap();

        let touched = storage.get_memory_l1(l1_ids[0]).await.unwrap().unwrap();
        assert_eq!(touched.last_accessed_at, Some(now), "前 2 条应被刷新");
        let untouched = storage.get_memory_l1(l1_ids[2]).await.unwrap().unwrap();
        assert!(untouched.last_accessed_at.is_none(), "未指定 ID 不应被刷新");
    }

    // =========================================================
    // mark_events_absorbed 事务化测试（v1.7 决策 D-V17-014-23）
    // =========================================================
    // 与 L1 版 mark_absorbed 对齐为事务化执行（杜绝事件半吸收），
    // 批次边界与指定 ID 语义测试覆盖行为一致性。

    #[tokio::test]
    async fn mark_events_absorbed_empty_slice_is_noop() {
        let (storage, persona_uid, _, _) = setup_with_persona().await;
        let _id = create_test_event(&storage, &persona_uid, "事件A").await;

        storage.mark_events_absorbed(&[]).await.unwrap();

        let remaining = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
        assert_eq!(remaining.len(), 1, "空切片不应吸收任何事件");
    }

    #[tokio::test]
    async fn mark_events_absorbed_batch_boundary_transactional() {
        // 101 条事件跨批次（100 + 1）在单事务中全部吸收（无半吸收）
        let (storage, persona_uid, _, _) = setup_with_persona().await;
        let mut ids = Vec::new();
        for i in 0..101 {
            ids.push(create_test_event(&storage, &persona_uid, &format!("事件{i}")).await);
        }

        storage.mark_events_absorbed(&ids).await.unwrap();

        let remaining = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
        assert!(
            remaining.is_empty(),
            "101 条跨批次应全部吸收（事务保证无半吸收），实际剩余: {}",
            remaining.len()
        );
    }

    #[tokio::test]
    async fn mark_events_absorbed_only_absorbs_specified_ids() {
        // 仅指定事件被吸收，未指定的不受影响
        let (storage, persona_uid, _, _) = setup_with_persona().await;
        let mut ids = Vec::new();
        for i in 0..5 {
            ids.push(create_test_event(&storage, &persona_uid, &format!("事件{i}")).await);
        }

        // 只吸收前 3 条
        storage.mark_events_absorbed(&ids[..3]).await.unwrap();

        let remaining = storage.list_unabsorbed_events(&persona_uid).await.unwrap();
        assert_eq!(remaining.len(), 2, "应剩余 2 条未吸收");
    }

    // =========================================================
    // background_jobs CRUD 集成测试
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

    // =========================================================
    // Utt Blocks（原文话语块）
    // =========================================================

    /// 辅助：创建 persona + session + 若干消息，返回 (storage, persona_uid, session_id)。
    async fn setup_utt_context() -> (SqliteStorage, String, Uuid) {
        let storage = setup().await;
        let p = Persona::new(
            "char-0001".into(),
            "测试角色".into(),
            PersonaKind::Char,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();
        let session = storage.create_session(Some("char-0001")).await.unwrap();

        // 插入 3 条消息作为块内原文（utt_blocks FK→messages）
        for (i, text) in ["你好呀", "最近怎么样", "挺好的"].iter().enumerate() {
            let msg = Message::new(
                session.id,
                MessageRole::User,
                text.to_string(),
                MessageSource::Local,
            )
            .with_persona_uid(Some("char-0001".to_string()));
            // 时间递增，保证 created_at 有序
            let mut m = msg;
            m.created_at = 1_700_000_000_000 + i as i64 * 60_000;
            storage.save_message(&m).await.unwrap();
        }
        (storage, "char-0001".to_string(), session.id)
    }

    #[tokio::test]
    async fn utt_block_insert_and_get_latest() {
        let (storage, persona_uid, session_id) = setup_utt_context().await;
        let messages = storage.list_messages(session_id).await.unwrap();

        let block = UttBlock::new(
            persona_uid.clone(),
            session_id,
            messages[0].id,
            messages[2].id,
            "你好呀\n最近怎么样\n挺好的".to_string(),
            3,
            120_000,
        );
        let id = storage.insert_utt_block(&block).await.unwrap();
        assert!(id > 0, "插入应返回自增 id");

        let latest = storage
            .get_latest_utt_block_by_session(session_id)
            .await
            .unwrap()
            .expect("会话应有最新话语块");
        assert_eq!(latest.id, id);
        assert_eq!(latest.persona_uid, persona_uid);
        assert_eq!(latest.msg_count, 3);
        assert_eq!(latest.time_span_ms, 120_000);
        assert_eq!(latest.block_text, "你好呀\n最近怎么样\n挺好的");
        assert!(latest.embedding.is_none(), "未设置 embedding 时应为 None");
    }

    #[tokio::test]
    async fn utt_block_list_by_persona_isolation() {
        let (storage, persona_uid, session_id) = setup_utt_context().await;
        let messages = storage.list_messages(session_id).await.unwrap();

        // persona A 插入 2 个块
        for n in 0..2 {
            let block = UttBlock::new(
                persona_uid.clone(),
                session_id,
                messages[0].id,
                messages[2].id,
                format!("块{n}"),
                1,
                0,
            );
            storage.insert_utt_block(&block).await.unwrap();
        }

        // persona B（不同 uid）查询 → 严格隔离，看不到 persona A 的块
        let other = storage
            .list_utt_blocks_by_persona("char-9999")
            .await
            .unwrap();
        assert!(other.is_empty(), "跨 persona 不应看到原文块");

        let mine = storage
            .list_utt_blocks_by_persona(&persona_uid)
            .await
            .unwrap();
        assert_eq!(mine.len(), 2, "应返回本人 persona 的全部块");
        assert_eq!(mine[0].block_text, "块0");
        assert_eq!(mine[1].block_text, "块1");
    }

    #[tokio::test]
    async fn utt_block_latest_returns_newest() {
        let (storage, persona_uid, session_id) = setup_utt_context().await;
        let messages = storage.list_messages(session_id).await.unwrap();

        // 按时间顺序插入 3 个块
        let mut last_id = 0;
        for n in 0..3 {
            let block = UttBlock::new(
                persona_uid.clone(),
                session_id,
                messages[0].id,
                messages[2].id,
                format!("块{n}"),
                1,
                0,
            );
            last_id = storage.insert_utt_block(&block).await.unwrap();
        }

        let latest = storage
            .get_latest_utt_block_by_session(session_id)
            .await
            .unwrap()
            .expect("应有最新块");
        assert_eq!(latest.id, last_id, "应返回最后插入的块");
        assert_eq!(latest.block_text, "块2");
    }

    // list_messages_by_persona 不再截断（原 LIMIT 200），
    // 导入管线重建能枚举该 persona 的全部消息与 session
    #[tokio::test]
    async fn message_list_by_persona_returns_all_over_200() {
        let storage = setup().await;
        let p = Persona::new(
            "char-p22".into(),
            "P2-2 角色".into(),
            PersonaKind::Char,
            1,
            "local".into(),
        );
        storage.create_persona(&p).await.unwrap();
        let session = storage.create_session(Some("char-p22")).await.unwrap();

        // 写入 250 条消息（> 原 LIMIT 200），横跨 3 个 session 更贴近导入场景
        let total = 250usize;
        for i in 0..total {
            let msg = Message::new(
                session.id,
                MessageRole::User,
                format!("消息{i}"),
                MessageSource::Local,
            )
            .with_persona_uid(Some("char-p22".to_string()));
            let mut m = msg;
            m.created_at = 1_700_000_000_000 + i as i64 * 1000;
            storage.save_message(&m).await.unwrap();
        }

        let all = storage.list_messages_by_persona("char-p22").await.unwrap();
        assert_eq!(all.len(), total, "应返回全部 {} 条消息而非截断", total);

        // 枚举出的 session 集合覆盖该 persona 全部会话
        let sessions: std::collections::HashSet<_> = all.iter().map(|m| m.session_id).collect();
        assert!(sessions.contains(&session.id));
    }

    #[tokio::test]
    async fn utt_block_delete_by_session() {
        let (storage, persona_uid, session_id) = setup_utt_context().await;
        let messages = storage.list_messages(session_id).await.unwrap();

        for n in 0..3 {
            let block = UttBlock::new(
                persona_uid.clone(),
                session_id,
                messages[0].id,
                messages[2].id,
                format!("块{n}"),
                1,
                0,
            );
            storage.insert_utt_block(&block).await.unwrap();
        }

        let deleted = storage
            .delete_utt_blocks_by_session(session_id)
            .await
            .unwrap();
        assert_eq!(deleted, 3, "应删除 3 个块");

        let remaining = storage
            .list_utt_blocks_by_persona(&persona_uid)
            .await
            .unwrap();
        assert!(remaining.is_empty(), "删除后不应残留块");

        // 幂等：再次删除返回 0
        let again = storage
            .delete_utt_blocks_by_session(session_id)
            .await
            .unwrap();
        assert_eq!(again, 0);
    }

    #[tokio::test]
    async fn utt_block_empty_session_returns_none() {
        let (storage, _, session_id) = setup_utt_context().await;
        let latest = storage
            .get_latest_utt_block_by_session(session_id)
            .await
            .unwrap();
        assert!(latest.is_none(), "无块会话应返回 None");
    }

    #[tokio::test]
    async fn utt_block_embedding_roundtrip() {
        let (storage, persona_uid, session_id) = setup_utt_context().await;
        let messages = storage.list_messages(session_id).await.unwrap();

        // 构造 4 维 f32 向量的小端 BLOB
        let vector = vec![0.1f32, 0.2, 0.3, 0.4];
        let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();

        let mut block = UttBlock::new(
            persona_uid,
            session_id,
            messages[0].id,
            messages[2].id,
            "带向量的块".to_string(),
            3,
            60_000,
        );
        block.embedding = Some(blob);
        storage.insert_utt_block(&block).await.unwrap();

        let latest = storage
            .get_latest_utt_block_by_session(session_id)
            .await
            .unwrap()
            .expect("应有块");
        let stored = latest.embedding.expect("embedding 应往返保留");
        assert_eq!(stored.len(), 16, "4 × f32 = 16 字节");
        let back: Vec<f32> = stored
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(back, vector);
    }

    // =========================================================
    // examples repo（v1.4）
    // =========================================================

    async fn setup_example_persona(storage: &SqliteStorage) -> String {
        let persona = ramaria_core::types::Persona::new(
            "char-0001".to_string(),
            "测试角色".to_string(),
            ramaria_core::types::PersonaKind::Char,
            1,
            "local".to_string(),
        );
        storage.create_persona(&persona).await.unwrap();
        "char-0001".to_string()
    }

    fn example(uid: &str, partner: &str, reply: &str) -> ramaria_core::types::PersonaExample {
        let mut e = ramaria_core::types::PersonaExample::new(
            uid.to_string(),
            partner.to_string(),
            reply.to_string(),
        );
        e.tags = Some("测试,话题".to_string());
        e.context = Some("前文".to_string());
        e
    }

    #[tokio::test]
    async fn examples_repo_save_and_list_all() {
        let storage = setup().await;
        let uid = setup_example_persona(&storage).await;

        storage
            .save_example(&example(&uid, "问题甲", "回复内容甲"))
            .await
            .unwrap();
        storage
            .save_example(&example(&uid, "问题乙", "回复内容乙"))
            .await
            .unwrap();

        let all = storage.list_all_examples(&uid).await.unwrap();
        assert_eq!(all.len(), 2, "候选池应包含全部示例");
        assert!(all.iter().all(|e| e.persona_uid == uid));

        // 新入库示例为候选（selected=false）→ list_selected 兼容路径为空
        let selected = storage.list_selected_examples(&uid).await.unwrap();
        assert!(selected.is_empty(), "候选池示例不进入静态 selected 路径");

        // 跨 persona 隔离
        let other = storage.list_all_examples("char-9999").await.unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn examples_repo_find_by_pair() {
        let storage = setup().await;
        let uid = setup_example_persona(&storage).await;
        storage
            .save_example(&example(&uid, "问题甲", "回复内容甲"))
            .await
            .unwrap();

        let hit = storage
            .find_example_by_pair(&uid, "问题甲", "回复内容甲")
            .await
            .unwrap();
        assert!(hit.is_some(), "相同回复对应查重命中");
        assert_eq!(hit.unwrap().id, 1);

        // 内容不同 / 归属不同 → 未命中
        assert!(
            storage
                .find_example_by_pair(&uid, "问题甲", "回复内容乙")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .find_example_by_pair("char-9999", "问题甲", "回复内容甲")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn examples_repo_list_selected_respects_flag() {
        let storage = setup().await;
        let uid = setup_example_persona(&storage).await;

        let mut sel = example(&uid, "问题甲", "回复内容甲");
        sel.selected = true;
        storage.save_example(&sel).await.unwrap();
        storage
            .save_example(&example(&uid, "问题乙", "回复内容乙"))
            .await
            .unwrap();

        let selected = storage.list_selected_examples(&uid).await.unwrap();
        assert_eq!(selected.len(), 1, "仅 selected=1 的示例被静态路径返回");
        assert_eq!(selected[0].partner, "问题甲");

        let all = storage.list_all_examples(&uid).await.unwrap();
        assert_eq!(all.len(), 2, "候选池路径返回全部");
    }

    #[tokio::test]
    async fn examples_repo_save_roundtrip_fields() {
        let storage = setup().await;
        let uid = setup_example_persona(&storage).await;
        let session = storage.create_session(Some(&uid)).await.unwrap();

        let mut e = example(&uid, "问题甲", "回复内容甲");
        e.session_id = Some(session.id);
        storage.save_example(&e).await.unwrap();

        let all = storage.list_all_examples(&uid).await.unwrap();
        assert_eq!(all.len(), 1);
        let got = &all[0];
        assert_eq!(got.session_id, e.session_id);
        assert_eq!(got.tags.as_deref(), Some("测试,话题"));
        assert_eq!(got.context.as_deref(), Some("前文"));
        assert_eq!(got.length, 5, "length = reply 字符数");
        assert!(!got.selected);
    }

    // =========================================================
    // SqliteLlmCache 容量自淘汰（v1.5 C）
    // =========================================================

    /// 写入三条记录（间隔 2ms 保证时间戳可区分顺序），
    /// 返回各 key 供断言。
    async fn fill_cache(cache: &SqliteLlmCache) {
        for (key, resp) in [("k1", "r1"), ("k2", "r2"), ("k3", "r3")] {
            cache
                .put(key, resp, "test-model", "test-version")
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    #[tokio::test]
    async fn llm_cache_evicts_lru_beyond_capacity() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        let cache = SqliteLlmCache::new(pool, 2, CacheEviction::Lru);
        fill_cache(&cache).await;

        // 容量 2，写入 3 条 → 自动淘汰最旧 1 条
        assert_eq!(cache.count().await.unwrap(), 2);
        assert!(
            cache.get("k1").await.unwrap().is_none(),
            "LRU 应淘汰最早写入的 k1"
        );
        assert_eq!(cache.get("k3").await.unwrap().as_deref(), Some("r3"));
    }

    #[tokio::test]
    async fn llm_cache_evicts_fifo_beyond_capacity() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        let cache = SqliteLlmCache::new(pool, 2, CacheEviction::Fifo);
        fill_cache(&cache).await;

        // FIFO 按写入顺序淘汰：即便 k3 先被访问，淘汰的仍是 early 写入的 k1
        assert_eq!(cache.count().await.unwrap(), 2);
        assert!(
            cache.get("k1").await.unwrap().is_none(),
            "FIFO 应按写入时间淘汰最早的 k1"
        );
        assert_eq!(cache.get("k3").await.unwrap().as_deref(), Some("r3"));
    }

    #[tokio::test]
    async fn llm_cache_unlimited_capacity_keeps_all() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        // max_entries=0 表示不限制容量
        let cache = SqliteLlmCache::new(pool, 0, CacheEviction::Lru);
        fill_cache(&cache).await;
        assert_eq!(cache.count().await.unwrap(), 3, "不限制容量时不应淘汰");
    }

    #[tokio::test]
    async fn llm_cache_hit_refreshes_lru_order() {
        let pool = database::init_test_pool().await.expect("测试库初始化失败");
        let cache = SqliteLlmCache::new(pool, 2, CacheEviction::Lru);
        for (key, resp) in [("k1", "r1"), ("k2", "r2")] {
            cache.put(key, resp, "m", "v").await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        // 命中 k1 刷新其访问时间 → 之后写入 k3 时应淘汰 k2（而非 k1）
        assert_eq!(cache.get("k1").await.unwrap().as_deref(), Some("r1"));
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        cache.put("k3", "r3", "m", "v").await.unwrap();
        assert_eq!(cache.count().await.unwrap(), 2);
        assert!(
            cache.get("k1").await.unwrap().is_some(),
            "被命中的 k1 应保留"
        );
        assert!(
            cache.get("k2").await.unwrap().is_none(),
            "LRU 应淘汰未命中的 k2"
        );
    }
}
