//! crates/ramaria-service/tests/channel_memory_e2e.rs - 社交通道记忆链路端到端核对
//!
//! 设计特点:
//! - 真实 SQLite 临时库 + 队列式 mock LLM：入站写入 → 封存 → L1 逐目标投影 →
//!   索引镜像 → 召回 / L2 候选池按人独立，全链路在真实存储语义上执行
//! - 断言只经服务层公开用例（channel_ingest / seal / recall）与存储查询读取结果，
//!   不绕过被测路径；本地会话同链路回归单行口径
//! - 不触网、不加载真实模型；临时目录由用例自行清理
//! - 隐私: 测试仅用合成消息与固定回复，不依赖真实数据

use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, stream};
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider, StorageBackend, StoreCrud, StreamDelta};
use ramaria_core::types::{
    BackendConfig, InboundMessage, InboundSender, Message, MessageRole, MessageSource,
    ModelCapability, Persona, PersonaKind,
};
use ramaria_service::{ChannelIngestRequest, Engine, RecallLayer, RecallPolicy, RecallRequest};
use ramaria_storage::SqliteStorage;

// =========================================================
// mock LLM（队列式）
// =========================================================

/// 队列式 mock LLM：逐次消费预设回复（队列用尽回落空串）。
struct QueuedLlm {
    replies: Mutex<VecDeque<String>>,
    backend: BackendConfig,
}

impl QueuedLlm {
    fn new(replies: Vec<String>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            backend: BackendConfig::lm_studio_default(),
        }
    }
}

#[async_trait]
impl LlmProvider for QueuedLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        let reply = self
            .replies
            .lock()
            .expect("mock LLM 回复队列锁不应中毒")
            .pop_front()
            .unwrap_or_default();
        Ok(reply)
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<Pin<Box<dyn Stream<Item = RamariaResult<StreamDelta>> + Send>>> {
        Ok(Box::pin(stream::empty()))
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
        "QueuedLlm"
    }
}

// =========================================================
// 环境装配与造数
// =========================================================

