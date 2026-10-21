//! crates/ramaria-testkit/src/mock/storage/trait_impl.rs - MockStorage 的存储 trait 转发实现
//!
//! 设计特点:
//! - 单一 trait impl：全部方法在此显式声明并转发到分域固有方法
//! - 保留 trait 默认语义的方法同样显式覆写，确保统一经过失败注入守卫
//! - 转发方法一行一条并跳过 rustfmt：trait impl 不可跨文件拆分，压缩后便于逐项核对
//!
//! 说明:
//! - 方法顺序与 trait 声明一致；参数名与返回类型逐字取自契约。
//! - 未建模能力通过固有方法内的 panic 暴露，不静默返回默认值。

use ramaria_core::behavior::{BehaviorRule, FeedbackLog};
use ramaria_core::error::RamariaResult;
use ramaria_core::keyword::{KeywordPoolRow, PendingAliasRow};
use ramaria_core::traits::{
    IndexCorpusStamp, ProactiveDeliveryPair, StoreCrud, StoreInfrastructure,
};
use ramaria_core::types::{
    AttachmentStatus, BackendConfig, ChannelBinding, ClusterSnapshot, EventBatchWrite,
    EventRelation, EventSource, MemoryEvent, MemoryL1, MemoryVector, Message, MessageAttachment,
    MessageKey, Persona, PersonaEventAggregate, PersonaExample, PersonaFact, PersonaStyleStats,
    PersonalityTrait, PrivacyConsent, ProfileField, Session, SessionMember, StickerAsset,
    TraitEvidence, TraitSource, TraitStatus, UttBlock,
};
use std::collections::HashMap;
use uuid::Uuid;

use super::MockStorage;

