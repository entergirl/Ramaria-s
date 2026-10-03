//! crates/ramaria-service/src/proactive/picker/tests.rs - Ramaria 主动对话选题器测试
//!
//! 设计特点:
//! - 真实 SQLite 临时库造事件 / 会话映射 / 消息 / 行为规则，直接调用选题器断言产出
//! - 事件时间相对固定 `now` 构造，跟进点与静默判定不依赖真实时钟
//! - 纯事件路径关闭判据（judge_enabled=false），观察算法打分直取的排序结果
//! - 判据路径用脚本化 LLM 按调用序返回裁决 JSON，并断言开口 / 沉默计数

use std::path::PathBuf;
use std::sync::Arc;

use ramaria_core::behavior::{BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{MemoryEvent, MemoryL1, now_ms};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

use super::*;
use crate::proactive::schedule::TopicPicker;
use crate::proactive::state::RecentTopic;
use crate::test_support::{
    MockLlm, ScriptedLlm, engine_with_llm_and_config, engine_with_shared_scripted_llm,
    seed_messages, seed_persona,
};
use crate::types::DEFAULT_PERSONA_UID;

/// 判据开口的裁决输出（合法 JSON；编号 c0 回引排序第一名）。
const SPEAK_JSON: &str = r#"{"speak": true, "candidate_id": "c0", "angle": "关心近况", "tone": "温和", "reason_bucket": "speak"}"#;

/// 一天的毫秒数（测试内独立给出，便于阅读）。
const DAY_MS: i64 = 86_400_000;
/// 一小时的毫秒数。
const HOUR_MS: i64 = 3_600_000;

// =========================================================
// 夹具
// =========================================================

/// 纯事件路径配置：判据关闭（算法直取），其余取默认值。
fn events_config() -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.proactive.judge_enabled = false;
    config
}

/// 装配"判据关闭"的选题引擎（纯事件 / 规则 / 轻触达用例）。
async fn engine_events(tag: &str) -> (Engine, Arc<SqliteStorage>, PathBuf) {
    let (engine, storage, dir) =
        engine_with_llm_and_config(tag, MockLlm::local(), events_config()).await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    (engine, storage, dir)
}

/// 装配"脚本化判据"的选题引擎（返回共享 LLM 句柄供调用计数断言）。
async fn engine_judge(
    tag: &str,
    replies: &[&str],
) -> (Engine, Arc<ScriptedLlm>, Arc<SqliteStorage>, PathBuf) {
    let llm = Arc::new(ScriptedLlm::replies(replies));
    let (engine, storage, dir) =
        engine_with_shared_scripted_llm(tag, Arc::clone(&llm), RamariaConfig::default(), None)
            .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    (engine, llm, storage, dir)
}

/// 直接调用真实选题器（固定人格与活跃度权重）。
async fn pick_once(
    engine: &Engine,
    now: i64,
    state: &mut ProactiveState,
) -> Option<ProactiveDirective> {
    PickerTopicProvider
        .pick(engine, DEFAULT_PERSONA_UID, now, state, 0.8)
        .await
}

/// 造一条事件（信号显式设置；start = end），返回落库 id。
async fn seed_event(
    storage: &SqliteStorage,
    title: &str,
    summary: &str,
    end: i64,
    valence: f64,
    salience: f64,
    confidence: f64,
) -> i64 {
    let mut event = MemoryEvent::new(
        DEFAULT_PERSONA_UID.to_string(),
        title.to_string(),
        summary.to_string(),
        end,
        end,
    );
    event.valence = valence;
    event.salience = salience;
    event.confidence = confidence;
    event.created_at = end;
    storage.save_event(&event).await.expect("写入事件应成功")
}

