//! crates/ramaria-service/src/proactive/state/tests.rs - 主动对话状态读写单元测试
//!
//! 设计特点:
//! - 覆盖完整往返、覆盖写、画像隔离、缺键回退、损坏回退、部分字段回退
//! - 使用真实 SQLite 临时库（`settings` 表读写与生产同路径）
//! - 锁定状态键形 `proactive.state.{persona_uid}`
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
