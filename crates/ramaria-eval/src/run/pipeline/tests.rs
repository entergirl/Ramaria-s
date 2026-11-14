//! crates/ramaria-eval/src/run/pipeline/tests.rs - Ramaria 评测条件内管线单元测试
//!
//! 设计特点:
//! - 有序缓冲覆盖乱序到达、缺号阻塞与游标推进
//! - 顺序写出用假执行器制造"后题先完成"，断言产物顺序仍是题集顺序
//! - 并发上限用峰值在飞数断言：默认值不超过 4 且大于 1，1 路恒为 1
//! - 串行与并发各跑一遍，逐题标识、回复与用量总量一致，只有耗时不同
//! - 取消只写已派发的前缀；重试计数按题收敛，失败后成功不留失败计数

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ramaria_core::traits::LlmUsage;
use ramaria_service::{EvalChatOutcome, EvalRecallObservation};
use uuid::Uuid;

use crate::baseline::QrelsRow;
use crate::run::{
    AnswerModel, AnswerModelKind, ConditionLine, ConditionState, RunItemRecord, RunMatrix,
    RunSection, RunStatus,
};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-pipeline-{tag}-{nanos}"))
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn item(question_id: &str) -> RunItem {
    let row: QrelsRow = serde_json::from_value(serde_json::json!({
        "question_id": question_id,
        "sample_key": "user-1",
        "ask_time": "2025-12-02",
        "ask_time_ms": 1_765_000_000_000i64,
        "question": "问题",
        "answer": "答案",
        "category": [0],
        "question_type": ["Single_hop"],
        "evidence": ["chat1"],
        "gold_message_ids": ["m1"],
        "score_points": []
    }))
    .expect("qrels 夹具应可构造");
    RunItem {
        row,
        tier: "u1-m1".to_string(),
        persona_uid: "lifebench-user-1".to_string(),
    }
}

fn condition(pipeline: Pipeline) -> Condition {
    Condition {
        id: "u1-m1/layered/800/0".to_string(),
        tier: "u1-m1".to_string(),
        pipeline,
        budget_chars: 800,
        answer_model: AnswerModel {
            kind: AnswerModelKind::Local,
            base_url: "http://127.0.0.1:1234/v1".to_string(),
            model: "local-model".to_string(),
        },
    }
}

fn line() -> ConditionLine {
    ConditionLine {
        corpus_name: "lifebench".to_string(),
        corpus_version: "2.0".to_string(),
        language: "zh".to_string(),
        corpus_sha256: "deadbeef".to_string(),
        memory_model: "local-model".to_string(),
        memory_model_kind: "local".to_string(),
        memory_temperature: 0.0,
        memory_max_tokens: 0,
        embedding_fingerprint: "fp".to_string(),
        embedding_dimension: None,
        prompt_template_version: "v1".to_string(),
        schema_version: 4,
        index_status: "ready".to_string(),
        device_os: "test".to_string(),
        device_arch: "test".to_string(),
        device_cores: 1,
    }
}

fn run_state(condition_id: &str, total: usize) -> RunState {
    RunState {
        artifact_kind: "ramaria-eval-run".to_string(),
        schema_version: 1,
        run_id: "run-test".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        status: RunStatus::Running,
        matrix: RunMatrix {
            run: RunSection {
                corpus: "lifebench".to_string(),
                language: "zh".to_string(),
                tiers: vec!["u1-m1".to_string()],
                pipelines: vec![Pipeline::Layered],
                budgets: vec![800],
                question_limit: None,
                question_sample: None,
                strict_isolation: false,
                retry_limit: 2,
            },
            answer_models: vec![AnswerModel {
                kind: AnswerModelKind::Local,
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                model: "local-model".to_string(),
            }],
            judge: None,
        },
        language: "zh".to_string(),
        tiers: Vec::new(),
        conditions: vec![ConditionState {
            id: condition_id.to_string(),
            total,
            completed: 0,
            failed: 0,
        }],
        elapsed_ms: 0,
        warnings: Vec::new(),
    }
}

fn success(
    item: &RunItem,
    condition: &Condition,
    line: &ConditionLine,
    reply: &str,
    tokens: u32,
) -> ItemRun {
    let outcome = EvalChatOutcome {
        reply: reply.to_string(),
        session_id: Uuid::nil(),
        chars: reply.chars().count(),
        recall: EvalRecallObservation::default(),
        usage: Some(LlmUsage {
            prompt_tokens: tokens,
            completion_tokens: tokens,
            cache_hit_tokens: 0,
            cache_miss_tokens: tokens,
        }),
    };
    let record = RunItemRecord::success(
        item,
        condition,
        outcome,
        1,
        1,
        "local-model",
        None,
        line.clone(),
    );
    ItemRun {
        record,
        success: true,
        attempts: 1,
        error_kind: None,
    }
}