#[rustfmt::skip]
#[async_trait::async_trait]
impl StoreCrud for MockStorage {
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> { self.create_session(persona_uid).await }
    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()> { self.close_session(session_id).await }
    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>> { self.get_session(session_id).await }
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> { self.list_active_sessions().await }
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> { self.list_sessions().await }
    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> { self.delete_session(session_id).await }
    async fn delete_session_cascade(&self, _session_id: Uuid) -> RamariaResult<()> { self.delete_session_cascade(_session_id).await }
    async fn bind_session_persona_uid(&self, _session_id: Uuid, _persona_uid: &str) -> RamariaResult<()> { self.bind_session_persona_uid(_session_id, _persona_uid).await }
    async fn create_session_in_channel(&self, _persona_uid: Option<&str>, _channel: &str, _external_ref: Option<&str>) -> RamariaResult<Session> { self.create_session_in_channel(_persona_uid, _channel, _external_ref).await }
    async fn find_active_session_by_channel(&self, _channel: &str, _external_ref: Option<&str>) -> RamariaResult<Option<Session>> { self.find_active_session_by_channel(_channel, _external_ref).await }
    async fn close_session_if_active(&self, _session_id: Uuid) -> RamariaResult<bool> { self.close_session_if_active(_session_id).await }
    async fn upsert_session_members(&self, _members: &[SessionMember]) -> RamariaResult<()> { self.upsert_session_members(_members).await }
    async fn list_session_members(&self, _session_id: Uuid) -> RamariaResult<Vec<SessionMember>> { self.list_session_members(_session_id).await }
    async fn list_closed_sessions_missing_l1(&self, _limit: u32) -> RamariaResult<Vec<Uuid>> { self.list_closed_sessions_missing_l1(_limit).await }
    async fn upsert_channel_binding(&self, _channel: &str, _external_ref: &str, _persona_uid: Option<&str>, _enabled: bool, _allow_proactive: bool) -> RamariaResult<ChannelBinding> { self.upsert_channel_binding(_channel, _external_ref, _persona_uid, _enabled, _allow_proactive).await }
    async fn get_channel_binding(&self, _channel: &str, _external_ref: &str) -> RamariaResult<Option<ChannelBinding>> { self.get_channel_binding(_channel, _external_ref).await }
    async fn list_channel_bindings(&self) -> RamariaResult<Vec<ChannelBinding>> { self.list_channel_bindings().await }
    async fn delete_channel_binding(&self, _channel: &str, _external_ref: &str) -> RamariaResult<()> { self.delete_channel_binding(_channel, _external_ref).await }
    async fn save_message(&self, message: &Message) -> RamariaResult<()> { self.save_message(message).await }
    async fn get_message(&self, _id: Uuid) -> RamariaResult<Option<Message>> { self.get_message(_id).await }
    async fn find_message_by_platform_id(&self, _session_id: Uuid, _platform_message_id: &str) -> RamariaResult<Option<Message>> { self.find_message_by_platform_id(_session_id, _platform_message_id).await }
    async fn mark_message_recalled(&self, _session_id: Uuid, _platform_message_id: &str, _recalled_at: i64) -> RamariaResult<u64> { self.mark_message_recalled(_session_id, _platform_message_id, _recalled_at).await }
    async fn mark_message_recalled_by_id(&self, _message_id: Uuid, _recalled_at: i64) -> RamariaResult<bool> { self.mark_message_recalled_by_id(_message_id, _recalled_at).await }
    async fn has_assistant_message_after(&self, _session_id: Uuid, _after_ms: i64) -> RamariaResult<bool> { self.has_assistant_message_after(_session_id, _after_ms).await }
    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>> { self.list_messages(session_id).await }
    async fn list_messages_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<Message>> { self.list_messages_by_persona(persona_uid).await }
    async fn list_messages_paginated(&self, session_id: Uuid, limit: i64, offset: i64) -> RamariaResult<Vec<Message>> { self.list_messages_paginated(session_id, limit, offset).await }
    async fn list_messages_by_persona_paginated(&self, persona_uid: &str, limit: i64, offset: i64) -> RamariaResult<Vec<Message>> { self.list_messages_by_persona_paginated(persona_uid, limit, offset).await }
    async fn get_last_message_time(&self, _session_id: Uuid) -> RamariaResult<Option<i64>> { self.get_last_message_time(_session_id).await }
    async fn last_message_time_by_persona(&self, _persona_uid: &str) -> RamariaResult<Option<i64>> { self.last_message_time_by_persona(_persona_uid).await }
    async fn last_user_message_time_by_persona(&self, _persona_uid: &str) -> RamariaResult<Option<i64>> { self.last_user_message_time_by_persona(_persona_uid).await }
    async fn has_local_user_message_by_persona(&self, _persona_uid: &str) -> RamariaResult<bool> { self.has_local_user_message_by_persona(_persona_uid).await }
    async fn mark_session_read(&self, _session_id: Uuid, _at_ms: i64) -> RamariaResult<()> { self.mark_session_read(_session_id, _at_ms).await }
    async fn list_unread_counts(&self) -> RamariaResult<HashMap<Uuid, u32>> { self.list_unread_counts().await }
    async fn last_message_time_by_session(&self, _session_id: Uuid) -> RamariaResult<Option<i64>> { self.last_message_time_by_session(_session_id).await }
    async fn list_user_message_times_since(&self, _persona_uid: &str, _since_ms: i64) -> RamariaResult<Vec<i64>> { self.list_user_message_times_since(_persona_uid, _since_ms).await }
    async fn list_persona_message_times_since(&self, _persona_uid: &str, _since_ms: i64) -> RamariaResult<Vec<i64>> { self.list_persona_message_times_since(_persona_uid, _since_ms).await }
    async fn list_proactive_delivery_pairs(&self, _persona_uid: &str, _response_window_ms: i64) -> RamariaResult<Vec<ProactiveDeliveryPair>> { self.list_proactive_delivery_pairs(_persona_uid, _response_window_ms).await }
    async fn count_messages(&self, session_id: Uuid) -> RamariaResult<u32> { self.count_messages(session_id).await }
    async fn count_messages_by_session(&self) -> RamariaResult<HashMap<Uuid, u32>> { self.count_messages_by_session().await }
    async fn find_message_by_fingerprint(&self, _fingerprint: &str) -> RamariaResult<Option<Message>> { self.find_message_by_fingerprint(_fingerprint).await }
    async fn list_message_keys_by_channel_ref(&self, _channel: &str, _external_ref: Option<&str>) -> RamariaResult<Vec<MessageKey>> { self.list_message_keys_by_channel_ref(_channel, _external_ref).await }
    async fn insert_message_attachments(&self, _attachments: &[MessageAttachment]) -> RamariaResult<()> { self.insert_message_attachments(_attachments).await }
    async fn list_pending_attachments_by_session(&self, _session_id: Uuid, _limit: u32) -> RamariaResult<Vec<MessageAttachment>> { self.list_pending_attachments_by_session(_session_id, _limit).await }
    async fn list_attachments_by_messages(&self, _message_ids: &[Uuid]) -> RamariaResult<Vec<MessageAttachment>> { self.list_attachments_by_messages(_message_ids).await }
    async fn fill_attachment_done_by_md5(&self, _md5: &str, _description: &str, _description_model: &str) -> RamariaResult<u64> { self.fill_attachment_done_by_md5(_md5, _description, _description_model).await }
    async fn mark_attachment_done(&self, _id: i64, _description: &str, _description_model: &str) -> RamariaResult<()> { self.mark_attachment_done(_id, _description, _description_model).await }
    async fn mark_attachment_status(&self, _id: i64, _status: AttachmentStatus) -> RamariaResult<()> { self.mark_attachment_status(_id, _status).await }
    async fn find_attachment_description_by_md5(&self, _md5: &str) -> RamariaResult<Option<(String, String)>> { self.find_attachment_description_by_md5(_md5).await }
    async fn list_all_pending_attachments(&self, _limit: u32) -> RamariaResult<Vec<MessageAttachment>> { self.list_all_pending_attachments(_limit).await }
    async fn list_all_skipped_attachments(&self, _limit: u32) -> RamariaResult<Vec<MessageAttachment>> { self.list_all_skipped_attachments(_limit).await }
    async fn reset_attachment_for_rescan(&self, _id: i64, _source_ref: Option<&str>) -> RamariaResult<()> { self.reset_attachment_for_rescan(_id, _source_ref).await }
    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()> { self.save_memory_l1(memory).await }
    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>> { self.list_memory_l1(session_id).await }
    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>> { self.get_memory_l1(id).await }
    async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()> { self.mark_l1_absorbed(l1_ids).await }
    async fn delete_memory_l1_by_session(&self, session_id: Uuid) -> RamariaResult<usize> { self.delete_memory_l1_by_session(session_id).await }
    async fn list_unabsorbed_l1(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>> { self.list_unabsorbed_l1(persona_uid).await }
    async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> { self.list_unabsorbed_l1_unbound().await }
    async fn assign_l1_persona_uid(&self, _l1_ids: &[Uuid], _persona_uid: &str) -> RamariaResult<usize> { self.assign_l1_persona_uid(_l1_ids, _persona_uid).await }
    async fn list_recent_l1_by_persona(&self, _persona_uid: &str, _limit: u32) -> RamariaResult<Vec<MemoryL1>> { self.list_recent_l1_by_persona(_persona_uid, _limit).await }
    async fn touch_l1(&self, _l1_ids: &[Uuid], _now_ms: i64) -> RamariaResult<()> { self.touch_l1(_l1_ids, _now_ms).await }
    async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64> { self.create_persona(persona).await }
    async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>> { self.get_persona_by_uid(uid).await }
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> { self.list_personas().await }
    async fn update_persona(&self, uid: &str, name: &str, avatar: Option<&str>, config: Option<&str>, description: Option<&str>) -> RamariaResult<()> { self.update_persona(uid, name, avatar, config, description).await }
    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64> { self.save_event(event).await }
    async fn get_event(&self, _id: i64) -> RamariaResult<Option<MemoryEvent>> { self.get_event(_id).await }
    async fn list_events_by_persona(&self, persona_uid: &str, offset: i64, limit: i64) -> RamariaResult<Vec<MemoryEvent>> { self.list_events_by_persona(persona_uid, offset, limit).await }
    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>> { self.list_unabsorbed_events(persona_uid).await }
    async fn list_events_by_salience(&self, _persona_uid: &str, _since_ms: i64, _min_salience: f64, _limit: u32) -> RamariaResult<Vec<MemoryEvent>> { self.list_events_by_salience(_persona_uid, _since_ms, _min_salience, _limit).await }
    async fn list_events_since(&self, _persona_uid: &str, _since_ms: i64, _limit: u32) -> RamariaResult<Vec<MemoryEvent>> { self.list_events_since(_persona_uid, _since_ms, _limit).await }
    async fn count_events_by_persona(&self, persona_uid: &str) -> RamariaResult<u64> { self.count_events_by_persona(persona_uid).await }
    async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()> { self.mark_events_absorbed(event_ids).await }
    async fn aggregate_persona_event_priors(&self, _exclude_persona_uid: &str) -> RamariaResult<Vec<PersonaEventAggregate>> { self.aggregate_persona_event_priors(_exclude_persona_uid).await }
    async fn save_event_relation(&self, rel: &EventRelation) -> RamariaResult<i64> { self.save_event_relation(rel).await }
    async fn list_event_relations_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<EventRelation>> { self.list_event_relations_by_persona(_persona_uid).await }
    async fn save_event_source(&self, event_id: i64, l1_id: Uuid, weight: f64) -> RamariaResult<()> { self.save_event_source(event_id, l1_id, weight).await }
    async fn save_event_batch(&self, batch: &EventBatchWrite) -> RamariaResult<Vec<i64>> { self.save_event_batch(batch).await }
    async fn list_event_sources_by_event(&self, _event_id: i64) -> RamariaResult<Vec<EventSource>> { self.list_event_sources_by_event(_event_id).await }
    async fn list_event_session_map(&self, _event_ids: &[i64]) -> RamariaResult<HashMap<i64, Uuid>> { self.list_event_session_map(_event_ids).await }
    async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64> { self.save_fact(fact).await }
    async fn list_facts_by_persona(&self, persona_uid: &str, field: ProfileField) -> RamariaResult<Vec<PersonaFact>> { self.list_facts_by_persona(persona_uid, field).await }
    async fn list_active_facts_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<PersonaFact>> { self.list_active_facts_by_persona(_persona_uid).await }
    async fn list_active_facts_by_field(&self, _persona_uid: &str, _field: ProfileField) -> RamariaResult<Vec<PersonaFact>> { self.list_active_facts_by_field(_persona_uid, _field).await }
    async fn list_all_facts_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<PersonaFact>> { self.list_all_facts_by_persona(_persona_uid).await }
    async fn get_fact_by_id(&self, _id: i64) -> RamariaResult<Option<PersonaFact>> { self.get_fact_by_id(_id).await }
    async fn save_fact_with_version(&self, _old: &PersonaFact, _f: &PersonaFact) -> RamariaResult<i64> { self.save_fact_with_version(_old, _f).await }
    async fn list_fact_versions(&self, _seed_id: i64) -> RamariaResult<Vec<PersonaFact>> { self.list_fact_versions(_seed_id).await }
    async fn count_all_facts_for_persona(&self, persona_uid: &str) -> RamariaResult<Vec<(ProfileField, usize)>> { self.count_all_facts_for_persona(persona_uid).await }
    async fn upsert_style_stats(&self, _stats: &PersonaStyleStats) -> RamariaResult<()> { self.upsert_style_stats(_stats).await }
    async fn get_style_stats(&self, _persona_uid: &str) -> RamariaResult<Option<PersonaStyleStats>> { self.get_style_stats(_persona_uid).await }
    async fn save_trait(&self, t: &PersonalityTrait) -> RamariaResult<i64> { self.save_trait(t).await }
    async fn list_traits_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<PersonalityTrait>> { self.list_traits_by_persona(persona_uid).await }
    async fn update_trait_confidence(&self, id: i64, confidence: f64, evidence: f64, consistency: f64) -> RamariaResult<()> { self.update_trait_confidence(id, confidence, evidence, consistency).await }
    async fn update_trait_status(&self, id: i64, status: TraitStatus) -> RamariaResult<()> { self.update_trait_status(id, status).await }
    async fn update_trait_source(&self, _id: i64, _source: TraitSource) -> RamariaResult<()> { self.update_trait_source(_id, _source).await }
    async fn save_evidence(&self, e: &TraitEvidence) -> RamariaResult<i64> { self.save_evidence(e).await }
    async fn list_evidence_by_trait(&self, trait_id: i64) -> RamariaResult<Vec<TraitEvidence>> { self.list_evidence_by_trait(trait_id).await }
    async fn save_example(&self, e: &PersonaExample) -> RamariaResult<i64> { self.save_example(e).await }
    async fn list_selected_examples(&self, persona_uid: &str) -> RamariaResult<Vec<PersonaExample>> { self.list_selected_examples(persona_uid).await }
    async fn list_all_examples(&self, _persona_uid: &str) -> RamariaResult<Vec<PersonaExample>> { self.list_all_examples(_persona_uid).await }
    async fn find_example_by_pair(&self, _persona_uid: &str, _partner: &str, _reply: &str) -> RamariaResult<Option<PersonaExample>> { self.find_example_by_pair(_persona_uid, _partner, _reply).await }
    async fn insert_utt_block(&self, _block: &UttBlock) -> RamariaResult<i64> { self.insert_utt_block(_block).await }
    async fn list_utt_blocks_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<UttBlock>> { self.list_utt_blocks_by_persona(_persona_uid).await }
    async fn get_latest_utt_block_by_session(&self, _session_id: Uuid) -> RamariaResult<Option<UttBlock>> { self.get_latest_utt_block_by_session(_session_id).await }
    async fn delete_utt_block(&self, _id: i64) -> RamariaResult<()> { self.delete_utt_block(_id).await }
    async fn delete_utt_blocks_by_session(&self, _session_id: Uuid) -> RamariaResult<usize> { self.delete_utt_blocks_by_session(_session_id).await }
    async fn save_cluster_snapshot(&self, s: &ClusterSnapshot) -> RamariaResult<i64> { self.save_cluster_snapshot(s).await }
    async fn get_current_snapshots(&self, persona_uid: &str, category: &str) -> RamariaResult<Vec<ClusterSnapshot>> { self.get_current_snapshots(persona_uid, category).await }
    async fn get_all_snapshots_with_embeddings(&self, persona_uid: &str) -> RamariaResult<Vec<ClusterSnapshot>> { self.get_all_snapshots_with_embeddings(persona_uid).await }
    async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()> { self.upsert_keyword(keyword).await }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> { self.list_keywords().await }
    async fn seed_keyword_canonical(&self, _keyword: &str) -> RamariaResult<bool> { self.seed_keyword_canonical(_keyword).await }
    async fn list_canonical_keywords(&self) -> RamariaResult<Vec<String>> { self.list_canonical_keywords().await }
    async fn list_established_keywords(&self) -> RamariaResult<Vec<String>> { self.list_established_keywords().await }
    async fn list_keyword_pool_entries(&self) -> RamariaResult<Vec<KeywordPoolRow>> { self.list_keyword_pool_entries().await }
    async fn list_keyword_statuses(&self, _keywords: &[String]) -> RamariaResult<Vec<(String, Option<String>)>> { self.list_keyword_statuses(_keywords).await }
    async fn list_pending_aliases(&self) -> RamariaResult<Vec<PendingAliasRow>> { self.list_pending_aliases().await }
    async fn confirm_keyword_alias(&self, _alias_id: i64) -> RamariaResult<bool> { self.confirm_keyword_alias(_alias_id).await }
    async fn reject_keyword_alias(&self, _alias_id: i64) -> RamariaResult<bool> { self.reject_keyword_alias(_alias_id).await }
    async fn upsert_pending_alias(&self, _alias: &str, _canonical_id: i64, _use_count: u32) -> RamariaResult<bool> { self.upsert_pending_alias(_alias, _canonical_id, _use_count).await }
    async fn insert_sticker_asset(&self, _asset: &StickerAsset) -> RamariaResult<i64> { self.insert_sticker_asset(_asset).await }
    async fn list_sticker_assets(&self) -> RamariaResult<Vec<StickerAsset>> { self.list_sticker_assets().await }
    async fn get_sticker_asset(&self, _id: i64) -> RamariaResult<Option<StickerAsset>> { self.get_sticker_asset(_id).await }
    async fn update_sticker_asset(&self, _id: i64, _description: Option<&str>, _tags: Option<&[String]>) -> RamariaResult<bool> { self.update_sticker_asset(_id, _description, _tags).await }
    async fn delete_sticker_asset(&self, _id: i64) -> RamariaResult<bool> { self.delete_sticker_asset(_id).await }
}

#[rustfmt::skip]
#[async_trait::async_trait]
impl StoreInfrastructure for MockStorage {
    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()> { self.save_privacy_consent(consent).await }
    async fn get_privacy_consent(&self, provider: &str, base_url: &str) -> RamariaResult<Option<PrivacyConsent>> { self.get_privacy_consent(provider, base_url).await }
    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()> { self.save_backend_config(config).await }
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> { self.get_backend_config().await }
    async fn get_schema_version(&self) -> RamariaResult<i32> { self.get_schema_version().await }
    async fn index_corpus_stamp(&self) -> RamariaResult<Option<IndexCorpusStamp>> { self.index_corpus_stamp().await }
    async fn upsert_memory_vector(&self, _vector: &MemoryVector) -> RamariaResult<()> { self.upsert_memory_vector(_vector).await }
    async fn load_memory_vectors(&self, _model: &str) -> RamariaResult<Vec<MemoryVector>> { self.load_memory_vectors(_model).await }
    async fn delete_memory_vector(&self, _doc_type: &str, _doc_id: &str) -> RamariaResult<()> { self.delete_memory_vector(_doc_type, _doc_id).await }
    async fn delete_memory_vectors_by_model(&self, _model: &str) -> RamariaResult<()> { self.delete_memory_vectors_by_model(_model).await }
    async fn prune_orphan_memory_vectors(&self) -> RamariaResult<u64> { self.prune_orphan_memory_vectors().await }
    async fn create_background_job(&self, job_type: &str, payload: Option<&str>) -> RamariaResult<i64> { self.create_background_job(job_type, payload).await }
    async fn update_job_status(&self, id: i64, status: &str, error: Option<&str>) -> RamariaResult<()> { self.update_job_status(id, status, error).await }
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> { self.list_pending_jobs().await }
    async fn claim_pending_job(&self, id: i64) -> RamariaResult<bool> { self.claim_pending_job(id).await }
    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> { self.get_setting(key).await }
    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> { self.set_setting(key, value).await }
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> { self.list_settings().await }
    async fn get_bm25_index_version(&self) -> RamariaResult<i32> { self.get_bm25_index_version().await }
    async fn set_bm25_index_version(&self, version: i32) -> RamariaResult<()> { self.set_bm25_index_version(version).await }
    async fn l2_fingerprint_exists(&self, _persona_uid: &str, _fingerprint: &str) -> RamariaResult<bool> { self.l2_fingerprint_exists(_persona_uid, _fingerprint).await }
    async fn save_l2_fingerprint(&self, _persona_uid: &str, _fingerprint: &str) -> RamariaResult<()> { self.save_l2_fingerprint(_persona_uid, _fingerprint).await }
    async fn list_recent_events(&self, _persona_uid: &str, _limit: u32) -> RamariaResult<Vec<MemoryEvent>> { self.list_recent_events(_persona_uid, _limit).await }
    async fn save_behavior_rule(&self, _rule: &BehaviorRule) -> RamariaResult<i64> { self.save_behavior_rule(_rule).await }
    async fn get_behavior_rule(&self, _id: i64) -> RamariaResult<Option<BehaviorRule>> { self.get_behavior_rule(_id).await }
    async fn list_behavior_rules_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<BehaviorRule>> { self.list_behavior_rules_by_persona(_persona_uid).await }
    async fn update_behavior_rule(&self, _rule: &BehaviorRule) -> RamariaResult<()> { self.update_behavior_rule(_rule).await }
    async fn delete_behavior_rule(&self, _id: i64) -> RamariaResult<()> { self.delete_behavior_rule(_id).await }
    async fn set_rule_enabled(&self, _id: i64, _enabled: bool) -> RamariaResult<()> { self.set_rule_enabled(_id, _enabled).await }
    async fn save_feedback_log(&self, _log: &FeedbackLog) -> RamariaResult<i64> { self.save_feedback_log(_log).await }
    async fn list_feedback_logs_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<FeedbackLog>> { self.list_feedback_logs_by_persona(_persona_uid).await }
}
