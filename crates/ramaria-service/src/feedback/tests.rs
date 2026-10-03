//! crates/ramaria-service/src/feedback/tests.rs - Ramaria 弱反馈模块测试
//!
//! 设计特点:
//! - 由 feedback.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖纠正前缀匹配 /
//!   弱信号检测 / 去重窗口 / 端到端落库四条路径
//! - 检测用例纯内存构造消息（零 I/O）；落库用例走真实 SQLite（临时文件库）
//! - 时间戳显式给定，检测窗口边界与间隔计算均为确定性断言
//!
//! 安全约束:
//! - 用例断言 detail 不含原文全文（仅前缀词等脱敏字段）；数据均为合成样例。

use super::*;
use crate::test_support::{engine_with_db, seed_persona};
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{MessageSource, new_id};

fn msg(role: MessageRole, content: &str, created_at: i64) -> Message {
    Message {
        id: new_id(),
        session_id: new_id(),
        role,
        content: content.to_string(),
        created_at,
        source: MessageSource::Local,
        fingerprint: None,
        persona_uid: None,
        is_proactive: false,
    }
}

fn cfg() -> FeedbackConfig {
    FeedbackConfig::default()
}

// ---- 纠正前缀 ----

#[test]
fn correction_prefix_hit() {
    assert_eq!(correction_prefix_match("不对，应该是这样的"), Some("不对"));
    assert_eq!(correction_prefix_match("不是这样"), Some("不是"));
    assert_eq!(correction_prefix_match("应该说重点"), Some("应该说"));
    assert_eq!(correction_prefix_match("其实我更想..."), Some("其实"));
    // 空白容忍
    assert_eq!(correction_prefix_match("  不对   "), Some("不对"));
}

#[test]
fn correction_prefix_miss() {
    assert_eq!(correction_prefix_match("好的，知道了"), None);
    assert_eq!(correction_prefix_match("对，没错"), None);
    assert_eq!(correction_prefix_match(""), None);
    assert_eq!(correction_prefix_match("   "), None);
}

// ---- 检测 ----

#[test]
fn detect_s2_correction_within_window() {
    let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
    let user = msg(MessageRole::User, "不对，你理解错了", 1100); // 100ms 后
    let recent = vec![assistant, user];
    let signal = detect_weak_signal(&recent, &cfg(), 1100).unwrap();
    assert_eq!(signal.signal_type, SignalType::Correction);
    assert_eq!(signal.matched_prefix, Some("不对"));
    assert_eq!(signal.interval_ms, 100);
}

#[test]
fn detect_s3_continue_within_window() {
    let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
    let user = msg(MessageRole::User, "好的继续聊这个话题", 1300);
    let recent = vec![assistant, user];
    let signal = detect_weak_signal(&recent, &cfg(), 1300).unwrap();
    assert_eq!(signal.signal_type, SignalType::Continue);
    assert_eq!(signal.matched_prefix, None);
    assert_eq!(signal.interval_ms, 300);
}

#[test]
fn detect_no_signal_beyond_window() {
    // 间隔 > 60s → 沉默/中断，不判为负反馈
    let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
    let user = msg(MessageRole::User, "好的", 1000 + 61_000);
    let recent = vec![assistant, user];
    assert!(detect_weak_signal(&recent, &cfg(), 1000 + 61_000).is_none());
}

#[test]
fn detect_no_signal_when_less_than_two_messages() {
    let user = msg(MessageRole::User, "只有一条", 1000);
    assert!(detect_weak_signal(&[user], &cfg(), 1000).is_none());
}

#[test]
fn detect_no_signal_when_no_assistant_predecessor() {
    // prev 是用户消息（非助手回复）→ 不检测
    let prev_user = msg(MessageRole::User, "前一条用户消息", 1000);
    let curr_user = msg(MessageRole::User, "当前用户消息", 1100);
    let recent = vec![prev_user, curr_user];
    assert!(detect_weak_signal(&recent, &cfg(), 1100).is_none());
}

#[test]
fn detect_disabled_returns_none() {
    let mut config = cfg();
    config.enabled = false;
    let assistant = msg(MessageRole::Assistant, "回复", 1000);
    let user = msg(MessageRole::User, "不对", 1100);
    assert!(detect_weak_signal(&[assistant, user], &config, 1100).is_none());
}

// ---- detail 脱敏 ----

#[test]
fn signal_detail_excludes_raw_text() {
    let assistant = msg(MessageRole::Assistant, "回复内容", 1000);
    let user = msg(MessageRole::User, "不对，你理解错了，原文内容", 1100);
    let signal = detect_weak_signal(&[assistant, user], &cfg(), 1100).unwrap();
    let detail = build_signal_detail(&signal);
    assert!(detail.contains("matched_prefix"));
    assert!(detail.contains("不对"));
    assert!(
        !detail.contains("你理解错了"),
        "detail 不得含原文全文（仅前缀词）"
    );
}

// ---- 去重窗口 ----

