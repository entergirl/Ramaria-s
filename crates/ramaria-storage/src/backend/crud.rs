//! crates/ramaria-storage/src/backend/crud.rs - Ramaria 存储后端 StoreCrud 实现模块
//!
//! 设计特点:
//! - 实现 `StoreCrud`：会话、消息、L1 摘要、L2 事件与关系、人物事实、风格统计、L3 性格与证据、示例、话语块、聚类快照、关键词池
//! - 纯代理：调用转发到 `repo` 对应子模块，SQL 逻辑集中在一处
//! - 个别方法显式覆写为高效实现（SQL 分页、GROUP BY 聚合计数、幂等写入、条件 UPDATE）
//! - 所有可恢复错误由 repo 层统一转换为 RamariaError::Storage

use std::collections::HashMap;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::keyword::{KeywordPoolRow, PendingAliasRow};
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    ClusterSnapshot, EventBatchWrite, EventRelation, EventSource, MemoryEvent, MemoryL1, Message,
    Persona, PersonaEventAggregate, PersonaExample, PersonaFact, PersonaStyleStats,
    PersonalityTrait, ProfileField, Session, TraitEvidence, TraitStatus, UttBlock,
};
use uuid::Uuid;

use super::SqliteStorage;
use crate::repo;

#[async_trait::async_trait]
impl StoreCrud for SqliteStorage {
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
    /// 覆写为事务内按外键依赖顺序显式删除各关联表（消息/utt 块/L1/反馈/示例）。
    async fn delete_session_cascade(&self, session_id: Uuid) -> RamariaResult<()> {
        repo::sessions::delete_cascade(&self.pool, session_id).await
    }
    async fn bind_session_persona_uid(
        &self,
        session_id: Uuid,
        persona_uid: &str,
    ) -> RamariaResult<()> {
        repo::sessions::bind_persona_uid(&self.pool, session_id, persona_uid).await
    }
    async fn create_session_in_channel(
        &self,
        persona_uid: Option<&str>,
        channel: &str,
        external_ref: Option<&str>,
    ) -> RamariaResult<Session> {
        repo::sessions::create_in_channel(&self.pool, persona_uid, channel, external_ref).await
    }
    async fn find_active_session_by_channel(
        &self,
        channel: &str,
        external_ref: Option<&str>,
    ) -> RamariaResult<Option<Session>> {
        repo::sessions::find_active_by_channel(&self.pool, channel, external_ref).await
    }
    async fn close_session_if_active(&self, session_id: Uuid) -> RamariaResult<bool> {
        repo::sessions::close_if_active(&self.pool, session_id).await
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
    /// 覆写为高效 SQL 分页（`ORDER BY created_at DESC LIMIT ? OFFSET ?`）。
    async fn list_messages_by_persona_paginated(
        &self,
        persona_uid: &str,
        limit: i64,
        offset: i64,
    ) -> RamariaResult<Vec<Message>> {
        repo::messages::list_by_persona_paginated(&self.pool, persona_uid, limit, offset).await
    }
    async fn get_last_message_time(&self, session_id: Uuid) -> RamariaResult<Option<i64>> {
        repo::messages::get_last_message_time(&self.pool, session_id).await
    }
    /// 覆写为 sessions/messages 联合查询（主动对话调度的空闲门禁）。
    async fn last_message_time_by_persona(&self, persona_uid: &str) -> RamariaResult<Option<i64>> {
        repo::sessions::last_message_time_by_persona(&self.pool, persona_uid).await
    }
    async fn count_messages(&self, session_id: Uuid) -> RamariaResult<u32> {
        repo::messages::count_by_session(&self.pool, session_id).await
    }
    /// 覆写为单条 GROUP BY 聚合（会话列表一次取回全部计数，替代逐会话 COUNT）。
    async fn count_messages_by_session(&self) -> RamariaResult<HashMap<Uuid, u32>> {
        repo::messages::count_by_sessions(&self.pool).await
    }
    /// 覆写为指纹精确查询（外部入口回流去重）。
    async fn find_message_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> RamariaResult<Option<Message>> {
        repo::messages::find_by_fingerprint(&self.pool, fingerprint).await
    }
    /// 覆写为跨会话按通道取去重键（外部对话重复提交去重）。
    async fn list_message_keys_by_channel_ref(
        &self,
        channel: &str,
        external_ref: Option<&str>,
    ) -> RamariaResult<Vec<ramaria_core::types::MessageKey>> {
        repo::messages::list_keys_by_channel_ref(&self.pool, channel, external_ref).await
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
    /// 覆写为 `SELECT COUNT(*)`（事件浏览的分页总数）。
    async fn count_events_by_persona(&self, persona_uid: &str) -> RamariaResult<u64> {
        repo::events::count_by_persona(&self.pool, persona_uid).await
    }
    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> {
        repo::events::list_unabsorbed_events(&self.pool, persona_uid).await
    }

    async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()> {
        repo::events::mark_absorbed(&self.pool, event_ids).await
    }

    async fn aggregate_persona_event_priors(
        &self,
        exclude_persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaEventAggregate>> {
        repo::events::aggregate_persona_event_priors(&self.pool, exclude_persona_uid).await
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

    /// 单事务写入事件批次（事件 + 来源 + 关系 + L1 吸收标记）。
    async fn save_event_batch(&self, batch: &EventBatchWrite) -> RamariaResult<Vec<i64>> {
        repo::events::save_event_batch(&self.pool, batch).await
    }

    async fn list_event_sources_by_event(&self, event_id: i64) -> RamariaResult<Vec<EventSource>> {
        repo::events::list_sources_by_event(&self.pool, event_id).await
    }

    /// 覆写为批量映射查询（event_sources → memory_l1.session_id）。
    async fn list_event_session_map(&self, event_ids: &[i64]) -> RamariaResult<HashMap<i64, Uuid>> {
        repo::events::list_session_map(&self.pool, event_ids).await
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
    async fn list_fact_versions(&self, seed_id: i64) -> RamariaResult<Vec<PersonaFact>> {
        repo::facts::list_versions(&self.pool, seed_id).await
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
        // keyword_pool 只接受标准化后的合法词条；非法 token 显式报错，
        // 避免"仅 warn 后 Ok(())"让调用方误以为写入成功（静默丢词）。
        let token = ramaria_core::keyword::KeywordToken::new(keyword)
            .ok_or_else(|| RamariaError::validation("关键词非法（空/超长），拒绝写入词条池"))?;
        repo::keyword::upsert(&self.pool, &token).await
    }
    /// 覆写为主键冲突 DO NOTHING 的幂等注入（已存在保持现状，不改任何列）。
    async fn seed_keyword_canonical(&self, keyword: &str) -> RamariaResult<bool> {
        // 与 upsert_keyword 同一防御口径：非法词条显式拒绝，不静默丢词。
        let token = ramaria_core::keyword::KeywordToken::new(keyword)
            .ok_or_else(|| RamariaError::validation("关键词非法（空/超长），拒绝写入词条池"))?;
        repo::keyword::seed_canonical(&self.pool, &token).await
    }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        let tokens = repo::keyword::list_all(&self.pool).await?;
        Ok(tokens.into_iter().map(|t| t.into_inner()).collect())
    }
    async fn list_canonical_keywords(&self) -> RamariaResult<Vec<String>> {
        let rows = repo::keyword::list_canonicals(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.keyword).collect())
    }
    async fn list_established_keywords(&self) -> RamariaResult<Vec<String>> {
        let rows = repo::keyword::list_established(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.keyword).collect())
    }
    async fn list_keyword_pool_entries(&self) -> RamariaResult<Vec<KeywordPoolRow>> {
        repo::keyword::list_pool_rows(&self.pool).await
    }
    /// 覆写为 join 规范词文本的待确认别名查询。
    async fn list_pending_aliases(&self) -> RamariaResult<Vec<PendingAliasRow>> {
        repo::keyword::list_pending_aliases(&self.pool).await
    }
    /// 覆写为条件 UPDATE（仅命中 pending 行，未命中返回 false）。
    async fn confirm_keyword_alias(&self, alias_id: i64) -> RamariaResult<bool> {
        repo::keyword::confirm_alias(&self.pool, alias_id).await
    }
    /// 覆写为条件 UPDATE（仅命中 pending 行，未命中返回 false）。
    async fn reject_keyword_alias(&self, alias_id: i64) -> RamariaResult<bool> {
        repo::keyword::reject_alias(&self.pool, alias_id).await
    }
    /// 覆写为主键冲突 DO NOTHING 的幂等登记（已存在行保持现状，不改状态与计数）。
    async fn upsert_pending_alias(
        &self,
        alias: &str,
        canonical_id: i64,
        use_count: u32,
    ) -> RamariaResult<bool> {
        // 与 upsert_keyword 同一防御口径：非法词条显式拒绝，不静默丢词。
        let token = ramaria_core::keyword::KeywordToken::new(alias)
            .ok_or_else(|| RamariaError::validation("关键词非法（空/超长），拒绝写入词条池"))?;
        repo::keyword::upsert_pending(&self.pool, &token, canonical_id, use_count).await
    }
}
