//! crates/ramaria-service/src/proactive/schedule/tests.rs - 主动调度与打扰控制单元测试
//!
//! 设计特点:
//! - 覆盖判定链各闸门（可见性 / 状态 / 宽限 / 免打扰 / 上限 / 冷却 / 退避 / 空闲 / 隐私 /
//!   活跃时段 / 判据节流）与投放三态（成功 / 未注册 / 失败）
//! - 用固定时间戳驱动单轮调度，装配真实 SQLite 临时库与 mock LLM，不依赖网络与真实时钟
//! - 断言口径：闸门用"选题器是否被触达"、状态变更用"库中 reload"、投放用接收端留档

use super::quiet::{QuietHours, parse_quiet_hours};
use super::*;
use crate::proactive::sink::ProactiveSink;
use crate::proactive::switch::{self, ProactivePersonaMode};
use crate::recall::RecallPolicy;
use crate::test_support::seed_session_with_messages;
use crate::test_support::{
    MockLlm, engine_with_llm_and_config, engine_with_shared_llm, seed_dialogue_history,
    seed_persona, seed_persona_kind,
};
use crate::types::DEFAULT_PERSONA_UID;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, BackendConfig, MessageRole, PersonaKind, PrivacyConsent};
use ramaria_storage::SqliteStorage;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// 固定回复（供"生成成功"路径断言）。
const REPLY: &str = "刚路过一家花店，想起你说想学插花。";

// =========================================================
// 共用工具
// =========================================================

/// 放行基线配置：主动开关开启、打扰控制闸门全部放行（各用例按需覆盖单项）。
fn test_config() -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.proactive.enabled = true;
    config.proactive.min_idle_hours = 0;
    config.proactive.cooldown_hours = 0;
    config.proactive.daily_limit = 3;
    config.proactive.quiet_hours = String::new();
    config.proactive.startup_grace_days = 0;
    config.proactive.judge_enabled = false;
    config.proactive.check_interval_seconds = 300;
    config
}

/// 装配"真实 SQLite + 空回复 mock LLM + 指定主动配置"的引擎，并造 persona、
/// 对话历史与就绪态（历史满足资格闸门的解锁判定）。
async fn ready_engine(
    tag: &str,
    config: RamariaConfig,
) -> (Engine, Arc<SqliteStorage>, std::path::PathBuf) {
    let (engine, storage, dir) = engine_with_llm_and_config(tag, MockLlm::local(), config).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);
    (engine, storage, dir)
}

/// 预置指定人格的运行时状态。
async fn save_initial_state(storage: &SqliteStorage, state: &ProactiveState) {
    state::save_state(storage, DEFAULT_PERSONA_UID, state)
        .await
        .expect("预置状态应成功");
}

/// 读取指定人格的运行时状态。
async fn reload_state(storage: &SqliteStorage) -> ProactiveState {
    state::load_state(storage, DEFAULT_PERSONA_UID)
        .await
        .expect("读取状态应成功")
}

/// 构造主动指令（固定来源 `event`、带选题键）。
fn directive(session_id: Option<Uuid>) -> ProactiveDirective {
    ProactiveDirective {
        persona: DEFAULT_PERSONA_UID.to_string(),
        session_id,
        source: "event".to_string(),
        topic_key: Some("evt-1".to_string()),
        anchor: None,
        angle: None,
        tone: None,
        valence: 0.0,
    }
}

/// 计数选题器：记录调用次数并按需返回候选指令。
struct CountingPicker {
    calls: AtomicUsize,
    directive: Mutex<Option<ProactiveDirective>>,
}

impl CountingPicker {
    /// 不产出候选（仅计数）。
    fn empty() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            directive: Mutex::new(None),
        }
    }

    /// 恒定产出指定候选。
    fn returning(directive: ProactiveDirective) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            directive: Mutex::new(Some(directive)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

#[async_trait]
impl TopicPicker for CountingPicker {
    async fn pick(
        &self,
        _engine: &Engine,
        _persona: &str,
        _now: i64,
        _state: &mut ProactiveState,
        _activity_weight: f64,
    ) -> Option<ProactiveDirective> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.directive.lock().expect("选题器锁不应中毒").clone()
    }
}

