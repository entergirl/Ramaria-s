//! crates/ramaria-eval/src/baseline/qrels/tests.rs - Ramaria 评测基线 qrels 测试
//!
//! 设计特点:
//! - gold 并集覆盖 evidence 顺序、重复 artifact 去重与多消息 artifact
//! - 不可回答题断言 evidence 与 gold 均为空
//! - 查询窗口断言两端含入、越界各一天排除
//! - 提问时钟断言为当日 23:59:59.999
//! - 落盘断言每行一条 JSON 且可反序列化

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::corpus::ScorePoint;

use crate::baseline::tier::U1_M1;

// =========================================================
// 夹具
// =========================================================

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("夹具日期应合法")
}

/// 构造一条 QA。
fn qa(question_id: &str, ask_time: NaiveDate, evidence: &[&str], category: &[u8]) -> Qa {
    Qa {
        question_id: question_id.to_string(),
        ask_time,
        question: format!("问题 {question_id}"),
        answer: "参考答案".to_string(),
        evidence: evidence.iter().map(|item| item.to_string()).collect(),
        category: category.to_vec(),
        question_type: vec!["Single_hop".to_string()],
        score_points: vec![ScorePoint {
            description: "要点".to_string(),
            score: 10.0,
        }],
    }
}

/// 构造只含 QA 的样本。
fn sample_with(qa: Vec<Qa>) -> Sample {
    Sample {
        sample_id: "样本甲".to_string(),
        user_key: "jia".to_string(),
        speaker_a: "甲".to_string(),
        speaker_b: "乙".to_string(),
        sessions: Vec::new(),
        qa,
    }
}

/// 构造 artifact → 消息 id 映射。
fn artifact_map(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(artifact, ids)| {
            (
                (*artifact).to_string(),
                ids.iter().map(|id| (*id).to_string()).collect(),
            )
        })
        .collect()
}

// =========================================================
// gold 与时钟
// =========================================================

/// gold 按 evidence 顺序拼接 artifact 内消息，并去重。
#[test]
fn gold_union_follows_evidence_order_and_dedupes() {
    let sample = sample_with(vec![qa(
        "lifebench_v5_jia_qa0",
        date(2025, 12, 10),
        &["chat1", "calendar2", "chat1"],
        &[0],
    )]);
    let artifacts = artifact_map(&[("chat1", &["m1", "m2"]), ("calendar2", &["m3"])]);

    let rows = rows(&sample, &U1_M1, &artifacts);
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0].gold_message_ids,
        vec!["m1", "m2", "m3"],
        "应为 evidence 顺序下的消息并集"
    );
    assert_eq!(rows.missing_evidence, 0);
}

/// 不可回答题没有 evidence，gold 为空。
#[test]
fn unanswerable_has_empty_gold() {
    let sample = sample_with(vec![qa(
        "lifebench_v5_jia_qa1",
        date(2025, 12, 11),
        &[],
        &[4],
    )]);
    let rows = rows(&sample, &U1_M1, &artifact_map(&[]));
    assert!(rows.rows[0].evidence.is_empty());
    assert!(rows.rows[0].gold_message_ids.is_empty());
    assert_eq!(rows.missing_evidence, 0);
}

/// evidence 未命中建库窗口时计数，gold 为空。
#[test]
fn missing_evidence_is_counted() {
    let sample = sample_with(vec![qa(
        "lifebench_v5_jia_qa2",
        date(2025, 12, 12),
        &["chat1", "chat0"],
        &[0],
    )]);
    let rows = rows(&sample, &U1_M1, &artifact_map(&[("chat1", &["m1"])]));
    assert_eq!(rows.rows[0].gold_message_ids, vec!["m1"]);
    assert_eq!(rows.missing_evidence, 1);
}

/// 提问时钟取当日 23:59:59.999。
#[test]
fn ask_time_ms_is_end_of_day() {
    let target = date(2025, 6, 15);
    let millis = ask_time_ms(target);
    let expected = target
        .and_hms_milli_opt(23, 59, 59, 999)
        .expect("时刻应合法");
    let actual = chrono::DateTime::from_timestamp_millis(millis)
        .expect("毫秒应可还原")
        .naive_utc();
    assert_eq!(actual, expected);
}

// =========================================================
// 窗口过滤与落盘
// =========================================================

/// 只保留建库起点到查询截止日之间的题。
#[test]
fn filters_by_query_window() {
    let sample = sample_with(vec![
        qa("lifebench_v5_jia_qa0", date(2025, 11, 30), &["chat1"], &[0]),
        qa("lifebench_v5_jia_qa1", date(2025, 12, 1), &["chat1"], &[0]),
        qa("lifebench_v5_jia_qa2", date(2026, 1, 15), &["chat1"], &[0]),
        qa("lifebench_v5_jia_qa3", date(2026, 1, 16), &["chat1"], &[0]),
    ]);
    let rows = rows(&sample, &U1_M1, &artifact_map(&[("chat1", &["m1"])]));
    let ids: Vec<&str> = rows
        .rows
        .iter()
        .map(|row| row.question_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["lifebench_v5_jia_qa1", "lifebench_v5_jia_qa2"],
        "窗口两端应含入，越界一天排除"
    );
}

/// 每行一条 JSON，字段可原样读回。
#[test]
fn writes_one_json_per_line() {
    let sample = sample_with(vec![
        qa("lifebench_v5_jia_qa0", date(2025, 12, 10), &["chat1"], &[0]),
        qa("lifebench_v5_jia_qa1", date(2025, 12, 11), &[], &[4]),
    ]);
    let rows = rows(&sample, &U1_M1, &artifact_map(&[("chat1", &["m1"])]));
    assert_eq!(rows.rows.len(), 2);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir: PathBuf = std::env::temp_dir().join(format!("ramaria-eval-qrels-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");

    write(&dir, &rows.rows).expect("qrels 应可写出");
    let text = std::fs::read_to_string(dir.join(QRELS_FILE)).expect("qrels 应可读");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "每题一行");
    for line in lines {
        let row: QrelsRow = serde_json::from_str(line).expect("每行应可反序列化");
        assert_eq!(row.sample_key, "jia");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