/// 构造一条失败结果（含累计尝试次数）。
fn failure(item: &RunItem, condition: &Condition, line: &ConditionLine, attempts: u32) -> ItemRun {
    ItemRun {
        record: RunItemRecord::failure(
            item,
            condition,
            "模拟失败",
            1,
            attempts,
            "local-model",
            None,
            line.clone(),
        ),
        success: false,
        attempts,
        error_kind: Some("llm"),
    }
}

/// 读出逐题产物的全部记录（一行一题）。
fn read_records(items_path: &Path) -> Vec<RunItemRecord> {
    let text = std::fs::read_to_string(items_path).expect("产物应可读取");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("产物行应可解析"))
        .collect()
}

/// 用假执行器跑一遍条件：第 0 题故意最慢，后题先完成。
async fn fake_run(dir: &Path, concurrency: usize, items: &[RunItem]) -> Vec<RunItemRecord> {
    std::fs::create_dir_all(dir).expect("目录应可创建");
    let items_path = dir.join("items.jsonl");
    let condition = condition(Pipeline::Layered);
    let line = line();
    let mut state = run_state(&condition.id, items.len());
    let mut done = HashSet::new();
    let mut attempts = HashMap::new();
    let started = Instant::now();
    let control = RunControl::default();
    let jobs: Vec<(usize, &RunItem)> = items.iter().enumerate().collect();
    {
        let mut sink = ItemSink::new(
            &items_path,
            dir,
            &condition.id,
            &mut done,
            &mut attempts,
            &mut state,
            started,
            &control,
        );
        ordered_run(
            jobs,
            concurrency,
            |item| {
                let condition = &condition;
                let line = &line;
                // 首题最慢：若按完成顺序写出，产物顺序会先把后题排到前面。
                let slow = item.row.question_id == "q0";
                let text = format!("r-{}", item.row.question_id);
                let tokens = item
                    .row
                    .question_id
                    .trim_start_matches('q')
                    .parse::<u32>()
                    .unwrap_or(0)
                    + 1;
                async move {
                    tokio::time::sleep(Duration::from_millis(if slow { 20 } else { 1 })).await;
                    Some(success(item, condition, line, &text, tokens))
                }
            },
            |outcome| sink.write(outcome),
        )
        .await
        .expect("写出应成功");
    }
    assert_eq!(
        state.conditions[0].completed,
        items.len(),
        "成功题数应与写出题数一致"
    );
    read_records(&items_path)
}

// =========================================================
// 并发度
// =========================================================

/// 环境变量取值合法时按原值，越界与非法值回退默认。
#[test]
fn concurrency_parses_env_values() {
    assert_eq!(parse_concurrency(None), DEFAULT_CONCURRENCY, "未设置取默认");
    assert_eq!(
        parse_concurrency(Some("")),
        DEFAULT_CONCURRENCY,
        "空白取默认"
    );
    assert_eq!(
        parse_concurrency(Some("abc")),
        DEFAULT_CONCURRENCY,
        "非数字取默认"
    );
    assert_eq!(
        parse_concurrency(Some("0")),
        DEFAULT_CONCURRENCY,
        "0 越界回退默认"
    );
    assert_eq!(
        parse_concurrency(Some("17")),
        DEFAULT_CONCURRENCY,
        "超过上限回退默认"
    );
    assert_eq!(parse_concurrency(Some("1")), 1);
    assert_eq!(parse_concurrency(Some("16")), MAX_CONCURRENCY);
    assert_eq!(parse_concurrency(Some(" 8 ")), 8, "两侧空白应忽略");
}

/// oracle-l1 恒串行，不受环境变量影响。
#[test]
fn oracle_pipeline_stays_serial() {
    assert_eq!(concurrency_for(Pipeline::OracleL1), 1);
    assert_eq!(concurrency_for(Pipeline::FullContext), DEFAULT_CONCURRENCY);
}

// =========================================================
// 有序缓冲
// =========================================================