/// 为事件附上会话映射（会话 + L1 + 事件溯源），返回所属会话 id。
async fn attach_event_to_session(storage: &SqliteStorage, event_id: i64) -> Uuid {
    let session = storage
        .create_session(Some(DEFAULT_PERSONA_UID))
        .await
        .expect("创建会话应成功");
    let mut l1 = MemoryL1::new(session.id, "测试摘要".to_string(), None);
    l1.persona_uid = Some(DEFAULT_PERSONA_UID.to_string());
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    storage
        .save_event_source(event_id, l1.id, 1.0)
        .await
        .expect("写入事件溯源应成功");
    session.id
}

/// 造一条启用中的行为规则（关键词与效价显式设置），返回落库 id。
async fn seed_rule(
    storage: &SqliteStorage,
    keywords: &[&str],
    reaction: &str,
    valence: f64,
) -> i64 {
    let mut situation = BehaviorSituation::empty();
    situation.keywords = keywords.iter().map(|keyword| keyword.to_string()).collect();
    situation.valence_mean = valence;
    let rule = BehaviorRule::new(
        DEFAULT_PERSONA_UID,
        situation,
        Some(reaction.to_string()),
        BehaviorParams::default(),
        RuleSource::Auto,
    );
    storage
        .save_behavior_rule(&rule)
        .await
        .expect("写入行为规则应成功")
}

/// 构造一条近期选题记录。
fn recent(source: &str, key: &str, sent_at: i64) -> RecentTopic {
    RecentTopic {
        source: source.to_string(),
        key: key.to_string(),
        sent_at,
    }
}

// =========================================================
// 四源用例
// =========================================================

/// 高显著事件源：命中且按得分降序取第一名（无会话映射允许新建会话）。
#[tokio::test]
async fn salient_source_picks_highest_score() {
    let (engine, storage, dir) = engine_events("picker-salient-order").await;
    let now = now_ms();
    let strong = seed_event(
        &storage,
        "陶艺展",
        "周末去看了陶艺展",
        now - DAY_MS,
        0.2,
        0.9,
        0.9,
    )
    .await;
    seed_event(
        &storage,
        "读书会",
        "参加了读书会",
        now - DAY_MS,
        0.4,
        0.7,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("应有高显著事件候选");
    let key = strong.to_string();
    assert_eq!(directive.source, "event");
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));
    assert_eq!(directive.valence, 0.2);
    assert!(
        directive.session_id.is_none(),
        "无会话映射的高显著事件允许新建会话"
    );
    assert!(
        directive
            .anchor
            .as_deref()
            .is_some_and(|anchor| anchor.starts_with("陶艺展：")),
        "锚点应为标题与摘要组合"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 高显著事件源：显著性低于门槛的事件不入选（回退轻触达）。
#[tokio::test]
async fn salient_source_respects_salience_threshold() {
    let (engine, storage, dir) = engine_events("picker-salient-threshold").await;
    let now = now_ms();
    seed_event(
        &storage,
        "普通日常",
        "只是普通的一天",
        now - DAY_MS,
        0.5,
        0.5,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("低显著事件应回退轻触达");
    assert_eq!(directive.source, "light_touch");

    let _ = std::fs::remove_dir_all(dir);
}

/// 未了结源：负效价 + 会话静默（事件结束后无新对话）命中，落点所属会话。
#[tokio::test]
async fn unresolved_source_hits_negative_silent_event() {
    let (engine, storage, dir) = engine_events("picker-unresolved-hit").await;
    let now = now_ms();
    let end = now - DAY_MS;
    let event = seed_event(&storage, "工作压力", "最近加班很多", end, -0.5, 0.4, 0.9).await;
    let session = attach_event_to_session(&storage, event).await;
    seed_messages(&storage, session, DEFAULT_PERSONA_UID, 1, end - HOUR_MS).await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("未了结事件应命中");
    let key = event.to_string();
    assert_eq!(directive.source, "unresolved");
    assert_eq!(directive.session_id, Some(session));
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));
    assert_eq!(directive.valence, -0.5);

    let _ = std::fs::remove_dir_all(dir);
}