/// 投递接收端桩：留档负载并可注入失败。
struct TestSink {
    received: Mutex<Vec<ProactiveMessage>>,
    fail: bool,
}

impl TestSink {
    /// 投递恒成功。
    fn ok() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    /// 投递恒失败。
    fn failing() -> Self {
        Self {
            received: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    fn received(&self) -> Vec<ProactiveMessage> {
        self.received.lock().expect("接收端留档锁不应中毒").clone()
    }
}

impl ProactiveSink for TestSink {
    fn deliver(&self, message: &ProactiveMessage) -> RamariaResult<()> {
        if self.fail {
            return Err(ramaria_core::error::RamariaError::storage("模拟投递失败"));
        }
        self.received
            .lock()
            .expect("接收端留档锁不应中毒")
            .push(message.clone());
        Ok(())
    }
}

/// 构造从 `start` 起长 `length` 分钟的窗口文本（支持跨零点）。
fn window_from_minute(start: u32, length: u32) -> String {
    let start = start % 1440;
    let end = (start + length) % 1440;
    format!(
        "{:02}:{:02}-{:02}:{:02}",
        start / 60,
        start % 60,
        end / 60,
        end % 60
    )
}

// =========================================================
// 免打扰时段解析与判定
// =========================================================

/// 解析：正常 / 跨零点 / 等值 / 非法 / 空白 / 越界。
#[test]
fn parse_quiet_hours_variants() {
    assert_eq!(
        parse_quiet_hours("22:00-08:00"),
        Some(QuietHours {
            start_minute: 22 * 60,
            end_minute: 8 * 60,
        })
    );
    assert_eq!(
        parse_quiet_hours("23:30-07:00"),
        Some(QuietHours {
            start_minute: 23 * 60 + 30,
            end_minute: 7 * 60,
        })
    );
    assert_eq!(
        parse_quiet_hours("00:00-23:59"),
        Some(QuietHours {
            start_minute: 0,
            end_minute: 23 * 60 + 59,
        })
    );
    assert_eq!(parse_quiet_hours("10:00-10:00"), None, "等值窗口视为未配置");
    assert_eq!(parse_quiet_hours(""), None, "空串视为未配置");
    assert_eq!(parse_quiet_hours("   "), None, "空白视为未配置");
    assert_eq!(parse_quiet_hours("abc"), None, "非法文本");
    assert_eq!(parse_quiet_hours("10:00"), None, "缺终点");
    assert_eq!(parse_quiet_hours("10:00-"), None, "终点为空");
    assert_eq!(parse_quiet_hours("25:00-26:00"), None, "小时越界");
    assert_eq!(parse_quiet_hours("10:60-11:00"), None, "分钟越界");
}

/// 跨零点窗口的边界口径：起点含、终点不含、跨零点区间连续。
#[test]
fn quiet_hours_contains_cross_midnight() {
    let window = parse_quiet_hours("23:30-07:00").expect("跨零点窗口应可解析");
    assert!(!window.contains(23 * 60 + 29), "起点前一分钟应在窗口外");
    assert!(window.contains(23 * 60 + 30), "起点应含");
    assert!(window.contains(0), "零点应含");
    assert!(window.contains(6 * 60 + 59), "终点前一分钟应含");
    assert!(!window.contains(7 * 60), "终点应不含");
    assert!(!window.contains(12 * 60), "白天应在窗口外");
}

// =========================================================
// 硬闸门
// =========================================================

/// 人格不可见：跳过且不触达选题。
#[tokio::test]
async fn gate_persona_not_allowed() {
    let (engine, _storage, dir) = ready_engine("proactive-gate-persona", test_config()).await;
    engine.set_recall_policy(
        RecallPolicy::default().with_allowed_personas(vec!["other".to_string()]),
    );

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 0);
    assert_eq!(picker.calls(), 0, "不可见人格不应触达选题");

    let _ = std::fs::remove_dir_all(dir);
}

/// 应用状态未就绪：跳过且不触达选题。
#[tokio::test]
async fn gate_state_not_ready() {
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-gate-state", MockLlm::local(), test_config()).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    // 补足历史让资格闸门放行，保留对"状态门"的覆盖
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    // 不设 Ready：状态门未通过

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 0);
    assert_eq!(picker.calls(), 0, "未就绪不应触达选题");

