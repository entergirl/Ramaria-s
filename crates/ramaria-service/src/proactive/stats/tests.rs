//! crates/ramaria-service/src/proactive/stats/tests.rs - 主动对话数值基线统计单元测试
//!
//! 设计特点:
//! - 装配真实 SQLite 临时库与 mock LLM：统计结果直接对照库中消息事实与状态键
//! - 覆盖零数据降级（比率为 None）、投递 / 回应配对、窗口边界与不限窗口、按日分桶
//! - 断言口径：投递 = 主动消息条数；回应 = 窗口内首条本地用户消息；判据计数取状态键

use super::*;
use crate::test_support::{MockLlm, engine_with_llm_and_config, seed_persona, seed_persona_kind};
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{Message, MessageRole, MessageSource, PersonaKind};
use ramaria_storage::SqliteStorage;
use std::sync::Arc;
use uuid::Uuid;

use super::state::ProactiveState;

/// 固定基准时间（Unix 毫秒）：日期分桶只断言总计数与升序，不依赖本地时区。
const BASE_TS: i64 = 1_700_000_000_000;

/// 装配"真实 SQLite + mock LLM"引擎并造 char / user 两类人格。
async fn fixture(tag: &str) -> (Engine, Arc<SqliteStorage>, std::path::PathBuf) {
    let (engine, storage, dir) =
        engine_with_llm_and_config(tag, MockLlm::local(), RamariaConfig::default()).await;
    seed_persona(&storage, "char-0001").await;
    seed_persona_kind(&storage, "user-0001", PersonaKind::User).await;
    (engine, storage, dir)
}

/// 写入一条本地消息（可带主动标记）。
async fn save_local_message(
    storage: &SqliteStorage,
    session_id: Uuid,
    role: MessageRole,
    created_at: i64,
    is_proactive: bool,
) {
    let mut message = Message::new(session_id, role, "内容".to_string(), MessageSource::Local)
        .with_proactive(is_proactive);
    message.created_at = created_at;
    storage
        .save_message(&message)
        .await
        .expect("写入消息应成功");
}

/// 无投递数据：全部人格条目为零值，比率 / 中位数为 None（不产出 0 / 0）。
#[tokio::test]
async fn collect_zero_data_degrades_to_empty_counts() {
    let (engine, _storage, dir) = fixture("proactive-stats-empty").await;

    let report = collect(&engine, 24).await.expect("统计应成功");
    assert!(report.generated_at > 0, "应带取数时间");
    assert_eq!(report.window_hours, 24);
    assert_eq!(report.personas.len(), 2, "应为全部活跃人格产出条目");
    assert!(
        report
            .personas
            .iter()
            .all(|row| row.deliveries == 0 && row.responded == 0 && row.daily.is_empty()),
        "无投递时条目应为零值"
    );
    assert_eq!(report.personas[0].response_rate, None);
    assert_eq!(report.totals.deliveries, 0);
    assert_eq!(report.totals.responded, 0);
    assert_eq!(report.totals.response_rate, None, "无投递时比率应为 None");
    assert_eq!(report.totals.median_response_ms, None);
    assert_eq!(report.global.daily_total_limit, 0, "应携带配置快照");
    assert_eq!(report.global.daily_count, 0);

    let _ = std::fs::remove_dir_all(dir);
}