/// 未了结源：事件结束后有后续对话视为已了结，不入选。
#[tokio::test]
async fn unresolved_source_requires_silence() {
    let (engine, storage, dir) = engine_events("picker-unresolved-silence").await;
    let now = now_ms();
    let end = now - DAY_MS;
    let event = seed_event(&storage, "工作压力", "最近加班很多", end, -0.5, 0.4, 0.9).await;
    let session = attach_event_to_session(&storage, event).await;
    seed_messages(&storage, session, DEFAULT_PERSONA_UID, 1, end + HOUR_MS).await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("已了结事件应回退轻触达");
    assert_eq!(directive.source, "light_touch");

    let _ = std::fs::remove_dir_all(dir);
}

/// 未了结源：无会话映射不入选（无落点可用，轻触达兜底出现）。
#[tokio::test]
async fn unresolved_source_requires_session_map() {
    let (engine, storage, dir) = engine_events("picker-unresolved-no-map").await;
    let now = now_ms();
    seed_event(
        &storage,
        "近况",
        "最近状态一般",
        now - DAY_MS,
        -0.5,
        0.4,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("无映射事件应回退轻触达");
    assert_eq!(directive.source, "light_touch");

    let _ = std::fs::remove_dir_all(dir);
}

/// 时间节点源：事件结束后恰第 3 天命中（不要求静默）。
#[tokio::test]
async fn time_node_hits_on_follow_up_day() {
    let (engine, storage, dir) = engine_events("picker-time-node-hit").await;
    let now = now_ms();
    let end = now - 3 * DAY_MS;
    let event = seed_event(&storage, "认识一周年", "认识一周年", end, 0.0, 0.3, 0.9).await;
    let session = attach_event_to_session(&storage, event).await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("跟进点应命中");
    let key = event.to_string();
    assert_eq!(directive.source, "time_node");
    assert_eq!(directive.session_id, Some(session));
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));

    let _ = std::fs::remove_dir_all(dir);
}

/// 时间节点源：非跟进日（第 2 / 4 天）不命中。
#[tokio::test]
async fn time_node_misses_on_non_follow_up_days() {
    let (engine, storage, dir) = engine_events("picker-time-node-miss").await;
    let now = now_ms();
    let two_days = seed_event(
        &storage,
        "第 2 天",
        "前日事件",
        now - 2 * DAY_MS,
        0.0,
        0.3,
        0.9,
    )
    .await;
    let four_days = seed_event(
        &storage,
        "第 4 天",
        "数日前事件",
        now - 4 * DAY_MS,
        0.0,
        0.3,
        0.9,
    )
    .await;
    attach_event_to_session(&storage, two_days).await;
    attach_event_to_session(&storage, four_days).await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("非跟进日应回退轻触达");
    assert_eq!(directive.source, "light_touch");

    let _ = std::fs::remove_dir_all(dir);
}

/// 行为规则源：与事件文本高关键词重合的规则命中，候选带规则反应锚点。
#[tokio::test]
async fn rule_source_matches_event_context() {
    let (engine, storage, dir) = engine_events("picker-rule-hit").await;
    let now = now_ms();
    seed_event(&storage, "陶艺展", "", now - DAY_MS, 0.3, 0.3, 0.9).await;
    let rule_id = seed_rule(
        &storage,
        &["陶艺", "艺展"],
        "用户喜欢陶艺，可以聊聊展览",
        0.3,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("规则情境应命中");
    let key = rule_id.to_string();
    assert_eq!(directive.source, "rule");
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));
    assert_eq!(
        directive.anchor.as_deref(),
        Some("用户喜欢陶艺，可以聊聊展览")
    );
    assert!(directive.session_id.is_none(), "规则源落问候类新建会话");

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 过滤链用例
// =========================================================