    let _ = std::fs::remove_dir_all(dir);
}

/// 宽限期：首见基准惰性写入；宽限内跳过，届满放行。
#[tokio::test]
async fn gate_startup_grace_and_expiry() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.startup_grace_days = 3;

    // 宽限期内（首见 1 天前）→ 跳过
    let (engine, storage, dir) = ready_engine("proactive-gate-grace-in", config.clone()).await;
    let st = ProactiveState {
        first_seen_at: Some(now - 86_400_000),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "宽限期内不应触达选题");
    let _ = std::fs::remove_dir_all(dir);

    // 宽限期届满（首见 4 天前）→ 放行
    let (engine, storage, dir) = ready_engine("proactive-gate-grace-out", config.clone()).await;
    let st = ProactiveState {
        first_seen_at: Some(now - 4 * 86_400_000),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "宽限期届满应触达选题");
    assert_eq!(summary.attempts, 1);
    let _ = std::fs::remove_dir_all(dir);

    // 首次见到（未预置）→ 写入基准并（宽限期内）跳过
    let (engine, storage, dir) = ready_engine("proactive-gate-grace-first", config).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "首见即起算，宽限期内跳过");
    assert_eq!(
        reload_state(&storage).await.first_seen_at,
        Some(now),
        "首次见到应写入宽限基准"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 免打扰：包含当前本地分钟的窗口跳过；排除当前分钟的窗口放行。
#[tokio::test]
async fn gate_quiet_hours_blocks_current_minute() {
    let now = now_ms();
    let minute = state::local_minute_of_day(now);

    // 包含当前分钟的窗口（自当前分钟起 60 分钟，跨零点安全）→ 跳过
    let mut config = test_config();
    config.proactive.quiet_hours = window_from_minute(minute, 60);
    let (engine, _storage, dir) = ready_engine("proactive-gate-quiet-in", config).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "免打扰时段内不应触达选题");
    let _ = std::fs::remove_dir_all(dir);

    // 排除当前分钟的窗口（两小时后起）→ 放行
    let mut config = test_config();
    config.proactive.quiet_hours = window_from_minute(minute + 120, 60);
    let (engine, _storage, dir) = ready_engine("proactive-gate-quiet-out", config).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "窗口外应触达选题");
    let _ = std::fs::remove_dir_all(dir);
}

/// 每日上限：当日已满跳过；归属日期为昨日时跨日重置后放行。
#[tokio::test]
async fn gate_daily_limit_and_cross_day_reset() {
    let now = now_ms();

    // 当日已满 → 跳过
    let (engine, storage, dir) = ready_engine("proactive-gate-daily-full", test_config()).await;
    let st = ProactiveState {
        daily_count: 3,
        daily_date: state::local_date_str(now),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "当日上限已满不应触达选题");
    let _ = std::fs::remove_dir_all(dir);

    // 归属昨日 → 跨日重置后放行
    let (engine, storage, dir) = ready_engine("proactive-gate-daily-reset", test_config()).await;
    let st = ProactiveState {
        daily_count: 3,
        daily_date: state::local_date_str(now - 86_400_000),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "跨日重置后应触达选题");
    let reloaded = reload_state(&storage).await;
    assert_eq!(reloaded.daily_count, 0, "跨日应清零");
    assert_eq!(reloaded.daily_date, state::local_date_str(now));
    let _ = std::fs::remove_dir_all(dir);
}