/// 乱序到达按序号放出，缺号阻塞后续结果。
#[test]
fn ordered_sink_releases_only_contiguous_results() {
    let mut sink = OrderedSink::new();
    sink.insert(2, "c");
    assert!(sink.drain_ready().is_empty(), "队首缺号时不应放出后续结果");

    sink.insert(0, "a");
    assert_eq!(sink.drain_ready(), vec!["a"], "0 到达后只放出 0");
    assert!(sink.drain_ready().is_empty(), "缺 1 时 2 仍需等待");

    sink.insert(1, "b");
    assert_eq!(sink.drain_ready(), vec!["b", "c"], "补齐后按序放出");
    assert!(sink.drain_ready().is_empty(), "缓冲已空");
}

// =========================================================
// 顺序与并发
// =========================================================

/// 后题先完成时，产物仍按题目原顺序写出。
#[tokio::test]
async fn ordered_run_keeps_question_order() {
    let dir = temp_dir("order");
    let items: Vec<RunItem> = (0..3).map(|index| item(&format!("q{index}"))).collect();
    let records = fake_run(&dir, DEFAULT_CONCURRENCY, &items).await;

    let ids: Vec<&str> = records
        .iter()
        .map(|record| record.question_id.as_str())
        .collect();
    assert_eq!(ids, vec!["q0", "q1", "q2"], "写出顺序应为题目原顺序");
    cleanup(&dir);
}

/// 在飞题目数不超过并发度；1 路时退化为逐题串行。
#[tokio::test]
async fn in_flight_items_stay_within_limit() {
    let items: Vec<RunItem> = (0..8).map(|index| item(&format!("q{index}"))).collect();
    let condition = condition(Pipeline::Layered);
    let line = line();

    for (concurrency, expected) in [(DEFAULT_CONCURRENCY, DEFAULT_CONCURRENCY), (1, 1)] {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let jobs: Vec<(usize, &RunItem)> = items.iter().enumerate().collect();
        let mut written = 0usize;
        ordered_run(
            jobs,
            concurrency,
            |item| {
                let in_flight = Arc::clone(&in_flight);
                let peak = Arc::clone(&peak);
                let condition = &condition;
                let line = &line;
                async move {
                    let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Some(success(item, condition, line, "r", 1))
                }
            },
            |outcome| {
                written += 1;
                let _ = outcome;
                Ok(())
            },
        )
        .await
        .expect("执行应成功");

        assert_eq!(written, items.len(), "全部题目都应写出");
        let peak = peak.load(Ordering::SeqCst);
        assert_eq!(
            peak, expected,
            "并发 {concurrency} 的峰值在飞数应为 {expected}"
        );
    }
}