/// 选题冷却：窗口内同键剔除；窗口外放行。
#[tokio::test]
async fn topic_cooldown_blocks_within_window_and_releases_after() {
    let (engine, storage, dir) = engine_events("picker-cooldown").await;
    let now = now_ms();
    let event = seed_event(&storage, "展览", "看了展览", now - DAY_MS, 0.3, 0.8, 0.9).await;
    let key = event.to_string();

    let mut cooling = ProactiveState::default();
    cooling.recent_topics = vec![recent("event", &key, now - 23 * HOUR_MS)];
    let directive = pick_once(&engine, now, &mut cooling)
        .await
        .expect("冷却窗口内应回退轻触达");
    assert_eq!(directive.source, "light_touch", "冷却窗口内同键不应复选");

    let mut released = ProactiveState::default();
    released.recent_topics = vec![recent("event", &key, now - 25 * HOUR_MS)];
    let directive = pick_once(&engine, now, &mut released)
        .await
        .expect("窗口外同键应放行");
    assert_eq!(directive.source, "event");
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));

    let _ = std::fs::remove_dir_all(dir);
}

/// 负效价出口：强负效价非静默事件不出现在任何事件源（最终轻触达兜底）。
#[tokio::test]
async fn negative_valence_not_exposed_by_other_sources() {
    let (engine, storage, dir) = engine_events("picker-negative-exit").await;
    let now = now_ms();
    let end = now - DAY_MS;
    let event = seed_event(&storage, "争执", "与朋友发生了争执", end, -0.6, 0.9, 0.9).await;
    let session = attach_event_to_session(&storage, event).await;
    seed_messages(&storage, session, DEFAULT_PERSONA_UID, 1, end + HOUR_MS).await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("应回退轻触达");
    assert_eq!(directive.source, "light_touch");
    assert_ne!(directive.source, "event");
    assert_ne!(directive.source, "unresolved");

    let _ = std::fs::remove_dir_all(dir);
}

/// 不连选：上一投递为负效价时负效价候选被剔除，正效价保留。
#[tokio::test]
async fn negative_valence_not_followed_by_negative() {
    let (engine, storage, dir) = engine_events("picker-no-repeat-negative").await;
    let now = now_ms();
    let end = now - DAY_MS;
    let negative = seed_event(&storage, "低落", "最近心情低落", end, -0.5, 0.9, 1.0).await;
    let session = attach_event_to_session(&storage, negative).await;
    seed_messages(&storage, session, DEFAULT_PERSONA_UID, 1, end - HOUR_MS).await;
    let positive = seed_event(&storage, "旅行", "规划了旅行", end, 0.4, 0.6, 0.8).await;

    // 上一投递非负效价：负效价候选得分更高，当选
    let mut neutral = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut neutral)
        .await
        .expect("应有候选");
    let negative_key = negative.to_string();
    assert_eq!(directive.source, "unresolved");
    assert_eq!(directive.topic_key.as_deref(), Some(negative_key.as_str()));

    // 上一投递为负效价：负效价候选被剔除，正效价保留
    let mut after_negative = ProactiveState::default();
    after_negative.last_valence_sign = -1;
    let directive = pick_once(&engine, now, &mut after_negative)
        .await
        .expect("正效价候选应保留");
    let positive_key = positive.to_string();
    assert_eq!(directive.source, "event");
    assert_eq!(directive.topic_key.as_deref(), Some(positive_key.as_str()));
    assert!(directive.valence > 0.0);

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 打分层用例
// =========================================================