/// 冷却：距上次生成不足冷却间隔跳过；超出后放行。
#[tokio::test]
async fn gate_cooldown_blocks() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.cooldown_hours = 8;

    // 距上次生成 1 小时 → 冷却跳过
    let (engine, storage, dir) = ready_engine("proactive-gate-cooldown-in", config.clone()).await;
    let st = ProactiveState {
        last_sent_at: Some(now - 3_600_000),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "冷却期内不应触达选题");
    let _ = std::fs::remove_dir_all(dir);

    // 距上次生成 9 小时 → 放行（退避未达阈值）
    let (engine, storage, dir) = ready_engine("proactive-gate-cooldown-out", config).await;
    let st = ProactiveState {
        last_sent_at: Some(now - 9 * 3_600_000),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "冷却期外应触达选题");
    let _ = std::fs::remove_dir_all(dir);
}

/// 沉默退避：未回应且超过退避天数跳过（不改写退避计数）；用户回应则归零放行。
#[tokio::test]
async fn gate_silence_backoff_blocks_and_reply_resets() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.silence_backoff_days = 3;

    let (engine, storage, dir) = ready_engine("proactive-gate-backoff", config).await;
    let last_sent = now - 4 * 86_400_000;
    let st = ProactiveState {
        last_sent_at: Some(last_sent),
        silence_streak: 2,
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;

    // 无用户回应且超过退避天数 → 跳过；streak 原值不被误伤
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "退避期不应触达选题");
    assert_eq!(
        reload_state(&storage).await.silence_streak,
        2,
        "跳过路径不应改写退避计数"
    );

    // 插入晚于上次生成的 user 消息 → 回应检测归零并放行
    seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, last_sent + 60_000).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "用户已回应应触达选题");
    assert_eq!(
        reload_state(&storage).await.silence_streak,
        0,
        "用户已回应应解除退避"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 最小空闲：最近对话在空闲窗口内跳过；无历史视为足够空闲放行。
#[tokio::test]
async fn gate_min_idle_blocks() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.min_idle_hours = 4;

    // 最近对话 1 小时前 → 跳过
    let (engine, storage, dir) = ready_engine("proactive-gate-idle-in", config.clone()).await;
    seed_session_with_messages(&storage, DEFAULT_PERSONA_UID, 2, now - 3_600_000).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "空闲不足不应触达选题");
    let _ = std::fs::remove_dir_all(dir);

    // 无任何消息 → 放行（手动强开绕过解锁门，保留"无历史视为足够空闲"语义）
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-gate-idle-out", MockLlm::local(), config).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    switch::save_mode(
        storage.as_ref(),
        DEFAULT_PERSONA_UID,
        ProactivePersonaMode::On,
    )
    .await
    .expect("保存开关应成功");
    engine.set_state(AppState::Ready);
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "无历史应视为足够空闲");
    let _ = std::fs::remove_dir_all(dir);
}

/// 隐私门禁：线上 provider 未确认跳过；写入确认后放行。
#[tokio::test]
async fn gate_privacy_unconfirmed_online() {
    let llm = Arc::new(MockLlm::online());
    let (engine, storage, dir) = engine_with_shared_llm(
        "proactive-gate-privacy",
        Arc::clone(&llm),
        test_config(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let now = now_ms();
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "线上未确认不应触达选题");

    // 写入当前后端粒度的隐私确认 → 放行
    let backend = BackendConfig::deepseek_default();
    storage
        .save_privacy_consent(&PrivacyConsent::new(
            backend.provider,
            backend.base_url.clone(),
            true,
        ))
        .await
        .expect("写入隐私确认应成功");
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "确认后应触达选题");

    let _ = std::fs::remove_dir_all(dir);
}

