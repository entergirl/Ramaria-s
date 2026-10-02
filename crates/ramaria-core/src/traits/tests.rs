//! crates/ramaria-core/src/traits/tests.rs - Ramaria 核心能力抽象模块单元测试
//!
//! 设计特点:
//! - 覆盖 LLM 请求/片段与 Embedding 模型信息的构造
//! - 通过 BareStore mock 仅实现必需方法，其余走 trait 默认实现
//! - 校验契约关键方法未覆写时显式返回 Unsupported 而非静默空结果
//! - 依赖父模块 re-export 与类型导入，保持断言零改写

use super::*;

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::RamariaResult;
use crate::types::{
    BackendConfig, ClusterSnapshot, EventRelation, MemoryEvent, MemoryL1, Message, MessageRole,
    Persona, PersonaExample, PersonaFact, PersonalityTrait, PrivacyConsent, ProfileField, Session,
    TraitEvidence, TraitStatus,
};

/// 验证 trait 可通过 trait object 引用，便于后续 mock 或依赖注入。
/// （原 trait_definitions_exist 为空验证，编译期已保证，已删除）

#[test]
fn chat_request_construction() {
    let req = ChatRequest {
        system_prompt: "你是一个助手".into(),
        memory_context: Some("用户偏好：喜欢猫".into()),
        history: vec![ChatMessage {
            role: MessageRole::User,
            content: "你好".into(),
        }],
        user_message: "今天天气如何？".into(),
        temperature: 0.3,
        max_tokens: 1024,
        request_id: Uuid::new_v4(),
        template_version: "test".into(),
    };
    assert_eq!(req.temperature, 0.3);
    assert!(req.memory_context.is_some());
    assert!(!req.history.is_empty());
    assert!(!req.template_version.is_empty());
}

#[test]
fn stream_delta_construction() {
    let delta = StreamDelta {
        content: "今天".into(),
        done: false,
        metadata: None,
    };
    assert!(!delta.done);
    assert_eq!(delta.content, "今天");
}

#[test]
fn embedding_model_info() {
    let info = EmbeddingModelInfo {
        model_id: "bge-small-zh".into(),
        dimension: 512,
    };
    assert_eq!(info.dimension, 512);
    assert_eq!(info.model_id, "bge-small-zh");
}

// =========================================================
// 契约关键方法默认实现回归：未覆写 → 显式 Unsupported
// =========================================================

/// 最小存储后端 mock：只实现无默认值的必需方法，其余全部走 trait 默认实现。
///
/// 用途:
/// - 验证"契约关键方法未覆写时返回 `Unsupported`"的约定，
///   防止静默空结果被上层误读为"确实无数据"。
struct BareStore;

/// 统一错误体：所有必需方法返回 `Unsupported`。
macro_rules! bare_store_err {
    () => {
        Err(crate::error::RamariaError::unsupported("BareStore 未实现"))
    };
}

