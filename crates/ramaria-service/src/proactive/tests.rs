//! crates/ramaria-service/src/proactive/tests.rs - 主动对话模块冒烟与端到端测试
//!
//! 设计特点:
//! - 空库单轮调度：无候选、无写入、不报错（模块装配与降级路径的冒烟覆盖）
//! - 端到端链路：短间隔后台循环触发 → 选题（打桩或真选题器）→ mock LLM 生成 →
//!   接收端收到，并断言落库与状态记账；关停后循环干净退出
//! - 判据开启路径：脚本化 LLM 先裁决后生成，断言调用序与开口计数
//! - 使用真实 SQLite 临时库与打桩接收端，不依赖网络与真实时钟

use super::schedule::run_tick;
use super::state::{ProactiveState, load_state};
use super::*;
use crate::engine::Engine;
use crate::proactive::sink::ProactiveSink;
use crate::test_support::{
    MockLlm, ScriptedLlm, engine_with_db, engine_with_llm_and_config,
    engine_with_shared_scripted_llm, seed_dialogue_history, seed_persona,
};
use crate::types::DEFAULT_PERSONA_UID;
use async_trait::async_trait;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{AppState, MemoryEvent, MessageRole, now_ms};
use ramaria_storage::SqliteStorage;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 固定回复（供端到端用例断言生成内容）。
const REPLY: &str = "刚想到一个你可能会喜欢的展，周末要不要一起去？";

// =========================================================
// 端到端测试工具
// =========================================================

/// 放行基线配置：主动开关开启、打扰控制闸门全部放行。
fn test_config() -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.proactive.enabled = true;
    config.proactive.min_idle_hours = 0;
    config.proactive.cooldown_hours = 1;
    config.proactive.daily_limit = 3;
    config.proactive.quiet_hours = String::new();
    config.proactive.startup_grace_days = 0;
    config.proactive.judge_enabled = false;
    config
}

/// 单次返回候选的选题器：首次调用取出候选，之后恒返回 None（避免循环多轮重复投递）。
struct OncePicker {
    directive: Mutex<Option<ProactiveDirective>>,
}

impl OncePicker {
    /// 以指定候选构造（取出即空）。
    fn new(directive: ProactiveDirective) -> Self {
        Self {
            directive: Mutex::new(Some(directive)),
        }
    }
}

#[async_trait]
impl TopicPicker for OncePicker {
    async fn pick(
        &self,
        _engine: &Engine,
        _persona: &str,
        _now: i64,
        _state: &mut ProactiveState,
        _activity_weight: f64,
    ) -> Option<ProactiveDirective> {
        self.directive.lock().expect("选题器锁不应中毒").take()
    }
}

/// 投递接收端桩：留档收到的消息。
struct TestSink {
    received: Mutex<Vec<ProactiveMessage>>,
}

impl TestSink {
    /// 投递恒成功。
    fn ok() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
        }
    }

    fn received(&self) -> Vec<ProactiveMessage> {
        self.received.lock().expect("接收端留档锁不应中毒").clone()
    }
}

impl ProactiveSink for TestSink {
    fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()> {
        self.received
            .lock()
            .expect("接收端留档锁不应中毒")
            .push(message.clone());
        Ok(())
    }
}

/// 造一条近窗内的高显著正效价事件（无会话映射：仅高显著源可出口）。
///
/// 返回:
/// - 落库后的事件 id。
async fn seed_salient_event(storage: &SqliteStorage, end: i64) -> i64 {
    let mut event = MemoryEvent::new(
        DEFAULT_PERSONA_UID.to_string(),
        "陶艺展".to_string(),
        "周末逛了陶艺展，用户很喜欢".to_string(),
        end - 3_600_000,
        end,
    );
    event.salience = 0.8;
    event.valence = 0.5;
    event.confidence = 0.9;
    event.keywords = Some("陶艺,展览".to_string());
    event.created_at = end;
    storage.save_event(&event).await.expect("写入事件应成功")
}

// =========================================================
// 用例
// =========================================================

/// 空库单轮调度：返回全零摘要且不报错。
#[tokio::test]
async fn empty_db_tick_returns_zero_summary() {
    let (engine, _storage, dir) = engine_with_db("proactive-empty-tick").await;

    let summary = run_tick(&engine, ramaria_core::types::now_ms(), &PickerTopicProvider)
        .await
        .expect("空库单轮应成功");
    assert_eq!(summary, schedule::ProactiveTickSummary::default());

    let _ = std::fs::remove_dir_all(dir);
}