/// 判据节流：窗口内跳过且不消费记账；窗口外通过并记账，同刻再调度被节流。
#[tokio::test]
async fn judge_throttle_blocks_then_consumes() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.judge_enabled = true;
    config.proactive.judge_interval_hours = 3;

    // 节流窗口内（1 小时前尝试过）→ 跳过，记账点保持
    let (engine, storage, dir) = ready_engine("proactive-gate-judge-in", config.clone()).await;
    let st = ProactiveState {
        last_judge_at: Some(now - 3_600_000),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "节流窗口内不应触达选题");
    assert_eq!(
        reload_state(&storage).await.last_judge_at,
        Some(now - 3_600_000),
        "节流跳过不应消费窗口"
    );
    let _ = std::fs::remove_dir_all(dir);

    // 节流窗口外（4 小时前尝试过）→ 通过并记账；同刻再次调度 → 被新记账点节流
    let (engine, storage, dir) = ready_engine("proactive-gate-judge-out", config).await;
    let st = ProactiveState {
        last_judge_at: Some(now - 4 * 3_600_000),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "窗口外应触达选题");
    assert_eq!(
        reload_state(&storage).await.last_judge_at,
        Some(now),
        "选题尝试应记账"
    );
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "同刻再次调度应被节流");
    let _ = std::fs::remove_dir_all(dir);
}

/// 活跃时段门：当前小时零计数（低活跃度）跳过；当前小时为峰值放行。
#[tokio::test]
async fn activity_gate_low_weight_skips() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.active_hours_min_samples = 1;
    let today = state::local_date_str(now);
    let hour = state::local_hour(now) as usize;

    // 当前小时零计数、其他小时高计数 → 低活跃度跳过
    let (engine, storage, dir) = ready_engine("proactive-gate-activity-low", config.clone()).await;
    let mut histogram = [100u32; 24];
    histogram[hour] = 0;
    let st = ProactiveState {
        hour_histogram: Some(histogram),
        histogram_date: today.clone(),
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "低活跃时段不应触达选题");
    let _ = std::fs::remove_dir_all(dir);

    // 当前小时为峰值 → 放行
    let (engine, storage, dir) = ready_engine("proactive-gate-activity-peak", config).await;
    let mut histogram = [0u32; 24];
    histogram[hour] = 5;
    let st = ProactiveState {
        hour_histogram: Some(histogram),
        histogram_date: today,
        ..Default::default()
    };
    save_initial_state(&storage, &st).await;
    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "峰值时段应触达选题");
    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 人格主动开关闸门
// =========================================================

/// user 类人格硬排除：手动强开也不放行。
#[tokio::test]
async fn gate_user_persona_excluded() {
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-gate-user", MockLlm::local(), test_config()).await;
    seed_persona_kind(&storage, "user-0001", PersonaKind::User).await;
    seed_dialogue_history(&storage, "user-0001").await;
    switch::save_mode(storage.as_ref(), "user-0001", ProactivePersonaMode::On)
        .await
        .expect("保存开关应成功");
    engine.set_state(AppState::Ready);

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 0);
    assert_eq!(picker.calls(), 0, "user 类人格应硬排除");

    let _ = std::fs::remove_dir_all(dir);
}

/// 手动关闭：有对话历史也跳过。
#[tokio::test]
async fn gate_persona_off_skips() {
    let (engine, storage, dir) = ready_engine("proactive-gate-off", test_config()).await;
    switch::save_mode(
        storage.as_ref(),
        DEFAULT_PERSONA_UID,
        ProactivePersonaMode::Off,
    )
    .await
    .expect("保存开关应成功");

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 0);
    assert_eq!(picker.calls(), 0, "手动关闭不应触达选题");

    let _ = std::fs::remove_dir_all(dir);
}

/// 自动冷：无对话历史跳过（自动态的冷启动解锁判定）。
#[tokio::test]
async fn gate_persona_cold_skips() {
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-gate-cold", MockLlm::local(), test_config()).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 0);
    assert_eq!(picker.calls(), 0, "自动态无历史不应触达选题");

    let _ = std::fs::remove_dir_all(dir);
}