#[async_trait]
impl StoreCrud for BareStore {
    async fn create_session(&self, _: Option<&str>) -> RamariaResult<Session> {
        bare_store_err!()
    }
    async fn close_session(&self, _: Uuid) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn get_session(&self, _: Uuid) -> RamariaResult<Option<Session>> {
        bare_store_err!()
    }
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>> {
        bare_store_err!()
    }
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>> {
        bare_store_err!()
    }
    async fn delete_session(&self, _: Uuid) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn save_message(&self, _: &Message) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn list_messages(&self, _: Uuid) -> RamariaResult<Vec<Message>> {
        bare_store_err!()
    }
    async fn list_messages_by_persona(&self, _: &str) -> RamariaResult<Vec<Message>> {
        bare_store_err!()
    }
    async fn save_memory_l1(&self, _: &MemoryL1) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn list_memory_l1(&self, _: Uuid) -> RamariaResult<Vec<MemoryL1>> {
        bare_store_err!()
    }
    async fn get_memory_l1(&self, _: Uuid) -> RamariaResult<Option<MemoryL1>> {
        bare_store_err!()
    }
    async fn mark_l1_absorbed(&self, _: &[Uuid]) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn list_unabsorbed_l1(&self, _: &str) -> RamariaResult<Vec<MemoryL1>> {
        bare_store_err!()
    }
    async fn create_persona(&self, _: &Persona) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn get_persona_by_uid(&self, _: &str) -> RamariaResult<Option<Persona>> {
        bare_store_err!()
    }
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>> {
        bare_store_err!()
    }
    async fn update_persona(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
    ) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn save_event(&self, _: &MemoryEvent) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn list_events_by_persona(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        bare_store_err!()
    }
    async fn list_unabsorbed_events(&self, _: &str) -> RamariaResult<Vec<MemoryEvent>> {
        bare_store_err!()
    }
    async fn mark_events_absorbed(&self, _: &[i64]) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn save_event_relation(&self, _: &EventRelation) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn save_event_source(&self, _: i64, _: Uuid, _: f64) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn save_fact(&self, _: &PersonaFact) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn list_facts_by_persona(
        &self,
        _: &str,
        _: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        bare_store_err!()
    }
    async fn save_trait(&self, _: &PersonalityTrait) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn list_traits_by_persona(&self, _: &str) -> RamariaResult<Vec<PersonalityTrait>> {
        bare_store_err!()
    }
    async fn update_trait_confidence(&self, _: i64, _: f64, _: f64, _: f64) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn update_trait_status(&self, _: i64, _: TraitStatus) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn save_evidence(&self, _: &TraitEvidence) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn list_evidence_by_trait(&self, _: i64) -> RamariaResult<Vec<TraitEvidence>> {
        bare_store_err!()
    }
    async fn save_example(&self, _: &PersonaExample) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn list_selected_examples(&self, _: &str) -> RamariaResult<Vec<PersonaExample>> {
        bare_store_err!()
    }
    async fn save_cluster_snapshot(&self, _: &ClusterSnapshot) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn get_current_snapshots(&self, _: &str, _: &str) -> RamariaResult<Vec<ClusterSnapshot>> {
        bare_store_err!()
    }
    async fn upsert_keyword(&self, _: &str) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        bare_store_err!()
    }
}

#[async_trait]
impl StoreInfrastructure for BareStore {
    async fn insert_keyword_ref(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: f64,
    ) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn save_privacy_consent(&self, _: &PrivacyConsent) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn get_privacy_consent(&self, _: &str, _: &str) -> RamariaResult<Option<PrivacyConsent>> {
        bare_store_err!()
    }
    async fn save_backend_config(&self, _: &BackendConfig) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        bare_store_err!()
    }
    async fn get_schema_version(&self) -> RamariaResult<i32> {
        bare_store_err!()
    }
    async fn get_index_version(&self) -> RamariaResult<i32> {
        bare_store_err!()
    }
    async fn set_index_version(&self, _: i32) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn create_background_job(&self, _: &str, _: Option<&str>) -> RamariaResult<i64> {
        bare_store_err!()
    }
    async fn update_job_status(&self, _: i64, _: &str, _: Option<&str>) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        bare_store_err!()
    }
    async fn get_setting(&self, _: &str) -> RamariaResult<Option<String>> {
        bare_store_err!()
    }
    async fn set_setting(&self, _: &str, _: &str) -> RamariaResult<()> {
        bare_store_err!()
    }
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        bare_store_err!()
    }
}

/// 未覆写契约关键方法时必须显式报 `Unsupported`（错误可见），
/// 而不是静默返回空列表 / `None` 让上层误判为"确实无数据"。
#[test]
fn bare_store_contract_methods_return_unsupported() {
    let store = BareStore;
    let errors = vec![
        futures::executor::block_on(store.delete_session_cascade(Uuid::new_v4()))
            .expect_err("delete_session_cascade 未覆写应报错"),
        futures::executor::block_on(store.get_last_message_time(Uuid::new_v4()))
            .expect_err("get_last_message_time 未覆写应报错"),
        futures::executor::block_on(store.list_recent_events("persona-1", 10))
            .expect_err("list_recent_events 未覆写应报错"),
        futures::executor::block_on(store.list_keyword_pool_entries())
            .expect_err("list_keyword_pool_entries 未覆写应报错"),
        futures::executor::block_on(store.upsert_pending_alias("压力", 1, 3))
            .expect_err("upsert_pending_alias 未覆写应报错"),
        futures::executor::block_on(store.create_session_in_channel(None, "mcp", None))
            .expect_err("create_session_in_channel 未覆写应报错"),
        futures::executor::block_on(store.find_active_session_by_channel("mcp", None))
            .expect_err("find_active_session_by_channel 未覆写应报错"),
        futures::executor::block_on(store.close_session_if_active(Uuid::new_v4()))
            .expect_err("close_session_if_active 未覆写应报错"),
    ];
    for err in errors {
        assert_eq!(
            err.category(),
            "unsupported",
            "未覆写方法应返回 Unsupported: {err}"
        );
    }
}