/// 端到端：短间隔循环触发 → 打桩选题 → mock LLM 生成 → 接收端收到；关停干净。
///
/// 说明:
/// - 直接以短间隔参数拉起后台调度任务（不拉 idle/L2/L3），选题器与接收端均为打桩；
/// - 循环级启停（lifecycle 装配口径）由生命周期容器用例覆盖。
#[tokio::test]
async fn loop_triggers_generate_and_deliver() {
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-loop-e2e",
        MockLlm::with_reply(REPLY),
        test_config(),
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let sink = Arc::new(TestSink::ok());
    engine.set_proactive_sink(sink.clone());

    let picker = Arc::new(OncePicker::new(ProactiveDirective {
        persona: DEFAULT_PERSONA_UID.to_string(),
        session_id: None,
        source: "time_node".to_string(),
        topic_key: Some("node-1".to_string()),
        anchor: None,
        angle: None,
        tone: None,
        valence: 0.0,
    }));

    let shutdown = Arc::new(AtomicBool::new(false));
    let handle = spawn(Arc::new(engine), Arc::clone(&shutdown), 1, 1, picker);

    // 轮询等待接收端收到 1 条（上限 5 秒）
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.received().is_empty() {
        assert!(
            Instant::now() < deadline,
            "后台循环应在限时内完成生成与投递"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 投递负载：内容 / 人格 / 来源与打桩指令一致
    let received = sink.received();
    assert_eq!(received.len(), 1, "接收端应恰收到一条");
    let message = &received[0];
    assert_eq!(message.content, REPLY);
    assert_eq!(message.persona, DEFAULT_PERSONA_UID);
    assert_eq!(message.source, "time_node");

    // 落库：落点会话仅 1 条 `is_proactive=true` 的 assistant 消息
    let messages = storage
        .list_messages(message.session_id)
        .await
        .expect("读取消息应成功");
    assert_eq!(messages.len(), 1, "新建会话应仅落库 1 条");
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(messages[0].is_proactive, "应为主动消息");

    // 状态记账：投递时间在轮次尾部统一回写，轮询等待落盘完成
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = load_state(&*storage, DEFAULT_PERSONA_UID)
            .await
            .expect("读取状态应成功");
        if state.last_sent_at.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "投递后应在限时内完成状态记账");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 关停：停止位置位后任务在超时内干净退出
    shutdown.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("调度任务应随停止位退出")
        .expect("任务不应 panic");

    let _ = std::fs::remove_dir_all(dir);
}

/// 端到端：真选题器从高显著事件产出候选 → 生成 → 投递；状态记账与关停同打桩路径。
///
/// 说明:
/// - 事件无会话映射：未了结与时间节点源需映射、无规则不命中规则源，
///   仅高显著源可出口且落问候类新建会话；
/// - 判据关闭（配置基线）：决策走算法直取，生成侧恰好一次 LLM 调用。
#[tokio::test]
async fn loop_with_real_picker_delivers_event_topic() {
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-loop-real-picker",
        MockLlm::with_reply(REPLY),
        test_config(),
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let now = now_ms();
    seed_salient_event(&storage, now - 3_600_000).await;

    let sink = Arc::new(TestSink::ok());
    engine.set_proactive_sink(sink.clone());

    let shutdown = Arc::new(AtomicBool::new(false));
    let picker: Arc<dyn TopicPicker> = Arc::new(PickerTopicProvider);
    let handle = spawn(Arc::new(engine), Arc::clone(&shutdown), 1, 1, picker);

    // 轮询等待接收端收到 1 条（上限 5 秒）
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.received().is_empty() {
        assert!(
            Instant::now() < deadline,
            "真选题器循环应在限时内完成生成与投递"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 投递负载：内容 / 来源与事件候选一致
    let received = sink.received();
    assert_eq!(received.len(), 1, "接收端应恰收到一条");
    let message = &received[0];
    assert_eq!(message.content, REPLY);
    assert_eq!(message.persona, DEFAULT_PERSONA_UID);
    assert_eq!(
        message.source, "event",
        "无会话映射的高显著事件应从高显著源出口"
    );

    // 落库：问候类新建会话仅 1 条 `is_proactive=true` 的 assistant 消息
    let messages = storage
        .list_messages(message.session_id)
        .await
        .expect("读取消息应成功");
    assert_eq!(messages.len(), 1, "新建会话应仅落库 1 条");
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(messages[0].is_proactive, "应为主动消息");

    // 状态记账：投递时间 / 效价符号 / 近期选题在轮次尾部同次落盘
    let deadline = Instant::now() + Duration::from_secs(5);
    let state = loop {
        let state = load_state(&*storage, DEFAULT_PERSONA_UID)
            .await
            .expect("读取状态应成功");
        if state.last_sent_at.is_some() {
            break state;
        }
        assert!(Instant::now() < deadline, "投递后应在限时内完成状态记账");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(state.last_valence_sign, 1, "正效价事件应记正向符号");
    assert!(
        state
            .recent_topics
            .iter()
            .any(|topic| topic.source == "event"),
        "近期选题应含高显著事件来源的记录"
    );

    // 关停：停止位置位后任务在超时内干净退出
    shutdown.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("调度任务应随停止位退出")
        .expect("任务不应 panic");

    let _ = std::fs::remove_dir_all(dir);
}

/// 端到端：判据开启时先由判据裁决、再由生成消费脚本回复；开口计数随状态落盘。
///
/// 说明:
/// - 脚本化 LLM 按调用序排队：第一条为判据 JSON、第二条为生成文本；
///   判据被跳过时生成会取到判据 JSON——内容断言即该路径的保险丝；
/// - 事件与装配同真选题器用例，仅判据开关置开。
#[tokio::test]
async fn loop_with_judge_uses_scripted_llm() {
    /// 判据开口裁决（编号 c0 回引排序第一名）。
    const JUDGE_SPEAK_JSON: &str = r#"{"speak": true, "candidate_id": "c0", "angle": "问候近况", "tone": "温暖", "reason_bucket": "speak"}"#;
    /// 判据通过后的生成文本。
    const GENERATED_REPLY: &str = "最近怎么样？";

    let llm = Arc::new(ScriptedLlm::replies(&[JUDGE_SPEAK_JSON, GENERATED_REPLY]));
    let mut config = test_config();
    config.proactive.judge_enabled = true;
    let (engine, storage, dir) =
        engine_with_shared_scripted_llm("proactive-loop-judge", Arc::clone(&llm), config, None)
            .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let now = now_ms();
    seed_salient_event(&storage, now - 3_600_000).await;

    let sink = Arc::new(TestSink::ok());
    engine.set_proactive_sink(sink.clone());

    let shutdown = Arc::new(AtomicBool::new(false));
    let picker: Arc<dyn TopicPicker> = Arc::new(PickerTopicProvider);
    let handle = spawn(Arc::new(engine), Arc::clone(&shutdown), 1, 1, picker);

    // 轮询等待接收端收到 1 条（上限 5 秒）
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.received().is_empty() {
        assert!(
            Instant::now() < deadline,
            "判据循环应在限时内完成裁决、生成与投递"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 投递负载：内容应为脚本第二条（判据 JSON 不应成为投递内容）
    let received = sink.received();
    assert_eq!(received.len(), 1, "接收端应恰收到一条");
    let message = &received[0];
    assert_eq!(
        message.content, GENERATED_REPLY,
        "判据开启时生成应消费脚本第二条；取到判据 JSON 说明判据调用被跳过"
    );
    assert_eq!(message.source, "event");
    assert_eq!(llm.call_count(), 2, "应恰为判据与生成各一次调用");

    // 落库：问候类新建会话仅 1 条 `is_proactive=true` 的 assistant 消息
    let messages = storage
        .list_messages(message.session_id)
        .await
        .expect("读取消息应成功");
    assert_eq!(messages.len(), 1, "新建会话应仅落库 1 条");
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(messages[0].is_proactive, "应为主动消息");

    // 状态记账：判据开口计数与投递时间在轮次尾部同次落盘
    let deadline = Instant::now() + Duration::from_secs(5);
    let state = loop {
        let state = load_state(&*storage, DEFAULT_PERSONA_UID)
            .await
            .expect("读取状态应成功");
        if state.last_sent_at.is_some() {
            break state;
        }
        assert!(Instant::now() < deadline, "投递后应在限时内完成状态记账");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(state.judge_yes_count, 1, "判据开口应计一次");
    assert_eq!(state.judge_no_count, 0, "判据未裁决沉默");

    // 关停：停止位置位后任务在超时内干净退出
    shutdown.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("调度任务应随停止位退出")
        .expect("任务不应 panic");

    let _ = std::fs::remove_dir_all(dir);
}
