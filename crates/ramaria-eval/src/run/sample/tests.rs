//! crates/ramaria-eval/src/run/sample/tests.rs - Ramaria 评测分层抽样单元测试
//!
//! 设计特点:
//! - 纯函数覆盖比例分配、稀有层保底、预算边界、多标签签名与确定性
//! - 抽样记录往返逐字段比对，并锁定末尾换行
//! - 续跑复用与三类不一致（规格 / 题数 / 题号缺失）逐个断言
//! - 接入层用真实档位夹具覆盖首跑落盘、续跑复用与规格冲突

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_core::error::RamariaError;

use crate::baseline::{ARTIFACT_KIND, ARTIFACT_SCHEMA_VERSION, MANIFEST_FILE, QRELS_FILE};
use crate::run::{AnswerModel, AnswerModelKind, BaselineTier, Pipeline, RunSection};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-sample-{tag}-{nanos}"))
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn row(sample_key: &str, question_id: &str, category: &[u8]) -> QrelsRow {
    serde_json::from_value(serde_json::json!({
        "question_id": question_id,
        "sample_key": sample_key,
        "ask_time": "2025-12-02",
        "ask_time_ms": 1_765_000_000_000i64,
        "question": "问题",
        "answer": "答案",
        "category": category,
        "question_type": ["Single_hop"],
        "evidence": ["chat1"],
        "gold_message_ids": ["m1"],
        "score_points": []
    }))
    .expect("qrels 夹具应可构造")
}

fn rows_of(sample_key: &str, count: usize, category: &[u8]) -> Vec<QrelsRow> {
    (0..count)
        .map(|index| row(sample_key, &format!("{sample_key}_qa{index:04}"), category))
        .collect()
}

fn picked(report: &TierSample, sample_key: &str, category: &str) -> usize {
    report
        .strata
        .iter()
        .find(|stratum| stratum.sample_key == sample_key && stratum.category == category)
        .map(|stratum| stratum.picked)
        .expect("层应存在于报告中")
}

fn matrix_with_sample(per_tier: usize) -> RunMatrix {
    RunMatrix {
        run: RunSection {
            corpus: "lifebench".to_string(),
            language: "zh".to_string(),
            tiers: vec!["u1-m1".to_string()],
            pipelines: vec![Pipeline::Layered],
            budgets: vec![400],
            question_limit: None,
            question_sample: Some(per_tier),
            strict_isolation: false,
            retry_limit: 0,
        },
        answer_models: vec![AnswerModel {
            kind: AnswerModelKind::Local,
            base_url: String::new(),
            model: String::new(),
        }],
        judge: None,
    }
}

fn tier_value(tier_id: &str) -> serde_json::Value {
    serde_json::json!({
        "artifact_kind": ARTIFACT_KIND,
        "schema_version": ARTIFACT_SCHEMA_VERSION,
        "baseline_id": format!("lifebench-2.0-zh-{tier_id}"),
        "created_at": "2026-01-01T00:00:00Z",
        "corpus": {
            "name": "lifebench",
            "version": "2.0",
            "language": "zh",
            "file_name": "lifebench.json",
            "sha256": "deadbeef",
            "size": 1,
            "qa_total": 3,
            "sample_ids": ["s1"]
        },
        "tier": {
            "id": tier_id,
            "users": 1,
            "memory_start": "2025-12-01",
            "memory_end": "2025-12-31",
            "query_end": "2026-01-15",
            "span_days": 31,
            "sample_ids": ["s1"]
        },
        "qa": { "total": 3, "in_window": 3 },
        "pipeline": {
            "crate_version": "0.0.0",
            "schema_version": 4,
            "prompt_template_version": "v1",
            "pipeline_version": "test",
            "code_fingerprint": "fingerprint"
        },
        "memory_model": {
            "kind": "local",
            "model": "m",
            "temperature": 0.0,
            "max_tokens": 16
        },
        "embedding": { "fingerprint": "e", "dimension": 8 },
        "index": { "status": "ready", "loaded": 1, "total": 1 },
        "counts": {
            "personas": 1,
            "sessions": 1,
            "messages": 2,
            "l1": 1,
            "l2": 0,
            "l3": 0,
            "unabsorbed_l1": 0
        },
        "timing": { "build_elapsed_ms": 1 },
        "warnings": []
    })
}

fn write_tier(root: &Path, tier_id: &str, rows: &[QrelsRow]) -> PathBuf {
    let dir = root.join(tier_id);
    std::fs::create_dir_all(&dir).expect("档位目录应可创建");
    let manifest = serde_json::to_string_pretty(&tier_value(tier_id)).expect("清单应可序列化");
    std::fs::write(dir.join(MANIFEST_FILE), manifest).expect("清单夹具应可写");
    let mut qrels = String::new();
    for row in rows {
        qrels.push_str(&serde_json::to_string(row).expect("题目应可序列化"));
        qrels.push('\n');
    }
    std::fs::write(dir.join(QRELS_FILE), qrels).expect("题目夹具应可写");
    dir
}

