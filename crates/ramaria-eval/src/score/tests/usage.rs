//! crates/ramaria-eval/src/score/tests/usage.rs - Ramaria 评测用量观测测试
//!
//! 设计特点:
//! - 用逐题产物夹具锁定"答案按题累加、判分按调用累加"两路口径
//! - 命中率只按输入 token 占比计算，无输入时显示未知
//! - 用量单独落 `usage.json`：锁定文件形状，以及"缓存命中零用量"的遥测语义
//! - 只读写临时目录，不触真实评测产物与用户库

use std::sync::Arc;

use ramaria_core::traits::LlmUsage;
use ramaria_testkit::MockLlm;

use super::{judge_for, observation, points_body, record, temp_dir, two_point_record, write_run};
use crate::score::{
    ScoreOptions, ScoreReport, USAGE_ARTIFACT_KIND, USAGE_FILE, USAGE_SCHEMA_VERSION, UsageReport,
    finish, read_state, run,
};

/// 答案用量逐题累加：调用数、token 合计与命中率按输入占比，摘要输出两行用量。
#[tokio::test]
async fn usage_sums_answer_tokens_and_renders_two_lines() {
    let dir = temp_dir("usage-answer");
    let mut first = record(
        "q1",
        &["m1"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    first.usage = Some(LlmUsage {
        prompt_tokens: 1000,
        completion_tokens: 100,
        cache_hit_tokens: 768,
        cache_miss_tokens: 232,
    });
    let mut second = record(
        "q2",
        &["m2"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    second.usage = Some(LlmUsage {
        prompt_tokens: 1000,
        completion_tokens: 50,
        cache_hit_tokens: 256,
        cache_miss_tokens: 744,
    });
    let plain = record(
        "q3",
        &["m3"],
        Some("回复"),
        Some(observation(&[], &[], &[])),
    );
    write_run(&dir, &[first, second, plain]);

    let (report, usage) = run(&dir, ScoreOptions { no_judge: true })
        .await
        .expect("判分应成功");

    assert_eq!(usage.total.answer.calls, 2, "无用量题不计调用");
    assert_eq!(usage.total.answer.prompt_tokens, 2000);
    assert_eq!(usage.total.answer.completion_tokens, 150);
    assert_eq!(usage.total.answer.cache_hit_tokens, 1024);
    assert_eq!(usage.total.answer.cache_miss_tokens, 976);
    let rate = usage.total.answer.cache_hit_rate.expect("应有命中率");
    assert!((rate - 1024.0 / 2000.0).abs() < 1e-9, "命中率={rate}");
    assert_eq!(
        usage.conditions[0].answer, usage.total.answer,
        "单条件运行级合计等于条件值"
    );

    let text = report.render(&ScoreReport::dir_of(&dir), &usage.total);
    assert!(
        text.contains("用量（答案）: 调用 2 / 输入 2000（命中 1024，命中率 51.2%）/ 输出 150"),
        "答案用量行: {text}"
    );
    assert!(
        text.contains("用量（判分）: 调用 0 / 输入 0（命中 0，命中率 未知）/ 输出 0"),
        "无判分时命中率显示未知: {text}"
    );

    // 落盘与内存结构同源：形状固定、末尾换行、可反序列化回读。
    let raw = std::fs::read_to_string(ScoreReport::dir_of(&dir).join(USAGE_FILE))
        .expect("应写出用量产物");
    assert!(raw.ends_with('\n'), "用量产物应以换行结尾");
    let stored: UsageReport = serde_json::from_str(&raw).expect("用量产物应可解析");
    assert_eq!(stored, usage, "落盘与内存结构应一致");
    assert_eq!(stored.artifact_kind, USAGE_ARTIFACT_KIND);
    assert_eq!(stored.schema_version, USAGE_SCHEMA_VERSION);
    assert_eq!(stored.run_id, "run-test");
    assert_eq!(stored.items_sha256.len(), 64, "应是 sha256 十六进制");
    assert_eq!(stored.conditions[0].condition_id, "u1-m1/layered/400/0");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 判分缓存命中时再次判分零调用：`usage.json` 随之变空，判分产物保持不变。
#[tokio::test]
async fn rescore_refreshes_usage_without_touching_products() {
    let dir = temp_dir("usage-telemetry");
    write_run(&dir, &[two_point_record("q1")]);
    let mock = Arc::new(MockLlm::with_reply(&points_body(&[true, false])));
    mock.set_responses(vec![
        points_body(&[true, false]),
        points_body(&[true, false]),
        points_body(&[true, false]),
        "4".to_string(),
    ]);
    let score_dir = ScoreReport::dir_of(&dir);

    let state = read_state(&dir).expect("运行状态应可读");
    let (_, first) = finish(&dir, state, Vec::new(), Some(judge_for(&mock, &dir)))
        .await
        .expect("首次判分应成功");
    assert_eq!(first.total.judge.calls, 4, "分项三票 + 语气维单票");
    assert_eq!(first.total.judge.calls, mock.chat_calls());
    let first_usage = std::fs::read(score_dir.join(USAGE_FILE)).expect("应写出用量");
    let first_score = std::fs::read(score_dir.join(crate::score::SCORE_FILE)).expect("应写出汇总");

    let state = read_state(&dir).expect("运行状态应可读");
    let (_, second) = finish(&dir, state, Vec::new(), Some(judge_for(&mock, &dir)))
        .await
        .expect("再次判分应成功");
    assert_eq!(mock.chat_calls(), 4, "命中缓存不再调用模型");
    assert_eq!(second.total.judge.calls, 0, "缓存命中不产生判分用量");
    assert_eq!(
        second.total.answer, first.total.answer,
        "答案用量来自逐题产物，两次相同"
    );
    assert_ne!(
        std::fs::read(score_dir.join(USAGE_FILE)).expect("应写出用量"),
        first_usage,
        "用量是运行遥测，两次判分允许不同"
    );
    assert_eq!(
        std::fs::read(score_dir.join(crate::score::SCORE_FILE)).expect("应写出汇总"),
        first_score,
        "判分结果不随用量变化"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