/// 装配"真实 SQLite + 队列式 mock LLM + 无嵌入"的引擎。
async fn build_engine(
    tag: &str,
    replies: Vec<String>,
) -> (Arc<Engine>, Arc<SqliteStorage>, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-channel-memory-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    let db_path = dir.join("e2e.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("测试库初始化应成功");
    let storage = Arc::new(SqliteStorage::new(pool));
    let llm: Arc<dyn LlmProvider> = Arc::new(QueuedLlm::new(replies));
    let engine = Engine::from_parts(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        llm,
        None,
        RamariaConfig::default(),
    );
    engine.set_recall_policy(RecallPolicy::default());
    (Arc::new(engine), storage, dir)
}

/// 造一个 char 类型 persona 行。
async fn seed_persona(storage: &SqliteStorage, uid: &str) {
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

/// 构造一条 QQ 群聊入站消息。
fn inbound(text: &str, uin: &str, name: &str, message_id: &str, created_at: i64) -> InboundMessage {
    InboundMessage {
        channel: "qq".to_string(),
        platform_message_id: Some(message_id.to_string()),
        reply_to: None,
        created_at,
        text: text.to_string(),
        sender: InboundSender {
            platform_id: format!("u-{uin}"),
            uin: Some(uin.to_string()),
            display_name: name.to_string(),
            group_nickname: None,
            role: None,
        },
        attachments: vec![],
    }
}

/// 构造一条 L1 摘要 mock 响应。
fn l1_json(summary: &str) -> String {
    serde_json::json!({
        "summary": summary,
        "keywords": "测试,关键词",
        "time_period": "上午",
        "atmosphere": "平静",
        "valence": 0.0,
        "salience": 0.5
    })
    .to_string()
}

// =========================================================
// 社交群聊记忆链路
// =========================================================

/// 群聊链路：入站写入 → 封存逐目标 → 召回 / L2 候选池按人独立。
#[tokio::test]
async fn social_group_memory_chain() {
    // 目标顺序：绑定人格首位，块内发言者按条数降序（char-0001 ×2 → char-0002 ×1）
    let (engine, storage, dir) = build_engine(
        "social-group",
        vec![
            l1_json("本机侧专属回顾"),
            l1_json("成员一专属回顾"),
            l1_json("成员二专属回顾"),
        ],
    )
    .await;
    seed_persona(&storage, "rama-0001").await;
    seed_persona(&storage, "char-0001").await;
    seed_persona(&storage, "char-0002").await;
    engine.ensure_index_loaded().await.expect("索引加载应成功");

    // 绑定（收到即建语义之外的显式建档；允许主动，供后续投放开关消费）
    let binding = engine
        .channel_set_binding("qq", "group:9001", None, None, Some(true))
        .await
        .expect("绑定写入应成功");
    assert!(binding.allow_proactive, "绑定应允许主动");

    // 入站写入 4 条（本机侧 ×1 assistant + char-0001 ×2 + char-0002 ×1）
    // 时间取当前附近：避免触发"空闲先封存再另起"（同一活跃会话内续写）
    let base = ramaria_core::types::now_ms() - 60_000;
    let messages = [
        inbound("大家好，我是小拉", "9999", "小拉", "m-1", base),
        inbound("今天天气不错", "0001", "小明", "m-2", base + 100),
        inbound("下午一起打球", "0001", "小明", "m-3", base + 200),
        inbound("我也去", "0002", "小红", "m-4", base + 300),
    ];
    let mut session_id = None;
    for message in messages {
        let outcome = engine
            .channel_ingest(ChannelIngestRequest {
                channel: "qq".to_string(),
                external_ref: "group:9001".to_string(),
                persona_uid: "rama-0001".to_string(),
                self_id: Some("9999".to_string()),
                message,
            })
            .await
            .expect("入站写入应成功");
        assert!(outcome.written, "每条事件应实际写入");
        session_id = Some(outcome.session_id);
    }
    let session_id = session_id.expect("应有写入产生会话");

    // 消息归属：本机侧 assistant + 人类成员 char 锚点 + user
    let stored = storage
        .list_messages(session_id)
        .await
        .expect("读取消息应成功");
    assert_eq!(stored.len(), 4);
    let self_msg = stored
        .iter()
        .find(|m| m.platform_message_id.as_deref() == Some("m-1"))
        .expect("应有本机侧消息");
    assert_eq!(self_msg.role, MessageRole::Assistant);
    assert_eq!(self_msg.persona_uid.as_deref(), Some("rama-0001"));
    let member_msg = stored
        .iter()
        .find(|m| m.platform_message_id.as_deref() == Some("m-2"))
        .expect("应有成员消息");
    assert_eq!(member_msg.role, MessageRole::User);
    assert_eq!(member_msg.persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(member_msg.sender_name.as_deref(), Some("小明"));

    // 封存：逐目标投影 → 3 目标 3 行
    let outcome = engine.seal(session_id).await.expect("封存应成功");
    assert!(outcome.sealed);
    assert_eq!(outcome.l1_count, 3, "3 目标应生成 3 行");

    let l1_list = storage
        .list_memory_l1(session_id)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(l1_list.len(), 3);
    let mut personas: Vec<String> = l1_list
        .iter()
        .filter_map(|l1| l1.persona_uid.clone())
        .collect();
    personas.sort();
    assert_eq!(
        personas,
        vec!["char-0001", "char-0002", "rama-0001"],
        "各行归属应为绑定人格与各成员"
    );

    // 召回按人独立：各自查询命中自己的行且不混入他人
    let recalled_char = engine
        .recall(RecallRequest {
            query: Some("专属回顾".to_string()),
            persona: Some("char-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("char-0001 召回应成功");
    assert!(
        recalled_char
            .items
            .iter()
            .any(|item| item.text.contains("成员一专属回顾")),
        "char-0001 应召回本行: {:?}",
        recalled_char.items
    );
    assert!(
        recalled_char
            .items
            .iter()
            .all(|item| !item.text.contains("本机侧专属回顾")),
        "char-0001 不应混入他人行: {:?}",
        recalled_char.items
    );

    let recalled_rama = engine
        .recall(RecallRequest {
            query: Some("专属回顾".to_string()),
            persona: Some("rama-0001".to_string()),
            include: Some(vec![RecallLayer::L1]),
            ..RecallRequest::default()
        })
        .await
        .expect("rama-0001 召回应成功");
    assert!(
        recalled_rama
            .items
            .iter()
            .any(|item| item.text.contains("本机侧专属回顾")),
        "rama-0001 应召回本行: {:?}",
        recalled_rama.items
    );
    assert!(
        recalled_rama
            .items
            .iter()
            .all(|item| !item.text.contains("成员一专属回顾")),
        "rama-0001 不应混入成员行: {:?}",
        recalled_rama.items
    );

    // L2 触达独立：未吸收 L1 候选池按人过滤，各自可见且互不串行
    let unabsorbed_char = storage
        .list_unabsorbed_l1("char-0001")
        .await
        .expect("查询候选池应成功");
    assert!(
        unabsorbed_char
            .iter()
            .any(|l1| l1.summary.contains("成员一专属回顾")),
        "char-0001 候选池应含本行: {unabsorbed_char:?}"
    );
    assert!(
        unabsorbed_char
            .iter()
            .all(|l1| !l1.summary.contains("本机侧专属回顾")),
        "char-0001 候选池不应含他人行"
    );
    let unabsorbed_char2 = storage
        .list_unabsorbed_l1("char-0002")
        .await
        .expect("查询候选池应成功");
    assert!(
        unabsorbed_char2
            .iter()
            .any(|l1| l1.summary.contains("成员二专属回顾")),
        "char-0002 候选池应含本行: {unabsorbed_char2:?}"
    );
    let unabsorbed_rama = storage
        .list_unabsorbed_l1("rama-0001")
        .await
        .expect("查询候选池应成功");
    assert!(
        unabsorbed_rama
            .iter()
            .any(|l1| l1.summary.contains("本机侧专属回顾")),
        "rama-0001 候选池应含本行: {unabsorbed_rama:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 本地会话回归
// =========================================================

/// 本地会话同链路：单行 L1（非投影口径零变化）。
#[tokio::test]
async fn local_memory_chain_unchanged() {
    let (engine, storage, dir) = build_engine("local-chain", vec![l1_json("本地单行摘要")]).await;
    seed_persona(&storage, "char-0001").await;

    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建本地会话应成功");
    let base = ramaria_core::types::now_ms() - 60_000;
    let mut user_msg = Message::new(
        session.id,
        MessageRole::User,
        "今天工作好累".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some("char-0001".to_string()));
    user_msg.created_at = base;
    storage
        .save_message(&user_msg)
        .await
        .expect("写入消息应成功");
    let mut assistant_msg = Message::new(
        session.id,
        MessageRole::Assistant,
        "辛苦了，早点休息".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some("char-0001".to_string()));
    assistant_msg.created_at = base + 100;
    storage
        .save_message(&assistant_msg)
        .await
        .expect("写入消息应成功");

    let outcome = engine.seal(session.id).await.expect("封存应成功");
    assert_eq!(outcome.l1_count, 1, "本地会话应单行");

    let l1_list = storage
        .list_memory_l1(session.id)
        .await
        .expect("读取 L1 应成功");
    assert_eq!(l1_list.len(), 1);
    assert_eq!(l1_list[0].persona_uid.as_deref(), Some("char-0001"));
    assert_eq!(l1_list[0].summary, "本地单行摘要");

    let _ = std::fs::remove_dir_all(&dir);
}