/// 并发 1 与默认并发各跑一遍：题目顺序、回复与用量总量一致。
#[tokio::test]
async fn serial_and_concurrent_products_agree() {
    let items: Vec<RunItem> = (0..5).map(|index| item(&format!("q{index}"))).collect();
    let serial_dir = temp_dir("serial");
    let concurrent_dir = temp_dir("concurrent");
    let serial = fake_run(&serial_dir, 1, &items).await;
    let concurrent = fake_run(&concurrent_dir, DEFAULT_CONCURRENCY, &items).await;

    let summary = |records: &[RunItemRecord]| {
        records
            .iter()
            .map(|record| {
                (
                    record.question_id.clone(),
                    record.reply.clone(),
                    record
                        .usage
                        .map(|usage| usage.prompt_tokens + usage.completion_tokens),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        summary(&serial),
        summary(&concurrent),
        "逐题标识、回复与用量应一致"
    );

    let total_usage = |records: &[RunItemRecord]| {
        records
            .iter()
            .filter_map(|record| record.usage)
            .map(|usage| usage.prompt_tokens + usage.completion_tokens)
            .sum::<u32>()
    };
    assert_eq!(
        total_usage(&serial),
        total_usage(&concurrent),
        "用量总量应一致"
    );

    cleanup(&serial_dir);
    cleanup(&concurrent_dir);
}

// =========================================================
// 取消与计数
// =========================================================

/// 取消只让尾部任务返回 None：已派发结果全部写出，不因缺号留在缓冲。
///
/// 说明: 未执行的任务不插入有序缓冲、不占序号；取消是单调的，None 只出现在
/// 已派发段之后，因此游标不会因漏号停住，已启动的题也不会丢。
#[tokio::test]
async fn cancelled_tail_does_not_block_dispatched_results() {
    let jobs: Vec<(usize, usize)> = (0..6).map(|seq| (seq, seq)).collect();
    let mut written = Vec::new();
    ordered_run(
        jobs,
        3,
        |seq| async move {
            if seq >= 3 {
                return None;
            }
            Some(seq)
        },
        |seq| {
            written.push(seq);
            Ok(())
        },
    )
    .await
    .expect("执行应成功");

    assert_eq!(
        written,
        vec![0, 1, 2],
        "已派发的三条结果应全部写出，尾部取消项不阻塞也不写出"
    );
}

/// 取消后不再派新题：写出的都是取消前已在飞的题，且保持题目原顺序。
#[tokio::test]
async fn cancel_keeps_in_flight_items_and_stops_dispatch() {
    let dir = temp_dir("cancel");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let items_path = dir.join("items.jsonl");
    let items: Vec<RunItem> = (0..8).map(|index| item(&format!("q{index}"))).collect();
    let condition = condition(Pipeline::Layered);
    let line = line();
    let mut state = run_state(&condition.id, items.len());
    let mut done = HashSet::new();
    let mut attempts = HashMap::new();
    let control = RunControl::default();
    let started_at = Instant::now();
    let started_jobs = Arc::new(AtomicUsize::new(0));
    let jobs: Vec<(usize, &RunItem)> = items.iter().enumerate().collect();
    {
        let mut sink = ItemSink::new(
            &items_path,
            &dir,
            &condition.id,
            &mut done,
            &mut attempts,
            &mut state,
            started_at,
            &control,
        );
        ordered_run(
            jobs,
            2,
            |item| {
                let counter = Arc::clone(&started_jobs);
                let control = &control;
                let condition = &condition;
                let line = &line;
                async move {
                    if control.is_cancelled() {
                        return None;
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    Some(success(item, condition, line, "r", 1))
                }
            },
            |outcome| {
                sink.write(outcome)?;
                // 首题写出即中断：其余未派发的题不应再启动。
                control.cancel();
                Ok(())
            },
        )
        .await
        .expect("执行应成功");
    }
    let records = read_records(&items_path);
    let started_jobs = started_jobs.load(Ordering::SeqCst);
    assert!(
        records.len() < items.len(),
        "中断后不应把全部题目写出，实际 {}",
        records.len()
    );
    assert_eq!(
        records.len(),
        started_jobs,
        "已启动的题都应写出，未启动的题不应写出"
    );
    let ids: Vec<&str> = records
        .iter()
        .map(|record| record.question_id.as_str())
        .collect();
    let expected: Vec<String> = (0..records.len())
        .map(|index| format!("q{index}"))
        .collect();
    assert_eq!(
        ids,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "写出的题应是题集前缀（取消只发生在派发之后）"
    );
    assert_eq!(
        state.conditions[0].completed,
        records.len(),
        "完成计数应与写出行数一致"
    );
    cleanup(&dir);
}

/// 重试计数不虚高：失败后成功只留完成，同一题再次失败只留一个失败。
#[test]
fn retry_keeps_condition_counts_exact() {
    let dir = temp_dir("retry");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let items_path = dir.join("items.jsonl");
    let items: Vec<RunItem> = (0..2).map(|index| item(&format!("q{index}"))).collect();
    let condition = condition(Pipeline::Layered);
    let line = line();
    let mut state = run_state(&condition.id, items.len());
    let mut done = HashSet::new();
    let mut attempts = HashMap::new();
    let control = RunControl::default();
    {
        let mut sink = ItemSink::new(
            &items_path,
            &dir,
            &condition.id,
            &mut done,
            &mut attempts,
            &mut state,
            Instant::now(),
            &control,
        );
        // 第一题：失败一次后成功，失败计数应回退。
        sink.write(failure(&items[0], &condition, &line, 1))
            .expect("失败行应可写出");
        sink.write(success(&items[0], &condition, &line, "r", 1))
            .expect("成功行应可写出");
        // 第二题：连续两次失败，失败计数只应加一次。
        sink.write(failure(&items[1], &condition, &line, 1))
            .expect("失败行应可写出");
        sink.write(failure(&items[1], &condition, &line, 2))
            .expect("失败行应可写出");
    }

    assert_eq!(state.conditions[0].completed, 1, "完成数应只含成功题");
    assert_eq!(state.conditions[0].failed, 1, "失败数应按题计而非按行计");
    assert!(
        !attempts.contains_key(&(condition.id.clone(), "q0".to_string())),
        "成功题应移出重试记录"
    );
    assert_eq!(
        attempts.get(&(condition.id.clone(), "q1".to_string())),
        Some(&2),
        "再次失败的题应保留最新尝试次数"
    );
    let records = read_records(&items_path);
    assert_eq!(records.len(), 4, "四行都应按写出顺序落盘");
    cleanup(&dir);
}