/// 手动强开：无对话历史也放行。
#[tokio::test]
async fn gate_persona_on_forces_pass_when_cold() {
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-gate-on", MockLlm::local(), test_config()).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    switch::save_mode(
        storage.as_ref(),
        DEFAULT_PERSONA_UID,
        ProactivePersonaMode::On,
    )
    .await
    .expect("保存开关应成功");
    engine.set_state(AppState::Ready);

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 1);
    assert_eq!(picker.calls(), 1, "手动强开应放行");

    let _ = std::fs::remove_dir_all(dir);
}

/// 自动暖：有对话历史且未设置开关（自动态）放行。
#[tokio::test]
async fn gate_persona_warm_auto_passes() {
    let (engine, _storage, dir) = ready_engine("proactive-gate-warm", test_config()).await;

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 1);
    assert_eq!(picker.calls(), 1, "自动态有历史应放行");

    let _ = std::fs::remove_dir_all(dir);
}

/// 开关值损坏：回退自动（有历史放行）。
#[tokio::test]
async fn gate_persona_corrupted_switch_falls_back_auto() {
    let (engine, storage, dir) = ready_engine("proactive-gate-corrupt", test_config()).await;
    storage
        .set_setting(
            &format!("proactive.persona.{DEFAULT_PERSONA_UID}"),
            "garbage",
        )
        .await
        .expect("写入损坏开关应成功");

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary.attempts, 1);
    assert_eq!(picker.calls(), 1, "损坏值应回退自动并放行");

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 全局日上限
// =========================================================

/// 预置全局主动状态。
async fn save_global(storage: &SqliteStorage, global: &ProactiveGlobalState) {
    state::save_global_state(storage, global)
        .await
        .expect("预置全局状态应成功");
}

/// 读取全局主动状态。
async fn reload_global(storage: &SqliteStorage) -> ProactiveGlobalState {
    state::load_global_state(storage)
        .await
        .expect("读取全局状态应成功")
}

/// 全局日上限已满：跳过。
#[tokio::test]
async fn gate_daily_total_limit_blocks() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.daily_total_limit = 1;
    let (engine, storage, dir) = ready_engine("proactive-gate-total-full", config).await;
    save_global(
        &storage,
        &ProactiveGlobalState {
            daily_count: 1,
            daily_date: state::local_date_str(now),
        },
    )
    .await;

    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 0, "全局上限已满不应触达选题");

    let _ = std::fs::remove_dir_all(dir);
}

/// 全局上限不限（0）：不读不写全局状态。
#[tokio::test]
async fn gate_daily_total_limit_disabled_zero() {
    let now = now_ms();
    let (engine, storage, dir) = ready_engine("proactive-gate-total-off", test_config()).await;
    let preset = ProactiveGlobalState {
        daily_count: 99,
        daily_date: state::local_date_str(now),
    };
    save_global(&storage, &preset).await;

    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "不限模式不应拦截");
    assert_eq!(
        reload_global(&storage).await,
        preset,
        "不限模式不应读写全局状态"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 跨日重置：归属昨日 → 清零后放行。
#[tokio::test]
async fn gate_daily_total_limit_cross_day_reset() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.daily_total_limit = 1;
    let (engine, storage, dir) = ready_engine("proactive-gate-total-reset", config).await;
    save_global(
        &storage,
        &ProactiveGlobalState {
            daily_count: 5,
            daily_date: state::local_date_str(now - 86_400_000),
        },
    )
    .await;

    let picker = CountingPicker::empty();
    run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(picker.calls(), 1, "跨日重置后应触达选题");
    let reloaded = reload_global(&storage).await;
    assert_eq!(reloaded.daily_count, 0, "空选题器无投递，计数应保持重置值");
    assert_eq!(reloaded.daily_date, state::local_date_str(now));

    let _ = std::fs::remove_dir_all(dir);
}