#[tokio::test]
async fn dedup_detects_recent_same_signal() {
    let (_engine, storage, dir) = engine_with_db("feedback-dedup").await;
    seed_persona(&storage, "char-0001").await;

    let now = now_ms();
    // 写入一条 correction 反馈（同一目标）
    storage
        .save_feedback_log(&FeedbackLog::new(
            "char-0001",
            TargetType::BehaviorRule,
            "3",
            SignalType::Correction,
            None,
            None,
        ))
        .await
        .expect("写入反馈日志应成功");

    // 去重窗口内同一目标 correction → 应去重
    let dup = should_dedup_recent_feedback(
        storage.as_ref(),
        &cfg(),
        "char-0001",
        SignalType::Correction,
        "3",
        now,
    )
    .await
    .expect("查询应成功");
    assert!(dup, "窗口内同信号应去重");
    // 不同目标 → 不去重
    let dup_other = should_dedup_recent_feedback(
        storage.as_ref(),
        &cfg(),
        "char-0001",
        SignalType::Correction,
        "99",
        now,
    )
    .await
    .expect("查询应成功");
    assert!(!dup_other);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 端到端写入 ----

#[tokio::test]
async fn writes_correction_feedback_with_weight_and_privacy() {
    // S2 纠正信号完整链路：检测 → 排除项 → feedback_log 写入
    let (_engine, storage, dir) = engine_with_db("feedback-correction").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    // 会话已有上一条助手回复（窗口内）
    let reply_at = now_ms() - 20_000;
    let mut assistant = msg(MessageRole::Assistant, "我认为应该是甲方案", reply_at);
    assistant.session_id = session.id;
    assistant.persona_uid = Some("char-0001".into());
    storage
        .save_message(&assistant)
        .await
        .expect("写入助手消息应成功");

    // 当前用户纠正消息（构造检测序列）
    let recent = vec![
        assistant.clone(),
        msg(MessageRole::User, "不对，应该是乙方案", now_ms()),
    ];

    process_feedback_for_new_message(
        storage.as_ref(),
        &cfg(),
        session.id,
        Some("char-0001"),
        &recent,
    )
    .await
    .expect("处理成功");

    // 断言 feedback_log 写入
    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("读取反馈日志应成功");
    assert_eq!(logs.len(), 1, "应写入一条 S2 纠正反馈");
    assert_eq!(logs[0].signal_type, SignalType::Correction);
    assert!((logs[0].weight - 0.6).abs() < f64::EPSILON, "S2 weight=0.6");
    // 隐私：detail 不含原文全文
    let detail = logs[0].detail.as_deref().unwrap_or_default();
    assert!(
        !detail.contains("应该是乙方案"),
        "detail 不得含用户消息原文全文"
    );
    assert!(detail.contains("不对"), "detail 含纠正前缀词（脱敏）");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn writes_continue_feedback_with_weight_02() {
    // S3 继续信号：非纠正、窗口内 → Continue，weight=0.2
    let (_engine, storage, dir) = engine_with_db("feedback-continue").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let reply_at = now_ms() - 10_000;
    let mut assistant = msg(MessageRole::Assistant, "好的，我们继续", reply_at);
    assistant.session_id = session.id;
    assistant.persona_uid = Some("char-0001".into());
    storage
        .save_message(&assistant)
        .await
        .expect("写入助手消息应成功");

    let recent = vec![
        assistant.clone(),
        msg(MessageRole::User, "接着聊下一件事", now_ms()),
    ];
    process_feedback_for_new_message(
        storage.as_ref(),
        &cfg(),
        session.id,
        Some("char-0001"),
        &recent,
    )
    .await
    .expect("处理成功");

    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("读取反馈日志应成功");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].signal_type, SignalType::Continue);
    assert!((logs[0].weight - 0.2).abs() < f64::EPSILON, "S3 weight=0.2");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn auto_apply_off_does_not_modify_review_queue() {
    // auto_apply_weak_feedback=false（默认）：弱信号仅写 feedback_log，
    // 不修改 settings 复审队列（零自动修改）
    let (_engine, storage, dir) = engine_with_db("feedback-auto-off").await;
    seed_persona(&storage, "char-0001").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let reply_at = now_ms() - 10_000;
    let mut assistant = msg(MessageRole::Assistant, "回复", reply_at);
    assistant.session_id = session.id;
    assistant.persona_uid = Some("char-0001".into());
    storage
        .save_message(&assistant)
        .await
        .expect("写入助手消息应成功");

    let recent = vec![
        assistant.clone(),
        msg(MessageRole::User, "不对，纠正一下", now_ms()),
    ];
    // 默认配置 auto_apply=false
    let config = FeedbackConfig::default();
    assert!(!config.auto_apply_weak_feedback);
    process_feedback_for_new_message(
        storage.as_ref(),
        &config,
        session.id,
        Some("char-0001"),
        &recent,
    )
    .await
    .expect("处理成功");

    // feedback_log 已写（审计）
    let logs = storage
        .list_feedback_logs_by_persona("char-0001")
        .await
        .expect("读取反馈日志应成功");
    assert_eq!(logs.len(), 1);
    // 复审队列未被修改（settings 无 REVIEW_QUEUE_KEY）
    let queue = storage
        .get_setting(REVIEW_QUEUE_KEY)
        .await
        .expect("读取成功");
    assert!(queue.is_none(), "auto_apply=false 时不应写复审队列");

    let _ = std::fs::remove_dir_all(&dir);
}
