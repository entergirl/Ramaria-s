//! crates/ramaria-service/src/proactive/state/tests.rs - 主动对话状态读写单元测试
//!
//! 设计特点:
//! - 覆盖完整往返、覆盖写、画像隔离、缺键回退、损坏回退、部分字段回退
//! - 全局状态覆盖往返 / 覆盖写、缺键回退与键形稳定
//! - 使用真实 SQLite 临时库（`settings` 表读写与生产同路径）
//! - 锁定状态键形 `proactive.state.{persona_uid}` 与 `proactive.state.global`
//! - 只构造合成状态，不依赖网络 / LLM

use super::*;
use crate::test_support::engine_with_db;
use ramaria_core::traits::StoreInfrastructure;

/// 构造一条含全部字段的非默认状态。
fn sample_state() -> ProactiveState {
    ProactiveState {
        last_sent_at: Some(1_700_000_000_000),
        daily_count: 2,
        daily_date: "2026-10-02".to_string(),
        silence_streak: 1,
        recent_topics: vec![RecentTopic {
            source: "event".to_string(),
            key: "42".to_string(),
            sent_at: 1_700_000_000_000,
        }],
        first_seen_at: Some(1_699_000_000_000),
        last_judge_at: Some(1_700_000_100_000),
        hour_histogram: Some([1; 24]),
        histogram_date: "2026-10-02".to_string(),
        last_valence_sign: -1,
        judge_yes_count: 3,
        judge_no_count: 2,
    }
}

/// 完整往返 + 覆盖写 + 画像隔离。
#[tokio::test]
async fn state_roundtrip_overwrite_and_persona_isolation() {
    let (_engine, storage, dir) = engine_with_db("proactive-state-roundtrip").await;

    let state = sample_state();
    save_state(storage.as_ref(), "char-0001", &state)
        .await
        .expect("保存应成功");
    let loaded = load_state(storage.as_ref(), "char-0001")
        .await
        .expect("读取应成功");
    assert_eq!(loaded, state, "读写应无损往返（含 recent_topics）");
    assert_eq!(loaded.last_valence_sign, -1, "效价符号应读写往返");
    assert_eq!(loaded.judge_yes_count, 3, "判据开口计数应读写往返");
    assert_eq!(loaded.judge_no_count, 2, "判据沉默计数应读写往返");

    // 覆盖写：同一键以最新快照为准
    let mut updated = state.clone();
    updated.daily_count = 3;
    updated.silence_streak = 0;
    save_state(storage.as_ref(), "char-0001", &updated)
        .await
        .expect("覆盖写应成功");
    let reloaded = load_state(storage.as_ref(), "char-0001")
        .await
        .expect("读取应成功");
    assert_eq!(reloaded, updated, "覆盖写应生效");

    // 画像隔离：未写入的画像读回默认状态
    let other = load_state(storage.as_ref(), "char-0002")
        .await
        .expect("读取应成功");
    assert_eq!(other, ProactiveState::default(), "不同画像状态互不串扰");

    let _ = std::fs::remove_dir_all(dir);
}

/// 键缺失 → 默认状态（空态非错误）。
#[tokio::test]
async fn missing_key_returns_default_without_error() {
    let (_engine, storage, dir) = engine_with_db("proactive-state-missing").await;

    let state = load_state(storage.as_ref(), "char-none")
        .await
        .expect("缺失键应回退默认而非报错");
    assert_eq!(state, ProactiveState::default());

    let _ = std::fs::remove_dir_all(dir);
}

/// 损坏 JSON → 回退默认状态（不阻塞调度）。
#[tokio::test]
async fn corrupted_json_falls_back_to_default() {
    let (_engine, storage, dir) = engine_with_db("proactive-state-corrupt").await;

    storage
        .set_setting("proactive.state.char-0001", "{ 不是 JSON")
        .await
        .expect("写入损坏值应成功");
    let state = load_state(storage.as_ref(), "char-0001")
        .await
        .expect("损坏值应回退默认而非报错");
    assert_eq!(state, ProactiveState::default(), "损坏 JSON 应回退默认");

    let _ = std::fs::remove_dir_all(dir);
}

/// 部分字段 JSON → 缺失字段回退默认（版本演进宽容）。
#[tokio::test]
async fn partial_json_fills_field_defaults() {
    let (_engine, storage, dir) = engine_with_db("proactive-state-partial").await;

    storage
        .set_setting(
            "proactive.state.char-0001",
            r#"{"daily_count":3,"daily_date":"2026-10-02"}"#,
        )
        .await
        .expect("写入部分状态应成功");
    let state = load_state(storage.as_ref(), "char-0001")
        .await
        .expect("读取应成功");
    assert_eq!(state.daily_count, 3);
    assert_eq!(state.daily_date, "2026-10-02");
    assert_eq!(state.last_sent_at, None, "缺失字段应回退默认");
    assert_eq!(state.silence_streak, 0);
    assert!(state.recent_topics.is_empty());

    let _ = std::fs::remove_dir_all(dir);
}