/// 置信度门槛：低于门槛的事件剔除（回退轻触达）。
#[tokio::test]
async fn confidence_below_floor_is_dropped() {
    let (engine, storage, dir) = engine_events("picker-confidence-floor").await;
    let now = now_ms();
    seed_event(
        &storage,
        "低置信事件",
        "事实确凿度不足",
        now - DAY_MS,
        0.4,
        0.9,
        0.5,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("低置信事件应回退轻触达");
    assert_eq!(directive.source, "light_touch", "置信度低于门槛不应入选");

    let _ = std::fs::remove_dir_all(dir);
}

/// 置信度折扣：同显著性 / 效价下高置信事件排序在前。
#[tokio::test]
async fn confidence_discount_orders_candidates() {
    let (engine, storage, dir) = engine_events("picker-confidence-discount").await;
    let now = now_ms();
    let high = seed_event(
        &storage,
        "高置信",
        "高置信事件",
        now - DAY_MS,
        0.2,
        0.8,
        0.9,
    )
    .await;
    seed_event(
        &storage,
        "低置信",
        "低置信事件",
        now - DAY_MS,
        0.2,
        0.8,
        0.6,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state).await.expect("应有候选");
    let key = high.to_string();
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));

    let _ = std::fs::remove_dir_all(dir);
}

/// 显著性主权重：同置信度 / 效价下高显著事件排序在前。
#[tokio::test]
async fn salience_primary_weight_orders_candidates() {
    let (engine, storage, dir) = engine_events("picker-salience-weight").await;
    let now = now_ms();
    let high = seed_event(
        &storage,
        "高显著",
        "高显著事件",
        now - DAY_MS,
        0.2,
        0.9,
        0.9,
    )
    .await;
    seed_event(
        &storage,
        "低显著",
        "较低显著事件",
        now - DAY_MS,
        0.2,
        0.7,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state).await.expect("应有候选");
    let key = high.to_string();
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));

    let _ = std::fs::remove_dir_all(dir);
}