// =========================================================
// 分配
// =========================================================

/// 三个断层按大小分预算：每层至少 1 题，大层拿得更多，总数正好等于预算。
#[test]
fn allocation_keeps_every_stratum_and_favors_large_layers() {
    let mut rows = rows_of("A", 100, &[0]);
    rows.extend(rows_of("B", 20, &[1]));
    rows.extend(rows_of("C", 1, &[2]));

    let (indices, report) = sample_tier("u1-m1", &rows, 12);

    assert_eq!(report.total, 121);
    assert_eq!(report.selected, 12);
    assert_eq!(indices.len(), 12);
    let large = picked(&report, "A", "0");
    let middle = picked(&report, "B", "1");
    let tiny = picked(&report, "C", "2");
    assert!(middle >= 1, "中间层至少 1 题，实际 {middle}");
    assert!(tiny >= 1, "小层至少 1 题，实际 {tiny}");
    assert!(large > middle, "大层应更多，实际 {large} / {middle}");
    assert_eq!(large + middle + tiny, 12);
    assert!(
        indices.windows(2).all(|pair| pair[0] < pair[1]),
        "下标应升序且不重复"
    );
    assert!(indices.iter().all(|index| *index < rows.len()));
}

/// 稀有层的单题在 3380 题中仍被选中。
#[test]
fn rare_stratum_is_always_selected() {
    let mut rows = rows_of("user-00", 343, &[0]);
    for user in 1..10 {
        rows.extend(rows_of(&format!("user-{user:02}"), 337, &[0]));
    }
    rows.extend(rows_of("UA", 3, &[0]));
    rows.push(row("UA", "UA_qa_rare", &[7]));
    assert_eq!(rows.len(), 3380);

    let (_, report) = sample_tier("u10", &rows, 300);

    assert_eq!(report.selected, 300);
    assert_eq!(report.strata.len(), 12);
    assert_eq!(picked(&report, "UA", "7"), 1);
}

/// 同一输入两次抽样逐字段相同（含顺序）。
#[test]
fn sampling_is_deterministic() {
    let mut rows = rows_of("B", 12, &[1]);
    rows.extend(rows_of("A", 9, &[0]));
    rows.extend(rows_of("C", 15, &[2]));
    rows.push(row("A", "A_multi", &[0, 3]));

    let first = sample_tier("u1-q1", &rows, 7);
    let second = sample_tier("u1-q1", &rows, 7);

    assert_eq!(first.0, second.0);
    assert_eq!(first.1, second.1);
}

/// 多标签类别按排序后签名单独成层，与单标签层不混。
#[test]
fn multi_label_categories_form_their_own_stratum() {
    let rows = vec![
        row("A", "q1", &[0, 3]),
        row("A", "q2", &[3, 0]),
        row("A", "q3", &[0]),
        row("B", "q4", &[3]),
    ];

    let (_, report) = sample_tier("u1", &rows, 4);

    assert_eq!(report.strata.len(), 3);
    let multi = report
        .strata
        .iter()
        .find(|stratum| stratum.category == "0-3")
        .expect("多标签层应存在");
    assert_eq!(multi.size, 2);
    assert_eq!(multi.picked, 2);
    assert_eq!(picked(&report, "A", "0"), 1);
    assert_eq!(picked(&report, "B", "3"), 1);
}

/// 预算不小于题数时全选；预算小于层数时按层大小降序取前几层，每层 1 题。
#[test]
fn budget_boundaries_select_all_or_top_strata() {
    let mut rows = rows_of("A", 5, &[0]);
    rows.extend(rows_of("B", 3, &[0]));
    rows.extend(rows_of("C", 1, &[0]));

    let (indices, report) = sample_tier("u1", &rows, 9);
    assert_eq!(indices, (0..9).collect::<Vec<usize>>());
    assert!(
        report
            .strata
            .iter()
            .all(|stratum| stratum.picked == stratum.size)
    );

    let (indices, report) = sample_tier("u1", &rows, 2);
    assert_eq!(indices.len(), 2);
    assert_eq!(picked(&report, "A", "0"), 1);
    assert_eq!(picked(&report, "B", "0"), 1);
    assert_eq!(picked(&report, "C", "0"), 0);
}

// =========================================================
// 读写与续跑
// =========================================================

