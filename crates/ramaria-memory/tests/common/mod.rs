//! ramaria-memory 集成测试共享基础设施（T-V13-2-016 / T-V13-3-010 收尾补齐）
//!
//! 提供:
//! - `MockLlm`: 实现 `LlmProviderTrait` 的 mock LLM（预设 JSON 回复 + 最近请求记录）
//! - `mem_storage`: 内存 SQLite（跑 v1.3 + v1.4 migration）→ `SqliteStorage`
//! - fixture 构造辅助：分组关键词 L1、mock embedding（预计算向量）

// 每个测试二进制独立编译本模块，各目标使用子集不同 → 允许未使用项
#![allow(dead_code)]

use std::pin::Pin;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::Stream;
use ramaria_core::traits::{ChatRequest, StorageBackend, StreamDelta};
use ramaria_core::types::{
    BackendConfig, MemoryL1, ModelCapability, Persona, PersonaKind, Session, now_ms,
};
use ramaria_core::{LlmProviderTrait, RamariaResult};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

// =========================================================
// Mock LLM
// =========================================================

/// 测试用 mock LLM：chat 恒返回预设文本，并记录最近一次请求。
pub struct MockLlm {
    reply: Mutex<String>,
    capability: ModelCapability,
    config: BackendConfig,
    /// 最近一次 chat 请求（prompt 断言用）
    last_request: Mutex<Option<ChatRequest>>,
    /// 累计 chat 调用次数（v1.5 L2 指纹测试断言"不重复调用 LLM"用）
    calls: std::sync::atomic::AtomicU32,
}

impl MockLlm {
    pub fn new(reply: &str) -> Self {
        let config = BackendConfig::lm_studio_default();
        Self {
            reply: Mutex::new(reply.to_string()),
            capability: config.capability.clone(),
            config,
            last_request: Mutex::new(None),
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// 最近一次 chat 请求内容。
    pub fn last_request(&self) -> Option<ChatRequest> {
        self.last_request.lock().unwrap().clone()
    }

    /// 累计 chat 调用次数（供"同集合跳过时不重复调用 LLM"断言）。
    pub fn call_count(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 预设/更新 chat 回复。
    pub fn set_reply(&self, reply: &str) {
        *self.reply.lock().unwrap() = reply.to_string();
    }
}

#[async_trait]
impl LlmProviderTrait for MockLlm {
    async fn chat(&self, request: &ChatRequest) -> RamariaResult<String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_request.lock().unwrap() = Some(request.clone());
        Ok(self.reply.lock().unwrap().clone())
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        // 事件提取链路仅使用非流式 chat；返回空流而非 unimplemented，
        // 防止未来链路切换流式后测试以 panic 形式爆炸。
        let empty: Vec<RamariaResult<StreamDelta>> = Vec::new();
        Ok(Box::pin(futures::stream::iter(empty)))
    }

    fn capability(&self) -> &ModelCapability {
        &self.capability
    }

    fn config(&self) -> &BackendConfig {
        &self.config
    }

    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MockLlm"
    }
}

// =========================================================
// 内存 SQLite 存储
// =========================================================

/// 创建内存 SQLite 存储（跑全部 migration）。
pub async fn mem_storage() -> SqliteStorage {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("内存测试数据库创建失败");
    sqlx::migrate!("../ramaria-storage/migrations")
        .run(&pool)
        .await
        .expect("测试 migration 失败");
    SqliteStorage::new(pool)
}

// =========================================================
// Fixture 构造
// =========================================================

/// 创建 persona 并返回 uid。
pub async fn create_persona(storage: &SqliteStorage, uid: &str, name: &str) -> String {
    let persona = Persona::new(
        uid.to_string(),
        name.to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("创建 persona 失败");
    uid.to_string()
}

/// 创建会话并返回 session。
pub async fn create_session(storage: &SqliteStorage, persona_uid: Option<&str>) -> Session {
    storage
        .create_session(persona_uid)
        .await
        .expect("创建 session 失败")
}

/// 构造一条未吸收 L1（默认 absorbed=false，persona_uid 关联）。
pub fn make_l1(
    session_id: Uuid,
    summary: &str,
    keywords: &str,
    persona_uid: &str,
    salience: f64,
    created_at: i64,
) -> MemoryL1 {
    let mut l1 = MemoryL1::new(session_id, summary.to_string(), Some("下午".to_string()));
    l1.keywords = Some(keywords.to_string());
    l1.persona_uid = Some(persona_uid.to_string());
    l1.salience = salience;
    l1.created_at = created_at;
    l1
}

/// 分组关键词构造辅助：把一组 L1 摘要配给一个主题。
///
/// 返回 (summary, keywords) 列表，keywords 为逗号分隔的主题词。
pub fn topic_fixture(
    topic_words: &[&str],
    count: usize,
    summary_prefix: &str,
) -> Vec<(String, String)> {
    let kw = topic_words.join(",");
    (0..count)
        .map(|i| (format!("{summary_prefix} 第{i}条"), kw.clone()))
        .collect()
}

/// 当前时间（Unix 毫秒）。
pub fn now() -> i64 {
    now_ms()
}