/// 状态键形稳定：`proactive.state.{persona_uid}`。
#[tokio::test]
async fn state_key_shape_is_stable() {
    let (_engine, storage, dir) = engine_with_db("proactive-state-key").await;

    save_state(storage.as_ref(), "char-0001", &ProactiveState::default())
        .await
        .expect("保存应成功");
    assert!(
        storage
            .get_setting("proactive.state.char-0001")
            .await
            .expect("读取应成功")
            .is_some(),
        "状态键应为 proactive.state.{{persona_uid}}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 旧版 JSON（缺新增字段）解析：旧字段保留、新字段回退默认值。
#[tokio::test]
async fn legacy_json_without_new_fields_falls_back() {
    let (_engine, storage, dir) = engine_with_db("proactive-state-legacy").await;

    storage
        .set_setting(
            "proactive.state.char-0001",
            r#"{"last_sent_at":1700000000000,"daily_count":2,"daily_date":"2026-10-02",
               "silence_streak":1,"recent_topics":[{"source":"event","key":"42","sent_at":1700000000000}]}"#,
        )
        .await
        .expect("写入旧版状态应成功");
    let state = load_state(storage.as_ref(), "char-0001")
        .await
        .expect("读取应成功");
    assert_eq!(state.daily_count, 2, "旧字段应保留");
    assert_eq!(state.silence_streak, 1);
    assert_eq!(state.first_seen_at, None, "缺 first_seen_at 应回退默认");
    assert_eq!(state.last_judge_at, None, "缺 last_judge_at 应回退默认");
    assert_eq!(state.hour_histogram, None, "缺 hour_histogram 应回退默认");
    assert_eq!(state.histogram_date, "", "缺 histogram_date 应回退默认");
    assert_eq!(
        state.last_valence_sign, 0,
        "缺 last_valence_sign 应回退默认"
    );
    assert_eq!(state.judge_yes_count, 0, "缺 judge_yes_count 应回退默认");
    assert_eq!(state.judge_no_count, 0, "缺 judge_no_count 应回退默认");

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 全局状态
// =========================================================

/// 全局状态往返 + 覆盖写。
#[tokio::test]
async fn global_state_roundtrip_and_overwrite() {
    let (_engine, storage, dir) = engine_with_db("proactive-global-roundtrip").await;

    let state = ProactiveGlobalState {
        daily_count: 2,
        daily_date: "2026-10-02".to_string(),
    };
    save_global_state(storage.as_ref(), &state)
        .await
        .expect("保存应成功");
    let loaded = load_global_state(storage.as_ref())
        .await
        .expect("读取应成功");
    assert_eq!(loaded, state, "读写应无损往返");

    // 覆盖写：同一键以最新快照为准
    let updated = ProactiveGlobalState {
        daily_count: 3,
        daily_date: "2026-10-03".to_string(),
    };
    save_global_state(storage.as_ref(), &updated)
        .await
        .expect("覆盖写应成功");
    let reloaded = load_global_state(storage.as_ref())
        .await
        .expect("读取应成功");
    assert_eq!(reloaded, updated, "覆盖写应生效");

    let _ = std::fs::remove_dir_all(dir);
}

/// 键缺失 → 默认全局状态（空态非错误）。
#[tokio::test]
async fn global_state_missing_key_returns_default() {
    let (_engine, storage, dir) = engine_with_db("proactive-global-missing").await;

    let state = load_global_state(storage.as_ref())
        .await
        .expect("缺失键应回退默认而非报错");
    assert_eq!(state, ProactiveGlobalState::default());

    let _ = std::fs::remove_dir_all(dir);
}

/// 损坏 JSON → 回退默认全局状态（不阻塞调度）。
#[tokio::test]
async fn global_state_corrupted_json_falls_back_to_default() {
    let (_engine, storage, dir) = engine_with_db("proactive-global-corrupt").await;

    storage
        .set_setting("proactive.state.global", "{ 不是 JSON")
        .await
        .expect("写入损坏值应成功");
    let state = load_global_state(storage.as_ref())
        .await
        .expect("损坏值应回退默认而非报错");
    assert_eq!(
        state,
        ProactiveGlobalState::default(),
        "损坏 JSON 应回退默认"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 全局状态键形稳定：`proactive.state.global`。
#[tokio::test]
async fn global_state_key_shape_is_stable() {
    let (_engine, storage, dir) = engine_with_db("proactive-global-key").await;

    save_global_state(storage.as_ref(), &ProactiveGlobalState::default())
        .await
        .expect("保存应成功");
    assert!(
        storage
            .get_setting("proactive.state.global")
            .await
            .expect("读取应成功")
            .is_some(),
        "全局状态键应为 proactive.state.global"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 本地时间工具与 `chrono::Local` 同口径（本地时区自洽 + 超范围安全退化）。
#[test]
fn local_time_helpers_agree_with_local_now() {
    let now = chrono::Local::now();
    let ms = now.timestamp_millis();
    assert_eq!(local_date_str(ms), now.format("%Y-%m-%d").to_string());
    assert_eq!(local_hour(ms), now.hour());
    assert_eq!(local_minute_of_day(ms), now.hour() * 60 + now.minute());

    // 固定本地时刻：小时 / 当日分钟 / 日期文本自洽
    let fixed = chrono::Local
        .with_ymd_and_hms(2026, 1, 15, 10, 30, 0)
        .single()
        .expect("固定本地时刻应可表示");
    let fixed_ms = fixed.timestamp_millis();
    assert_eq!(local_date_str(fixed_ms), "2026-01-15");
    assert_eq!(local_hour(fixed_ms), 10);
    assert_eq!(local_minute_of_day(fixed_ms), 10 * 60 + 30);

    // 超范围时间戳：安全退化（空串 / 0），不 panic
    assert_eq!(local_date_str(i64::MAX), "");
    assert_eq!(local_hour(i64::MAX), 0);
    assert_eq!(local_minute_of_day(i64::MIN), 0);
}