/// 抽样记录写读逐字段相等，文件末尾带换行；文件缺失返回 None。
#[test]
fn sample_report_roundtrips_with_trailing_newline() {
    let dir = temp_dir("io");
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");
    assert!(
        load(&dir).expect("缺失不应报错").is_none(),
        "未写记录时应返回 None"
    );

    let rows = rows_of("A", 5, &[0]);
    let (_, tier) = sample_tier("u1-m1", &rows, 2);
    let report = SampleReport {
        artifact_kind: SAMPLE_ARTIFACT_KIND.to_string(),
        schema_version: SAMPLE_SCHEMA_VERSION,
        spec: SampleSpec { per_tier: 2 },
        tiers: vec![tier],
    };
    let path = write(&dir, &report).expect("抽样记录应可写");

    assert_eq!(path, dir.join(SAMPLE_FILE));
    let text = std::fs::read_to_string(&path).expect("抽样记录应可读");
    assert!(text.ends_with('\n'), "文件应以换行结尾");
    let loaded = load(&dir).expect("抽样记录应可读").expect("记录应存在");
    assert_eq!(loaded, report);
    cleanup(&dir);
}

/// 同规格续跑复用同一批题号；规格、题数或题号变化逐个报错。
#[test]
fn reuse_keeps_same_questions_and_rejects_mismatch() {
    let original = rows_of("A", 10, &[0]);
    let mut rows = original.clone();
    let first = apply_tier("u1-m1", &mut rows, 4, None).expect("首次抽样应成功");
    assert_eq!(first.selected, 4);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        first.question_ids,
        rows.iter()
            .map(|row| row.question_id.clone())
            .collect::<Vec<_>>()
    );

    let mut reused = original.clone();
    let second = apply_tier("u1-m1", &mut reused, 4, Some(&first)).expect("续跑应复用记录");
    assert_eq!(second.question_ids, first.question_ids);
    assert_eq!(reused.len(), 4);

    let error = check_spec(SampleSpec { per_tier: 4 }, SampleSpec { per_tier: 5 })
        .expect_err("规格变化应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("既有 4 / 当前 5"),
        "文案应含规格对比，实际 {error}"
    );

    let mut grown = rows_of("A", 11, &[0]);
    let error = apply_tier("u1-m1", &mut grown, 4, Some(&first)).expect_err("题数变化应报错");
    assert!(
        error.to_string().contains("记录 10 / 当前 11"),
        "文案应含题数对比，实际 {error}"
    );

    let mut tampered = first.clone();
    tampered.question_ids.push("A_qa9999".to_string());
    tampered.selected += 1;
    let mut rows = original.clone();
    let error = apply_tier("u1-m1", &mut rows, 4, Some(&tampered)).expect_err("题号缺失应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("A_qa9999"),
        "文案应带题号，实际 {error}"
    );
}

/// 接入层首跑写出记录并过滤题集，续跑复用记录不重写，规格变化报错。
#[test]
fn apply_writes_and_reuses_sample_file() {
    let dir = temp_dir("apply");
    let tier_dir = write_tier(&dir, "u1-m1", &rows_of("A", 6, &[0]));
    let matrix = matrix_with_sample(2);

    let mut tiers = vec![BaselineTier::load(&tier_dir).expect("档位应可加载")];
    let first = apply(&matrix, &dir, &mut tiers).expect("首次抽样应成功");
    let report = first.report.expect("首跑应返回待写报告");
    assert!(
        first
            .lines
            .iter()
            .any(|line| line == "档位 u1-m1: 抽样 2/6（分层 1）"),
        "提示行应含抽样计数，实际 {:?}",
        first.lines
    );
    assert_eq!(report.tiers[0].selected, 2);
    write(&dir, &report).expect("抽样记录应可写");
    let first_ids: Vec<String> = tiers[0]
        .rows
        .iter()
        .map(|row| row.question_id.clone())
        .collect();
    assert_eq!(first_ids.len(), 2);

    let mut tiers = vec![BaselineTier::load(&tier_dir).expect("档位应可加载")];
    let second = apply(&matrix, &dir, &mut tiers).expect("续跑应成功");
    assert!(second.report.is_none(), "续跑不应重写记录");
    assert_eq!(second.lines, first.lines);
    let second_ids: Vec<String> = tiers[0]
        .rows
        .iter()
        .map(|row| row.question_id.clone())
        .collect();
    assert_eq!(second_ids, first_ids);

    let changed = matrix_with_sample(3);
    let mut tiers = vec![BaselineTier::load(&tier_dir).expect("档位应可加载")];
    let error = apply(&changed, &dir, &mut tiers).expect_err("规格变化应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("既有 2 / 当前 3"),
        "文案应含规格对比，实际 {error}"
    );
    cleanup(&dir);
}