/// 投递 / 回应 / 判据计数与按日分桶：只计主动消息、回应取窗口内首条本地用户消息。
#[tokio::test]
async fn collect_counts_deliveries_responses_and_judge() {
    let (engine, storage, dir) = fixture("proactive-stats-matrix").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    // 投递 d1 + 30 秒后的本地用户回应；投递 d2 之后无回应
    save_local_message(&storage, session.id, MessageRole::Assistant, BASE_TS, true).await;
    save_local_message(
        &storage,
        session.id,
        MessageRole::User,
        BASE_TS + 30_000,
        false,
    )
    .await;
    save_local_message(
        &storage,
        session.id,
        MessageRole::Assistant,
        BASE_TS + 3_600_000,
        true,
    )
    .await;
    // 常规助手消息不计投递
    save_local_message(
        &storage,
        session.id,
        MessageRole::Assistant,
        BASE_TS + 7_200_000,
        false,
    )
    .await;

    // 状态键（判据计数 / 当日记账 / 退避）随选题器与调度记账写入
    let st = ProactiveState {
        last_sent_at: Some(BASE_TS + 3_600_000),
        daily_count: 2,
        daily_date: state::local_date_str(BASE_TS + 3_600_000),
        silence_streak: 1,
        judge_yes_count: 3,
        judge_no_count: 5,
        last_judge_at: Some(BASE_TS + 3_600_000),
        first_seen_at: Some(BASE_TS),
        ..Default::default()
    };
    state::save_state(storage.as_ref(), "char-0001", &st)
        .await
        .expect("预置状态应成功");

    let report = collect(&engine, 24).await.expect("统计应成功");
    let row = report
        .personas
        .iter()
        .find(|row| row.uid == "char-0001")
        .expect("应包含目标人格");
    assert_eq!(row.deliveries, 2, "常规助手消息不计投递");
    assert_eq!(row.responded, 1);
    assert_eq!(row.response_rate, Some(0.5));
    assert_eq!(row.median_response_ms, Some(30_000));
    assert_eq!(row.judge_yes_count, 3);
    assert_eq!(row.judge_no_count, 5);
    assert_eq!(row.last_judge_at, Some(BASE_TS + 3_600_000));
    assert_eq!(row.silence_streak, 1);
    assert_eq!(row.daily_count, 2);
    assert_eq!(row.last_sent_at, Some(BASE_TS + 3_600_000));
    assert_eq!(row.first_seen_at, Some(BASE_TS));
    assert_eq!(row.kind, "char");

    let daily_sum: u32 = row.daily.iter().map(|bucket| bucket.count).sum();
    assert_eq!(daily_sum, 2, "按日分桶应覆盖全部投递");
    assert!(
        row.daily
            .windows(2)
            .all(|pair| pair[0].date <= pair[1].date),
        "按日分桶应按日期升序"
    );

    assert_eq!(report.totals.deliveries, 2);
    assert_eq!(report.totals.responded, 1);
    assert_eq!(report.totals.response_rate, Some(0.5));
    assert_eq!(report.totals.median_response_ms, Some(30_000));
    assert_eq!(report.totals.judge_yes_count, 3);
    assert_eq!(report.totals.judge_no_count, 5);
    assert_eq!(report.totals.personas, 2);

    let _ = std::fs::remove_dir_all(dir);
}

/// 回应窗口边界：窗口内命中、超窗不命中、0 = 不限窗口。
#[tokio::test]
async fn collect_window_bounds_response_and_zero_window_is_unlimited() {
    let (engine, storage, dir) = fixture("proactive-stats-window").await;
    let session = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    // 投递与 2 小时后的本地用户回应
    save_local_message(&storage, session.id, MessageRole::Assistant, BASE_TS, true).await;
    save_local_message(
        &storage,
        session.id,
        MessageRole::User,
        BASE_TS + 2 * 3_600_000,
        false,
    )
    .await;

    let narrow = collect(&engine, 1).await.expect("统计应成功");
    let row = narrow
        .personas
        .iter()
        .find(|row| row.uid == "char-0001")
        .expect("应包含目标人格");
    assert_eq!(row.responded, 0, "超出窗口不应命中回应");
    assert_eq!(row.response_rate, Some(0.0));

    let wide = collect(&engine, 3).await.expect("统计应成功");
    let row = wide
        .personas
        .iter()
        .find(|row| row.uid == "char-0001")
        .expect("应包含目标人格");
    assert_eq!(row.responded, 1, "窗口内应命中回应");
    assert_eq!(row.median_response_ms, Some(2 * 3_600_000));

    let unlimited = collect(&engine, 0).await.expect("统计应成功");
    let row = unlimited
        .personas
        .iter()
        .find(|row| row.uid == "char-0001")
        .expect("应包含目标人格");
    assert_eq!(row.responded, 1, "0 = 不限窗口应命中任意更晚的回应");

    let _ = std::fs::remove_dir_all(dir);
}
