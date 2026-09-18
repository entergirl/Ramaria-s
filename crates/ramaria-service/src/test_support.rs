//! crates/ramaria-service/src/test_support.rs - 服务层测试脚手架（仅测试编译）
//!
//! 设计特点:
//! - 真实 SQLite（临时文件库 + 全量 migration）：用例测试直接验证存储交互，不用 mock 顶替
//! - 最小 LLM mock：满足 `LlmProvider` 契约但不发起任何网络调用（CI 无外网依赖）；
//!   提供"空回复"与"固定回复"两种口径，覆盖只读用例与封存生成 L1 的写用例
//! - 统一脚手架：临时目录、引擎装配、persona / L1 / 消息造数在各用例测试间复用，
//!   避免每个模块各写一套 mock 导致口径漂移
//! - 调用方负责删除临时目录（`std::fs::remove_dir_all`）

use std::path::PathBuf;
use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::{ChatRequest, LlmProvider, StorageBackend, StoreCrud, StreamDelta};
use ramaria_core::types::{
    BackendConfig, MemoryL1, Message, MessageRole, MessageSource, ModelCapability, Persona,
    PersonaKind,
};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

use crate::engine::{Engine, EngineOptions};

/// 符合 L1 摘要 JSON 契约的固定 LLM 响应（供封存用例测试"LLM 可用"路径）。
pub(crate) const L1_JSON_REPLY: &str = r#"{
  "summary": "用户最近工作压力很大，聊到常常加班到深夜。",
  "keywords": "工作压力,加班",
  "time_period": "夜间",
  "atmosphere": "疲惫",
  "valence": -0.5,
  "salience": 0.8,
  "situation_strength": 4
}"#;

// =========================================================
// 最小 LLM mock
// =========================================================

/// 最小 LLM mock：服务层用例测试不依赖真实 LLM，也不发起网络调用。
///
/// 两种口径:
/// - [`MockLlm::local`]：`chat` 返回空串（模拟"调用成功但无内容"，适合只读用例）；
/// - [`MockLlm::with_reply`]：`chat` 返回固定文本（模拟"LLM 可用"，供封存生成 L1 等写用例）。
pub(crate) struct MockLlm {
    backend: BackendConfig,
    reply: Option<String>,
}

impl MockLlm {
    /// 本地 LM Studio 口径的 mock（无 API key、无网络；chat 返回空串）。
    pub(crate) fn local() -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: None,
        }
    }

    /// 返回固定文本的 mock（供需要 LLM 真实响应的用例，如 L1 摘要 JSON）。
    pub(crate) fn with_reply(reply: &str) -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: Some(reply.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        Ok(self.reply.clone().unwrap_or_default())
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<
        std::pin::Pin<Box<dyn futures::Stream<Item = RamariaResult<StreamDelta>> + Send>>,
    > {
        // 服务层用例不消费流式回复；显式报错避免测试误以为有真实生成能力
        Err(RamariaError::unsupported("MockLlm 不支持流式生成"))
    }

    fn capability(&self) -> &ModelCapability {
        &self.backend.capability
    }

    fn config(&self) -> &BackendConfig {
        &self.backend
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MockLlm"
    }
}

// =========================================================
// 引擎装配
// =========================================================

/// 创建唯一临时目录（调用方负责清理）。
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-service-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    dir
}

/// 装配一套"真实 SQLite + 无回复 mock LLM + 无嵌入"的引擎。
pub(crate) async fn engine_with_db(tag: &str) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_llm(tag, MockLlm::local()).await
}

/// 装配一套"真实 SQLite + 固定回复 mock LLM + 无嵌入"的引擎（LLM 可用路径）。
pub(crate) async fn engine_with_l1_reply(
    tag: &str,
    reply: &str,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_llm(tag, MockLlm::with_reply(reply)).await
}

/// 以指定 LLM 装配引擎（测试统一的装配入口）。
async fn engine_with_llm(tag: &str, llm: MockLlm) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    let dir = temp_dir(tag);
    let db_path = dir.join("assistant.db");
    // 先做一次装配（建库 + migration），再以同一路径构造测试可见的存储句柄
    let _ = Engine::open_with(EngineOptions::new(db_path.clone()))
        .await
        .expect("引擎装配应成功");
    let storage = Arc::new(SqliteStorage::new(
        ramaria_storage::database::init_pool(Some(db_path))
            .await
            .expect("测试库初始化应成功"),
    ));
    let engine = Engine::from_parts(
        storage.clone() as Arc<dyn StorageBackend>,
        Arc::new(llm),
        None,
        RamariaConfig::default(),
    );
    (engine, storage, dir)
}

// =========================================================
// 造数
// =========================================================

/// 造一个 persona 行（L1 / facts / sessions 的外键依赖）。
pub(crate) async fn seed_persona(storage: &SqliteStorage, uid: &str) {
    let persona = Persona::new(
        uid.to_string(),
        "测试人格".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("插入 persona 应成功");
}

/// 造一条带 persona 归属的 L1 摘要（自动建会话满足外键）。
pub(crate) async fn seed_l1(
    storage: &SqliteStorage,
    persona: &str,
    summary: &str,
    keywords: Option<&str>,
    created_at: i64,
) -> Uuid {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话应成功");
    let mut l1 = MemoryL1::new(session.id, summary.to_string(), None);
    l1.persona_uid = Some(persona.to_string());
    l1.keywords = keywords.map(str::to_string);
    l1.created_at = created_at;
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    l1.id
}

/// 造 N 条会话消息（角色按 user / assistant 交替，created_at 自 base 递增）。
pub(crate) async fn seed_messages(
    storage: &SqliteStorage,
    session_id: Uuid,
    persona: &str,
    count: usize,
    base_ts: i64,
) {
    for i in 0..count {
        let role = if i % 2 == 0 {
            MessageRole::User
        } else {
            MessageRole::Assistant
        };
        let mut message = Message::new(
            session_id,
            role,
            format!("消息内容 {i}"),
            MessageSource::Local,
        )
        .with_persona_uid(Some(persona.to_string()));
        message.created_at = base_ts + i as i64;
        storage
            .save_message(&message)
            .await
            .expect("写入消息应成功");
    }
}

/// 造一个带消息的活跃会话（供封存 / 空闲检查用例）。
pub(crate) async fn seed_session_with_messages(
    storage: &SqliteStorage,
    persona: &str,
    count: usize,
    base_ts: i64,
) -> Uuid {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话应成功");
    seed_messages(storage, session.id, persona, count, base_ts).await;
    session.id
}

/// 造一个带通道标识的活跃会话 + 消息（供"外部对话超时另起"等回流用例）。
pub(crate) async fn seed_channel_session(
    storage: &SqliteStorage,
    persona: &str,
    channel: &str,
    external_ref: Option<&str>,
    count: usize,
    base_ts: i64,
) -> Uuid {
    let session = storage
        .create_session_in_channel(Some(persona), channel, external_ref)
        .await
        .expect("创建通道会话应成功");
    seed_messages(storage, session.id, persona, count, base_ts).await;
    session.id
}