/// 生成成功计入全局计数（口径与人格级一致）。
#[tokio::test]
async fn daily_total_limit_counts_generated() {
    let now = now_ms();
    let mut config = test_config();
    config.proactive.daily_total_limit = 5;
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-total-counts", MockLlm::with_reply(REPLY), config)
            .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let sink = Arc::new(TestSink::ok());
    engine.set_proactive_sink(sink.clone());
    let picker = CountingPicker::returning(directive(None));
    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(summary.generated, 1);

    let global = reload_global(&storage).await;
    assert_eq!(global.daily_count, 1, "生成成功应计入全局计数");
    assert_eq!(global.daily_date, state::local_date_str(now));

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 生成与投放
// =========================================================

/// 全链路：选题 → 生成落库 → 投递成功 → 状态记账完整。
#[tokio::test]
async fn tick_generates_and_delivers_full_chain() {
    let now = now_ms();
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-tick-full",
        MockLlm::with_reply(REPLY),
        test_config(),
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let sink = Arc::new(TestSink::ok());
    engine.set_proactive_sink(sink.clone());
    let picker = CountingPicker::returning(directive(None));
    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(
        summary,
        ProactiveTickSummary {
            personas: 1,
            attempts: 1,
            generated: 1,
            delivered: 1,
        }
    );

    // 接收端留档：内容 / 人格 / 来源 / 时间与生成结果一致
    let received = sink.received();
    assert_eq!(received.len(), 1, "接收端应收到一条");
    let message = &received[0];
    assert_eq!(message.content, REPLY);
    assert_eq!(message.persona, DEFAULT_PERSONA_UID);
    assert_eq!(message.source, "event");
    assert_eq!(message.created_at, now);

    // 落库：落点会话仅 1 条 `is_proactive=true` 的 assistant 消息
    let messages = storage
        .list_messages(message.session_id)
        .await
        .expect("读取消息应成功");
    assert_eq!(messages.len(), 1, "新建会话应仅落库 1 条");
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(messages[0].is_proactive, "应为主动消息");
    assert_eq!(
        message.message_id, messages[0].id,
        "投递负载应携带落库消息 id"
    );

    // 状态记账：投递时间 / 当日计数 / 退避计数 / 选题冷却
    let reloaded = reload_state(&storage).await;
    assert_eq!(reloaded.last_sent_at, Some(now));
    assert_eq!(reloaded.daily_count, 1);
    assert_eq!(reloaded.silence_streak, 1);
    assert_eq!(reloaded.recent_topics.len(), 1);
    assert_eq!(reloaded.recent_topics[0].source, "event");
    assert_eq!(reloaded.recent_topics[0].key, "evt-1");

    let _ = std::fs::remove_dir_all(dir);
}

/// 未注册接收端：生成成功计投递时间，但不记选题冷却。
#[tokio::test]
async fn tick_without_sink_records_sent_but_no_cooldown() {
    let now = now_ms();
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-tick-no-sink",
        MockLlm::with_reply(REPLY),
        test_config(),
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let picker = CountingPicker::returning(directive(None));
    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(summary.generated, 1);
    assert_eq!(summary.delivered, 0, "未注册接收端应静默丢弃");

    let reloaded = reload_state(&storage).await;
    assert_eq!(reloaded.last_sent_at, Some(now), "生成成功应记投递时间");
    assert!(reloaded.recent_topics.is_empty(), "未投递不应记选题冷却");

    let _ = std::fs::remove_dir_all(dir);
}

/// 投递失败：不记选题冷却（允许下窗重试），其余记账照常。
#[tokio::test]
async fn tick_sink_failure_does_not_record_cooldown() {
    let now = now_ms();
    let (engine, storage, dir) = engine_with_llm_and_config(
        "proactive-tick-sink-fail",
        MockLlm::with_reply(REPLY),
        test_config(),
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    engine.set_proactive_sink(Arc::new(TestSink::failing()));
    let picker = CountingPicker::returning(directive(None));
    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(summary.generated, 1);
    assert_eq!(summary.delivered, 0);

    let reloaded = reload_state(&storage).await;
    assert_eq!(reloaded.last_sent_at, Some(now), "生成成功应记投递时间");
    assert!(reloaded.recent_topics.is_empty(), "投递失败不应记选题冷却");

    let _ = std::fs::remove_dir_all(dir);
}

/// 生成失败：错误被吸收为 warn（不 panic），不记投递状态，判据尝试已记账。
#[tokio::test]
async fn tick_generation_failure_keeps_sent_unrecorded() {
    let now = now_ms();
    let (engine, storage, dir) =
        engine_with_llm_and_config("proactive-tick-gen-fail", MockLlm::failing(), test_config())
            .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    seed_dialogue_history(&storage, DEFAULT_PERSONA_UID).await;
    engine.set_state(AppState::Ready);

    let picker = CountingPicker::returning(directive(None));
    let summary = run_tick(&engine, now, &picker)
        .await
        .expect("生成失败应被吸收");
    assert_eq!(summary.generated, 0);
    assert_eq!(summary.delivered, 0);
    assert_eq!(summary.attempts, 1, "生成失败仍应计入选题尝试");
    assert_eq!(picker.calls(), 1, "生成失败前应已尝试选题");

    let reloaded = reload_state(&storage).await;
    assert_eq!(reloaded.last_sent_at, None, "生成失败不应记投递时间");
    assert_eq!(reloaded.last_judge_at, Some(now), "判据尝试应记账");

    let _ = std::fs::remove_dir_all(dir);
}

/// 总开关关闭：一律不触达选题，摘要全零。
#[tokio::test]
async fn tick_disabled_by_config() {
    let mut config = test_config();
    config.proactive.enabled = false;
    let (engine, _storage, dir) = ready_engine("proactive-tick-disabled", config).await;

    let picker = CountingPicker::empty();
    let summary = run_tick(&engine, now_ms(), &picker)
        .await
        .expect("单轮应完成");
    assert_eq!(summary, ProactiveTickSummary::default());
    assert_eq!(picker.calls(), 0, "总开关关闭不应触达选题");

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 空选题器与后台循环
// =========================================================

/// 空选题器：不产出候选但消费判据尝试记账。
#[tokio::test]
async fn noop_picker_returns_none() {
    let now = now_ms();
    let (engine, storage, dir) = ready_engine("proactive-noop-picker", test_config()).await;

    let picker = CountingPicker::empty();
    let picked = picker
        .pick(
            &engine,
            DEFAULT_PERSONA_UID,
            now,
            &mut ProactiveState::default(),
            0.0,
        )
        .await;
    assert!(picked.is_none(), "空选题器不产出候选");

    let summary = run_tick(&engine, now, &picker).await.expect("单轮应完成");
    assert_eq!(summary.attempts, 1, "空候选也计入判据尝试");
    assert_eq!(summary.generated, 0);
    assert_eq!(
        reload_state(&storage).await.last_judge_at,
        Some(now),
        "进入选题应消费节流记账"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 后台循环：停止位置位后任务在超时内干净退出。
#[tokio::test]
async fn spawn_loop_stops_cleanly() {
    let mut config = test_config();
    config.proactive.enabled = false;
    let (engine, _storage, dir) = ready_engine("proactive-spawn-stop", config).await;

    let shutdown = Arc::new(AtomicBool::new(false));
    let handle = spawn(
        Arc::new(engine),
        Arc::clone(&shutdown),
        0,
        1,
        Arc::new(CountingPicker::empty()),
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    shutdown.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("调度任务应随停止位退出")
        .expect("任务不应 panic");

    let _ = std::fs::remove_dir_all(dir);
}
