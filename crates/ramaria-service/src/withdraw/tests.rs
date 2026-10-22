//! crates/ramaria-service/src/withdraw/tests.rs - Ramaria 撤回域用例测试
//!
//! 设计特点:
//! - 真实 SQLite 临时库 + 打桩接收端 + 脚本化 / mock LLM，不依赖网络与真实时钟
//! - 覆盖通道撤回处理链（标记 / 忽略 / 幂等重入 / 触发资格 / 窗口 / 在途去重 /
//!   闸门否决 / 判据两分支 / 连撤合并 / 状态记账）与本地撤回校验
//! - 撤回文本两态渲染由纯函数直接断言

use super::*;
use crate::engine::Engine;
use crate::proactive::state::{load_global_state, load_state};
use crate::proactive::{ProactiveMessage, ProactiveSink};
use crate::test_support::{
    ScriptedLlm, engine_with_db, engine_with_shared_scripted_llm, seed_persona,
};
use crate::types::DEFAULT_PERSONA_UID;
use crate::types::{RecallNoticeRequest, RecallOutcome};
use async_trait::async_trait;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{AppState, Message, MessageRole, MessageSource, now_ms};
use ramaria_storage::SqliteStorage;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

// =========================================================
// 常量与夹具
// =========================================================

/// 判据开口裁决（编号 c0 回引）。
const JUDGE_SPEAK_JSON: &str = r#"{"speak": true, "candidate_id": "c0", "angle": "关切", "tone": "自然", "reason_bucket": "speak"}"#;

/// 判据沉默裁决。
const JUDGE_SILENT_JSON: &str = r#"{"speak": false}"#;

/// 判据通过后的生成文本。
const GENERATED_REPLY: &str = "看到你撤回了一条消息，还好吗？";

/// 放行基线配置：打扰控制放行 + 通道主动投递开启。
fn recall_config() -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.proactive.enabled = true;
    config.proactive.cooldown_hours = 0;
    config.proactive.daily_limit = 3;
    config.proactive.daily_total_limit = 0;
    config.proactive.quiet_hours = String::new();
    config.channel.proactive_delivery_enabled = true;
    config
}

/// 投递接收端桩：留档收到的通道消息。
struct RecordingSink {
    received: Mutex<Vec<ProactiveMessage>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
        }
    }

    fn received(&self) -> Vec<ProactiveMessage> {
        self.received.lock().expect("接收端留档锁不应中毒").clone()
    }
}

#[async_trait]
impl ProactiveSink for RecordingSink {
    async fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()> {
        self.received
            .lock()
            .expect("接收端留档锁不应中毒")
            .push(message.clone());
        Ok(())
    }
}

