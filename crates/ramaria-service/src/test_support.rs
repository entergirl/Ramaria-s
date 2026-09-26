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
use ramaria_core::traits::{
    ChatRequest, EmbeddingProvider, LlmProvider, StorageBackend, StoreCrud, StreamDelta,
};
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
/// 口径:
/// - [`MockLlm::local`]：`chat` 返回空串（模拟"调用成功但无内容"，适合只读用例）；
/// - [`MockLlm::with_reply`]：`chat` 返回固定文本（模拟"LLM 可用"，供封存生成 L1 等写用例）；
/// - [`MockLlm::failing`]：`chat` 恒失败（模拟后端不可用，覆盖降级与"不落半条"路径）；
/// - [`MockLlm::with_health_failures`]：健康探测前 N 次失败（模拟后端启动中，覆盖探测重试）。
pub(crate) struct MockLlm {
    backend: BackendConfig,
    reply: Option<String>,
    /// 为 true 时所有生成调用返回 Llm 错误（不发起网络调用）。
    always_fail: bool,
    /// 健康探测剩余失败次数（递减；0 表示探测成功）。
    health_failures: std::sync::atomic::AtomicUsize,
    /// `chat` 调用计数（含失败；供"幂等跳过不重复调用 LLM"类断言使用）。
    chat_calls: std::sync::atomic::AtomicUsize,
}

impl MockLlm {
    /// 本地 LM Studio 口径的 mock（无 API key、无网络；chat 返回空串）。
    pub(crate) fn local() -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: None,
            always_fail: false,
            health_failures: std::sync::atomic::AtomicUsize::new(0),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 返回固定文本的 mock（供需要 LLM 真实响应的用例，如 L1 摘要 JSON）。
    pub(crate) fn with_reply(reply: &str) -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: Some(reply.to_string()),
            always_fail: false,
            health_failures: std::sync::atomic::AtomicUsize::new(0),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 恒失败的 mock（模拟"LLM 后端不可用"，用于降级与不落半条写入的断言）。
    pub(crate) fn failing() -> Self {
        Self {
            backend: BackendConfig::lm_studio_default(),
            reply: None,
            always_fail: true,
            health_failures: std::sync::atomic::AtomicUsize::new(0),
            chat_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 指定健康探测前 N 次失败（链式调用；N = 0 表示探测直接成功）。
    pub(crate) fn with_health_failures(self, failures: usize) -> Self {
        Self {
            health_failures: std::sync::atomic::AtomicUsize::new(failures),
            ..self
        }
    }

    /// `chat` 调用次数（含成功与失败；健康探测不计入）。
    pub(crate) fn chat_calls(&self) -> usize {
        self.chat_calls.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        self.chat_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.always_fail {
            return Err(RamariaError::llm("MockLlm 恒失败（模拟 LLM 后端不可用）"));
        }
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

    /// 健康探测：按剩余失败次数返回错误（模拟后端启动中，之后转为可达）。
    async fn health_check(&self) -> RamariaResult<()> {
        use std::sync::atomic::Ordering;

        if self.health_failures.load(Ordering::Acquire) > 0 {
            self.health_failures.fetch_sub(1, Ordering::AcqRel);
            return Err(RamariaError::llm("MockLlm 健康探测失败（模拟后端未就绪）"));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MockLlm"
    }
}

// =========================================================
// 最小嵌入 mock
// =========================================================

/// 最小嵌入 mock：确定性向量（无模型文件、无网络、无随机性）。
///
/// 职责:
/// - 让"嵌入模型已加载"的路径在无模型环境下可测（热更新 / 读取 / 可用性判定）；
/// - 提供固定维度，供维度断言复用。
pub(crate) struct DeterministicEmbedding {
    info: ramaria_core::traits::EmbeddingModelInfo,
}

impl DeterministicEmbedding {
    /// 向量维度（足够区分测试文本即可）。
    pub(crate) const DIMENSION: usize = 16;

    /// 构造可用的嵌入 provider。
    pub(crate) fn new() -> Self {
        Self {
            info: ramaria_core::traits::EmbeddingModelInfo {
                model_id: "mock-deterministic-embedding".to_string(),
                dimension: Self::DIMENSION,
            },
        }
    }

    /// 计算确定性向量（字符哈希分桶后 L2 归一化）。
    fn vector(text: &str) -> Vec<f32> {
        use std::hash::{Hash, Hasher};

        let mut vector = vec![0.0_f32; Self::DIMENSION];
        for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            ch.hash(&mut hasher);
            let index = (hasher.finish() as usize) % Self::DIMENSION;
            vector[index] += 1.0;
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::EmbeddingProvider for DeterministicEmbedding {
    async fn embed(&self, text: &str) -> RamariaResult<Vec<f32>> {
        Ok(Self::vector(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|text| Self::vector(text)).collect())
    }

    fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
        self.info.clone()
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
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
    engine_with_llm_and_config(tag, llm, RamariaConfig::default()).await
}

/// 装配一套"真实 SQLite + 恒失败 LLM + 无嵌入"的引擎（LLM 不可用降级路径）。
pub(crate) async fn engine_with_failing_llm(tag: &str) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    engine_with_llm(tag, MockLlm::failing()).await
}

/// 以指定 LLM 与生效配置装配引擎（供需要覆盖阈值 / 开关等配置项的用例）。
pub(crate) async fn engine_with_llm_and_config(
    tag: &str,
    llm: MockLlm,
    config: RamariaConfig,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    assemble_engine(tag, Arc::new(llm), None, config).await
}

/// 以指定 LLM 与嵌入 provider 装配引擎（覆盖向量通道 / 状态机等用例）。
pub(crate) async fn engine_with_llm_config_and_embedding(
    tag: &str,
    llm: MockLlm,
    config: RamariaConfig,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    assemble_engine(tag, Arc::new(llm), embedding, config).await
}

/// 以共享句柄注入 LLM mock（调用计数等断言用），并可指定嵌入 provider。
pub(crate) async fn engine_with_shared_llm(
    tag: &str,
    llm: Arc<MockLlm>,
    config: RamariaConfig,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    let provider: Arc<dyn LlmProvider> = llm;
    assemble_engine(tag, provider, embedding, config).await
}

/// 统一装配实现：建库（含 migration）→ 以注入依赖构造引擎。
async fn assemble_engine(
    tag: &str,
    llm: Arc<dyn LlmProvider>,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
    config: RamariaConfig,
) -> (Engine, Arc<SqliteStorage>, PathBuf) {
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
        llm,
        embedding,
        config,
    );
    (engine, storage, dir)
}

/// 在**已存在的库路径**上装配第二台引擎（多进程并发场景的服务层等价物）。
///
/// 职责:
/// - 模拟"桌面 + MCP 并存"：同一库文件、独立连接池与内存索引，各自持有 LLM provider；
/// - 用于验证抢占幂等（同一会话只生成一份 L1）与写锁争用（并发写不报 locked）。
///
/// 参数:
/// - `db_path`: 已由首台引擎建好并执行过 migration 的库文件路径。
/// - `llm` / `config`: 第二台引擎的 LLM provider 与生效配置。
pub(crate) async fn engine_on_existing_db(
    db_path: &std::path::Path,
    llm: MockLlm,
    config: RamariaConfig,
) -> Engine {
    let pool = ramaria_storage::database::init_pool(Some(db_path.to_path_buf()))
        .await
        .expect("第二连接池应可创建");
    let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));
    Engine::from_parts(storage, Arc::new(llm), None, config)
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