/// 效价强度加成：同置信度 / 显著性下 |valence| 更大者排序在前。
#[tokio::test]
async fn valence_magnitude_orders_candidates() {
    let (engine, storage, dir) = engine_events("picker-valence-weight").await;
    let now = now_ms();
    let strong = seed_event(
        &storage,
        "情绪强",
        "情绪波动很大",
        now - DAY_MS,
        0.6,
        0.7,
        0.9,
    )
    .await;
    seed_event(
        &storage,
        "情绪弱",
        "情绪波动较小",
        now - DAY_MS,
        0.2,
        0.7,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state).await.expect("应有候选");
    let key = strong.to_string();
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 轻触达用例
// =========================================================

/// 轻触达：四源皆空时产出无锚点低权重候选。
#[tokio::test]
async fn light_touch_used_when_sources_empty() {
    let (engine, _storage, dir) = engine_events("picker-light-touch").await;
    let now = now_ms();

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("轻触达应产出");
    assert_eq!(directive.source, "light_touch");
    assert_eq!(directive.topic_key.as_deref(), Some("light_touch"));
    assert!(directive.anchor.is_none());
    assert!(directive.session_id.is_none());
    assert_eq!(directive.valence, 0.0);

    let _ = std::fs::remove_dir_all(dir);
}

/// 轻触达冷却：窗口内无候选（保持沉默，不硬凑话题）。
#[tokio::test]
async fn light_touch_cooldown_returns_none() {
    let (engine, _storage, dir) = engine_events("picker-light-touch-cooldown").await;
    let now = now_ms();

    let mut state = ProactiveState::default();
    state.recent_topics = vec![recent("light_touch", "light_touch", now - HOUR_MS)];
    assert!(
        pick_once(&engine, now, &mut state).await.is_none(),
        "轻触达冷却窗口内本轮应无候选"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 判据用例
// =========================================================

/// 判据开口：产出指令并计开口一次，角度 / 语气来自裁决。
#[tokio::test]
async fn judge_speak_returns_directive_and_counts_yes() {
    let (engine, llm, storage, dir) = engine_judge("picker-judge-yes", &[SPEAK_JSON]).await;
    let now = now_ms();
    let event = seed_event(
        &storage,
        "陶艺展",
        "周末去看了陶艺展",
        now - DAY_MS,
        0.2,
        0.8,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("判据开口应产出");
    let key = event.to_string();
    assert_eq!(llm.call_count(), 1, "判据应恰好调用一次");
    assert_eq!(state.judge_yes_count, 1);
    assert_eq!(state.judge_no_count, 0);
    assert_eq!(directive.source, "event");
    assert_eq!(directive.topic_key.as_deref(), Some(key.as_str()));
    assert_eq!(directive.angle.as_deref(), Some("关心近况"));
    assert_eq!(directive.tone.as_deref(), Some("温和"));

    let _ = std::fs::remove_dir_all(dir);
}

/// 判据沉默：无产出并计沉默一次。
#[tokio::test]
async fn judge_silent_returns_none_and_counts_no() {
    let reply = r#"{"speak": false, "candidate_id": "", "angle": "", "tone": "", "reason_bucket": "too_recent"}"#;
    let (engine, llm, storage, dir) = engine_judge("picker-judge-no", &[reply]).await;
    let now = now_ms();
    seed_event(
        &storage,
        "陶艺展",
        "周末去看了陶艺展",
        now - DAY_MS,
        0.2,
        0.8,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    assert!(pick_once(&engine, now, &mut state).await.is_none());
    assert_eq!(llm.call_count(), 1);
    assert_eq!(state.judge_yes_count, 0);
    assert_eq!(state.judge_no_count, 1);

    let _ = std::fs::remove_dir_all(dir);
}

/// 判据失败（垃圾响应）：无产出且开口 / 沉默计数均不变。
#[tokio::test]
async fn judge_failure_returns_none_without_counts() {
    let (engine, llm, storage, dir) =
        engine_judge("picker-judge-failed", &["我觉得现在不适合说话。"]).await;
    let now = now_ms();
    seed_event(
        &storage,
        "陶艺展",
        "周末去看了陶艺展",
        now - DAY_MS,
        0.2,
        0.8,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    assert!(pick_once(&engine, now, &mut state).await.is_none());
    assert_eq!(llm.call_count(), 1);
    assert_eq!(state.judge_yes_count, 0);
    assert_eq!(state.judge_no_count, 0, "解析失败不应计沉默");

    let _ = std::fs::remove_dir_all(dir);
}

/// 判据关闭回退：算法排序直取第一名，不调用 LLM。
#[tokio::test]
async fn judge_disabled_falls_back_to_algorithm_without_llm_call() {
    let llm = Arc::new(ScriptedLlm::replies(&[SPEAK_JSON]));
    let (engine, storage, dir) = engine_with_shared_scripted_llm(
        "picker-judge-off",
        Arc::clone(&llm),
        events_config(),
        None,
    )
    .await;
    seed_persona(&storage, DEFAULT_PERSONA_UID).await;
    let now = now_ms();
    seed_event(
        &storage,
        "陶艺展",
        "周末去看了陶艺展",
        now - DAY_MS,
        0.2,
        0.8,
        0.9,
    )
    .await;

    let mut state = ProactiveState::default();
    let directive = pick_once(&engine, now, &mut state)
        .await
        .expect("算法直取应产出");
    assert_eq!(directive.source, "event");
    assert!(directive.angle.is_none(), "算法路径无角度");
    assert!(directive.tone.is_none(), "算法路径无语气");
    assert_eq!(llm.call_count(), 0, "判据关闭不应发生 LLM 调用");

    let _ = std::fs::remove_dir_all(dir);
}

/// 候选空（轻触达冷却内）+ 判据开启：不调用判据。
#[tokio::test]
async fn empty_candidates_skip_judge_call() {
    let (engine, llm, _storage, dir) = engine_judge("picker-judge-empty", &[SPEAK_JSON]).await;
    let now = now_ms();

    let mut state = ProactiveState::default();
    state.recent_topics = vec![recent("light_touch", "light_touch", now - HOUR_MS)];
    assert!(pick_once(&engine, now, &mut state).await.is_none());
    assert_eq!(llm.call_count(), 0, "候选为空不应发起判据调用");

    let _ = std::fs::remove_dir_all(dir);
}