/// 阻塞式投递接收端：进入投递后发信号并等待放行（连撤在途去重用例）。
struct BlockingSink {
    received: Mutex<Vec<ProactiveMessage>>,
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl BlockingSink {
    fn new(
        started: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Self {
        Self {
            received: Mutex::new(Vec::new()),
            started: Mutex::new(Some(started)),
            release: tokio::sync::Mutex::new(Some(release)),
        }
    }

    fn received(&self) -> Vec<ProactiveMessage> {
        self.received.lock().expect("接收端留档锁不应中毒").clone()
    }
}

#[async_trait]
impl ProactiveSink for BlockingSink {
    async fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()> {
        self.received
            .lock()
            .expect("接收端留档锁不应中毒")
            .push(message.clone());
        if let Some(tx) = self.started.lock().expect("信号锁不应中毒").take() {
            let _ = tx.send(());
        }
        let release = self.release.lock().await.take();
        if let Some(rx) = release {
            let _ = rx.await;
        }
        Ok(())
    }
}

/// 造绑定 + 通道会话 + 一条可撤回的入站消息。
///
/// 返回:
/// - `(会话 id, 消息 id)`（消息带平台消息 ID，时间取 `now - 1000`）。
async fn seed_bound_channel(
    engine: &Engine,
    storage: &SqliteStorage,
    now: i64,
    platform_message_id: &str,
) -> (Uuid, Uuid) {
    engine
        .channel_set_binding(
            "qq",
            "c2c:10001",
            Some(DEFAULT_PERSONA_UID),
            Some(true),
            Some(true),
        )
        .await
        .expect("通道绑定应写入成功");
    let session = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:10001"))
        .await
        .expect("建通道会话应成功");
    let mut message = Message::new(
        session.id,
        MessageRole::User,
        "刚才那条".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
    message.platform_message_id = Some(platform_message_id.to_string());
    message.created_at = now - 1000;
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
    (session.id, message.id)
}

/// 构造对方撤回通知（operator 为对方账号，self_id 为本机账号）。
fn notice_for(platform_message_id: &str, occurred_at: i64) -> RecallNoticeRequest {
    RecallNoticeRequest {
        channel: "qq".to_string(),
        external_ref: "c2c:10001".to_string(),
        platform_message_id: platform_message_id.to_string(),
        operator_id: Some("10001".to_string()),
        self_id: Some("99999".to_string()),
        occurred_at,
    }
}

/// 构造恰好覆盖当前时刻所在分钟的免打扰窗口。
fn quiet_hours_covering(now: i64) -> String {
    let minute = crate::proactive::state::local_minute_of_day(now) % 1440;
    let start = minute;
    let end = (minute + 1) % 1440;
    format!(
        "{:02}:{:02}-{:02}:{:02}",
        start / 60,
        start % 60,
        end / 60,
        end % 60
    )
}

/// 触发结果断言辅助：取出 (triggered, reason)。
fn marked_reason(outcome: &RecallOutcome) -> (bool, Option<String>) {
    match outcome {
        RecallOutcome::Marked { triggered, reason } => (*triggered, reason.clone()),
        other => panic!("应为 Marked 结果: {other:?}"),
    }
}

// =========================================================
// 通道撤回处理链
// =========================================================

/// 无绑定 → 忽略（binding_missing）。
#[tokio::test]
async fn notice_ignored_when_binding_missing() {
    let (engine, storage, dir) = engine_with_db("recall-binding-missing").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;

    let now = now_ms();
    let outcome = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("处理应成功");
    assert_eq!(
        outcome,
        RecallOutcome::Ignored {
            reason: "binding_missing".to_string()
        }
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 绑定存在但会话 / 消息缺失 → 对应忽略原因。
#[tokio::test]
async fn notice_ignored_when_session_or_message_missing() {
    let (engine, storage, dir) = engine_with_db("recall-locate-missing").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine
        .channel_set_binding(
            "qq",
            "c2c:10001",
            Some(DEFAULT_PERSONA_UID),
            Some(true),
            Some(true),
        )
        .await
        .expect("通道绑定应写入成功");
    engine.set_state(AppState::Ready);

    let now = now_ms();
    // 无会话
    let outcome = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("处理应成功");
    assert_eq!(
        outcome,
        RecallOutcome::Ignored {
            reason: "session_missing".to_string()
        }
    );

    // 有会话但消息不存在
    storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:10001"))
        .await
        .expect("建通道会话应成功");
    let outcome = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("处理应成功");
    assert_eq!(
        outcome,
        RecallOutcome::Ignored {
            reason: "message_missing".to_string()
        }
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 全链路：判据开口 → 生成 → 通道投递 → 状态记账。
#[tokio::test]
async fn notice_triggers_reaction_when_judge_speaks() {
    let llm = Arc::new(ScriptedLlm::replies(&[JUDGE_SPEAK_JSON, GENERATED_REPLY]));
    let mut config = recall_config();
    config.proactive.daily_total_limit = 5;
    let (engine, storage, dir) =
        engine_with_shared_scripted_llm("recall-speak", Arc::clone(&llm), config, None).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let sink = Arc::new(RecordingSink::new());
    engine.set_channel_proactive_sink(sink.clone());

    let now = now_ms();
    let (session_id, message_id) = seed_bound_channel(&engine, &storage, now, "pm-1").await;

    let outcome = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("处理应成功");
    assert_eq!(marked_reason(&outcome), (true, None));

    // 标记已落库
    let recalled = storage
        .find_message_by_platform_id(session_id, "pm-1")
        .await
        .expect("查询消息应成功")
        .expect("消息应存在");
    assert!(recalled.recalled_at.is_some(), "撤回标记应已落库");

    // 生成落库 + 通道投递
    let received = sink.received();
    assert_eq!(received.len(), 1, "通道接收端应恰收到一条");
    assert_eq!(received[0].content, GENERATED_REPLY);
    assert_eq!(received[0].session_id, session_id);
    let messages = storage
        .list_messages(session_id)
        .await
        .expect("查询消息应成功");
    let assistant = messages
        .iter()
        .find(|message| message.role == MessageRole::Assistant)
        .expect("应新增助手行");
    assert!(assistant.is_proactive, "应为主动消息");
    assert_eq!(
        assistant.id, received[0].message_id,
        "投递消息 id 应与落库一致"
    );
    assert_ne!(assistant.id, message_id, "原消息不应被改写");

    // 状态记账：判据 yes / 投递时间 / 当日计数；判据节流豁免（last_judge_at 不写）
    let state = load_state(&*storage, DEFAULT_PERSONA_UID)
        .await
        .expect("读取状态应成功");
    assert_eq!(state.judge_yes_count, 1, "判据开口应计一次");
    assert_eq!(state.judge_no_count, 0);
    assert_eq!(state.last_sent_at, Some(now), "投递时间应回写");
    assert_eq!(state.daily_count, 1, "当日计数应加一");
    assert_eq!(state.last_judge_at, None, "即时事件豁免判据节流");
    let global = load_global_state(&*storage)
        .await
        .expect("读取全局状态应成功");
    assert_eq!(global.daily_count, 1, "全局日计数应加一");

    let _ = std::fs::remove_dir_all(dir);
}

/// 我方消息（助手行）标记但不触发回应。
#[tokio::test]
async fn notice_marks_assistant_message_without_trigger() {
    let (engine, storage, dir) = engine_with_db("recall-own-message").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let sink = Arc::new(RecordingSink::new());
    engine.set_channel_proactive_sink(sink.clone());

    let now = now_ms();
    let (session_id, _) = seed_bound_channel(&engine, &storage, now, "pm-1").await;
    let mut assistant = Message::new(
        session_id,
        MessageRole::Assistant,
        "我发的一条".to_string(),
        MessageSource::Online,
    )
    .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
    assistant.platform_message_id = Some("pm-self".to_string());
    assistant.created_at = now - 500;
    storage
        .save_message(&assistant)
        .await
        .expect("写入助手消息应成功");

    let outcome = handle_notice(&engine, notice_for("pm-self", now), now)
        .await
        .expect("处理应成功");
    assert_eq!(
        marked_reason(&outcome),
        (false, Some("own_message".to_string()))
    );
    // 助手行标记已落库（以平台 ID 定位）
    let recalled = storage
        .find_message_by_platform_id(session_id, "pm-self")
        .await
        .expect("查询消息应成功")
        .expect("消息应存在");
    assert!(recalled.recalled_at.is_some(), "撤回标记应已落库");
    assert!(sink.received().is_empty(), "不应产生投递");

    let _ = std::fs::remove_dir_all(dir);
}

/// 撤回者为本机（self_id == operator_id）标记但不触发。
#[tokio::test]
async fn notice_marks_self_recall_without_trigger() {
    let (engine, storage, dir) = engine_with_db("recall-self-op").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let now = now_ms();
    let (_, _) = seed_bound_channel(&engine, &storage, now, "pm-1").await;
    let mut notice = notice_for("pm-1", now);
    notice.operator_id = Some("99999".to_string());

    let outcome = handle_notice(&engine, notice, now)
        .await
        .expect("处理应成功");
    assert_eq!(
        marked_reason(&outcome),
        (false, Some("own_message".to_string()))
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 超出反应窗口标记但不触发。
#[tokio::test]
async fn notice_skips_when_window_expired() {
    let (engine, storage, dir) = engine_with_db("recall-window").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let now = now_ms();
    seed_bound_channel(&engine, &storage, now, "pm-1").await;
    let outcome = handle_notice(&engine, notice_for("pm-1", now - 121_000), now)
        .await
        .expect("处理应成功");
    assert_eq!(
        marked_reason(&outcome),
        (false, Some("window_expired".to_string()))
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 幂等重入：同一通知二次处理 → AlreadyMarked，且不产生第二次生成 / 投递。
#[tokio::test]
async fn notice_repeat_returns_already_marked() {
    let llm = Arc::new(ScriptedLlm::replies(&[JUDGE_SPEAK_JSON, GENERATED_REPLY]));
    let (engine, storage, dir) =
        engine_with_shared_scripted_llm("recall-repeat", Arc::clone(&llm), recall_config(), None)
            .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let sink = Arc::new(RecordingSink::new());
    engine.set_channel_proactive_sink(sink.clone());

    let now = now_ms();
    seed_bound_channel(&engine, &storage, now, "pm-1").await;

    let first = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("首次处理应成功");
    assert_eq!(marked_reason(&first), (true, None));

    let second = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("重复处理应成功");
    assert_eq!(second, RecallOutcome::AlreadyMarked);
    assert_eq!(sink.received().len(), 1, "重复通知不应产生第二次投递");
    assert_eq!(llm.call_count(), 2, "重复通知不应追加 LLM 调用");

    let _ = std::fs::remove_dir_all(dir);
}

/// 连撤合并：同会话两条消息连撤，首条在途时第二条仅标记不触发。
#[tokio::test]
async fn notice_dedups_second_recall_while_first_inflight() {
    let llm = Arc::new(ScriptedLlm::replies(&[JUDGE_SPEAK_JSON, GENERATED_REPLY]));
    let (engine, storage, dir) =
        engine_with_shared_scripted_llm("recall-inflight", Arc::clone(&llm), recall_config(), None)
            .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let now = now_ms();
    let (session_id, _) = seed_bound_channel(&engine, &storage, now, "pm-1").await;
    // 第二条待撤消息（同会话）
    let mut second_message = Message::new(
        session_id,
        MessageRole::User,
        "另一条".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
    second_message.platform_message_id = Some("pm-2".to_string());
    second_message.created_at = now - 900;
    storage
        .save_message(&second_message)
        .await
        .expect("写入第二条消息应成功");

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let sink = Arc::new(BlockingSink::new(started_tx, release_rx));
    engine.set_channel_proactive_sink(sink.clone());

    let engine = Arc::new(engine);
    let first_engine = Arc::clone(&engine);
    let first =
        tokio::spawn(async move { first_engine.channel_recall(notice_for("pm-1", now)).await });
    started_rx.await.expect("首条应进入投递等待");

    // 首条阻塞在投递时：第二条同会话撤回 → 在途去重
    let second = engine
        .channel_recall(notice_for("pm-2", now))
        .await
        .expect("第二条处理应成功");
    assert_eq!(
        marked_reason(&second),
        (false, Some("inflight_dedup".to_string()))
    );
    // 第二条已标记（不触发但标记照落）
    let recalled = storage
        .find_message_by_platform_id(session_id, "pm-2")
        .await
        .expect("查询消息应成功")
        .expect("消息应存在");
    assert!(recalled.recalled_at.is_some());

    // 放行首条并断言触发完成
    let _ = release_tx.send(());
    let first_outcome = first
        .await
        .expect("首条任务不应 panic")
        .expect("首条处理应成功");
    assert_eq!(marked_reason(&first_outcome), (true, None));
    assert_eq!(sink.received().len(), 1, "仅首条产生投递");

    let _ = std::fs::remove_dir_all(dir);
}

/// 闸门否决：免打扰覆盖当前时刻 → 不生成（LLM 零调用）。
#[tokio::test]
async fn notice_skips_when_quiet_hours() {
    let llm = Arc::new(ScriptedLlm::replies(&[JUDGE_SPEAK_JSON, GENERATED_REPLY]));
    let now = now_ms();
    let mut config = recall_config();
    config.proactive.quiet_hours = quiet_hours_covering(now);
    let (engine, storage, dir) =
        engine_with_shared_scripted_llm("recall-quiet", Arc::clone(&llm), config, None).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    seed_bound_channel(&engine, &storage, now, "pm-1").await;

    let outcome = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("处理应成功");
    assert_eq!(
        marked_reason(&outcome),
        (false, Some("quiet_hours".to_string()))
    );
    assert_eq!(llm.call_count(), 0, "闸门否决不应发起 LLM 调用");

    let _ = std::fs::remove_dir_all(dir);
}

/// 判据沉默：不生成不投递，仅计 no。
#[tokio::test]
async fn notice_skips_reaction_when_judge_silent() {
    let llm = Arc::new(ScriptedLlm::replies(&[JUDGE_SILENT_JSON]));
    let (engine, storage, dir) =
        engine_with_shared_scripted_llm("recall-silent", Arc::clone(&llm), recall_config(), None)
            .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    let sink = Arc::new(RecordingSink::new());
    engine.set_channel_proactive_sink(sink.clone());

    let now = now_ms();
    seed_bound_channel(&engine, &storage, now, "pm-1").await;
    let outcome = handle_notice(&engine, notice_for("pm-1", now), now)
        .await
        .expect("处理应成功");
    assert_eq!(
        marked_reason(&outcome),
        (false, Some("judge_silent".to_string()))
    );
    assert!(sink.received().is_empty(), "判据沉默不应投递");

    let state = load_state(&*storage, DEFAULT_PERSONA_UID)
        .await
        .expect("读取状态应成功");
    assert_eq!(state.judge_no_count, 1, "沉默应计 no");
    assert_eq!(state.judge_yes_count, 0);
    assert_eq!(state.last_sent_at, None, "未投递不应记账投递时间");

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 本地撤回校验
// =========================================================

/// 本地撤回：正常路径标记落库；重复 / 角色 / 窗口 / 社交通道 / 不存在逐项拒绝。
#[tokio::test]
async fn local_recall_validates_and_marks() {
    let (engine, storage, dir) = engine_with_db("recall-local").await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let now = now_ms();

    let session = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("建会话应成功");
    let mut mine = Message::new(
        session.id,
        MessageRole::User,
        "我自己发的".to_string(),
        MessageSource::Local,
    )
    .with_persona_uid(Some(DEFAULT_PERSONA_UID.to_string()));
    mine.created_at = now - 1000;
    storage.save_message(&mine).await.expect("写入消息应成功");

    recall_local_message(&engine, mine.id, now)
        .await
        .expect("本地撤回应成功");
    let recalled = storage
        .get_message(mine.id)
        .await
        .expect("查询应成功")
        .expect("消息应存在");
    assert!(recalled.recalled_at.is_some(), "撤回标记应落库");

    let err = recall_local_message(&engine, mine.id, now)
        .await
        .expect_err("重复撤回应报错");
    assert!(err.to_string().contains("消息已撤回"), "{err}");

    let assistant = Message::new(
        session.id,
        MessageRole::Assistant,
        "回复".to_string(),
        MessageSource::Local,
    );
    storage
        .save_message(&assistant)
        .await
        .expect("写入消息应成功");
    let err = recall_local_message(&engine, assistant.id, now)
        .await
        .expect_err("助手消息应拒绝");
    assert!(err.to_string().contains("只能撤回自己发送的消息"), "{err}");

    let mut old = Message::new(
        session.id,
        MessageRole::User,
        "很旧的一条".to_string(),
        MessageSource::Local,
    );
    old.created_at = now - 121_000;
    storage.save_message(&old).await.expect("写入消息应成功");
    let err = recall_local_message(&engine, old.id, now)
        .await
        .expect_err("窗口过期应拒绝");
    assert!(err.to_string().contains("2 分钟"), "{err}");

    let err = recall_local_message(&engine, Uuid::new_v4(), now)
        .await
        .expect_err("消息不存在应拒绝");
    assert!(err.to_string().contains("消息不存在"), "{err}");

    let social = storage
        .create_session_in_channel(Some(DEFAULT_PERSONA_UID), "qq", Some("c2c:1"))
        .await
        .expect("建通道会话应成功");
    let mut in_social = Message::new(
        social.id,
        MessageRole::User,
        "通道消息".to_string(),
        MessageSource::Local,
    );
    in_social.created_at = now - 1000;
    storage
        .save_message(&in_social)
        .await
        .expect("写入消息应成功");
    let err = recall_local_message(&engine, in_social.id, now)
        .await
        .expect_err("社交通道应拒绝");
    assert!(err.to_string().contains("社交平台"), "{err}");

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 渲染纯函数
// =========================================================

/// 撤回文本两态：未消费仅信号；已消费原文 + 信号。
#[test]
fn render_recalled_content_two_states() {
    assert_eq!(render_recalled_content("原内容", false), RECALL_SIGNAL);
    assert_eq!(
        render_recalled_content("原内容", true),
        format!("原内容{RECALL_SIGNAL}")
    );
}
