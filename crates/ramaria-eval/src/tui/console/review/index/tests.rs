//! crates/ramaria-eval/src/tui/console/review/index/tests.rs - Ramaria 审查索引测试
//!
//! 设计特点:
//! - 夹具写真实 `run.json` / `items.jsonl` / `scored.jsonl`，只走公开构建入口
//! - 500 条题单验证按偏移读取与条件分组，不读完整文件
//! - 同键多行只保留最后一次写出，顺序按最后出现位置
//! - 坏行与非运行目录都要给出带定位的错误

use super::*;

use std::path::Path;

use serde_json::json;

use crate::run::{ConditionLine, RunItemRecord};

// =========================================================
// 夹具
// =========================================================

fn temp_root(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let root = std::env::temp_dir().join(format!("ramaria-review-index-{tag}-{nanos}"));
    std::fs::create_dir_all(&root).expect("运行根应可创建");
    root
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

/// 最小可解析的 `run.json`。
fn write_run(root: &Path, conditions: &[(&str, usize)]) {
    let body = json!({
        "artifact_kind": "ramaria_eval_run",
        "schema_version": 2,
        "run_id": "run-test",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:00Z",
        "status": "completed",
        "matrix": {
            "run": {
                "corpus": "lifebench",
                "language": "zh",
                "tiers": ["u1"],
                "pipelines": ["l1"],
                "budgets": [800]
            },
            "answer_models": []
        },
        "language": "zh",
        "tiers": [],
        "conditions": conditions
            .iter()
            .map(|(id, total)| json!({ "id": id, "total": total, "completed": total, "failed": 0 }))
            .collect::<Vec<_>>(),
        "elapsed_ms": 0,
        "warnings": []
    });
    std::fs::write(root.join(RUN_FILE), body.to_string()).expect("运行状态应可写");
}

fn condition_line() -> ConditionLine {
    ConditionLine {
        corpus_name: "lifebench".to_string(),
        corpus_version: "2.0".to_string(),
        language: "zh".to_string(),
        corpus_sha256: String::new(),
        memory_model: "local".to_string(),
        memory_model_kind: "local".to_string(),
        memory_temperature: 0.0,
        memory_max_tokens: 0,
        embedding_fingerprint: String::new(),
        embedding_dimension: None,
        prompt_template_version: String::new(),
        schema_version: 1,
        index_status: "ready".to_string(),
        device_os: "windows".to_string(),
        device_arch: "x86_64".to_string(),
        device_cores: 1,
    }
}

fn record(condition: &str, question_id: &str, question: &str) -> RunItemRecord {
    RunItemRecord {
        question_id: question_id.to_string(),
        condition_id: condition.to_string(),
        tier: "u1".to_string(),
        pipeline: "l1".to_string(),
        budget_chars: 800,
        answer_model: "local".to_string(),
        answer_model_effective: "local".to_string(),
        sample_key: "alice".to_string(),
        persona_uid: "lifebench-alice".to_string(),
        ask_time: "2025-01-02".to_string(),
        as_of_ms: 0,
        question: question.to_string(),
        gold_answer: "gold".to_string(),
        gold_message_ids: Vec::new(),
        evidence: Vec::new(),
        question_type: Vec::new(),
        score_points: Vec::new(),
        category: vec![0],
        reply: Some("reply".to_string()),
        reply_chars: 5,
        recall: None,
        preset_chars: None,
        usage: None,
        elapsed_ms: 10,
        error: None,
        attempts: 1,
        conditions: condition_line(),
    }
}

/// 追加一条逐题产物。
fn append_item(root: &Path, record: &RunItemRecord) {
    let line = serde_json::to_string(record).expect("逐题产物应可序列化");
    let path = root.join(ITEMS_FILE);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str(&line);
    text.push('\n');
    std::fs::write(path, text).expect("逐题产物应可写");
}

// =========================================================
// 用例
// =========================================================

/// 500 条题单全部入索引，条件分组与按偏移读行都正确。
#[test]
fn index_reads_five_hundred_items_by_offset() {
    let root = temp_root("500");
    write_run(&root, &[("c1", 250), ("c2", 250)]);
    for number in 0..500 {
        let condition = if number < 250 { "c1" } else { "c2" };
        let record = record(condition, &format!("q{number}"), "问题正文");
        append_item(&root, &record);
    }

    let index = ReviewIndex::build(&root).expect("索引应可建立");
    assert_eq!(index.items.len(), 500);
    assert_eq!(index.questions("c1").len(), 250);
    assert_eq!(index.questions("c2").len(), 250);
    assert!(!index.has_scored_file());

    let item = index.read_item(250).expect("第 251 条应可读");
    assert_eq!(item.question_id, "q250");
    assert_eq!(item.condition_id, "c2");
    assert!(index.read_scored(0).expect("判分缺失不算错误").is_none());
    cleanup(&root);
}

/// 同一复合键后行覆盖前行，题单顺序按最后一次出现。
#[test]
fn duplicate_key_keeps_last_line() {
    let root = temp_root("dup");
    write_run(&root, &[("c1", 2)]);
    append_item(&root, &record("c1", "q0", "第一版"));
    append_item(&root, &record("c1", "q1", "问题一"));
    append_item(&root, &record("c1", "q0", "第二版\"引号\"与\\反斜杠"));

    let index = ReviewIndex::build(&root).expect("索引应可建立");
    assert_eq!(index.items.len(), 2);
    assert_eq!(index.item(0).expect("第 0 条").question_id, "q1");
    assert_eq!(index.item(1).expect("第 1 条").question_id, "q0");
    let item = index.read_item(1).expect("覆盖后的行应可读");
    assert_eq!(item.question, "第二版\"引号\"与\\反斜杠");
    cleanup(&root);
}

/// 坏行报行号。
#[test]
fn broken_line_reports_line_number() {
    let root = temp_root("broken");
    write_run(&root, &[("c1", 1)]);
    append_item(&root, &record("c1", "q0", "问题"));
    std::fs::write(root.join(ITEMS_FILE), "{\"question_id\":\"q1\"}\n").expect("坏行应可写");

    let error = ReviewIndex::build(&root).expect_err("坏行应拒绝");
    let message = error.to_string();
    assert!(message.contains("第 1 行"), "错误应带行号: {message}");
    cleanup(&root);
}

/// 读判分产物：命中时返回逐题判分，行偏移可复用。
#[test]
fn scored_index_reads_one_item() {
    let root = temp_root("scored");
    write_run(&root, &[("c1", 1)]);
    append_item(&root, &record("c1", "q0", "问题"));
    let scored = json!({
        "question_id": "q0",
        "condition_id": "c1",
        "tier": "u1",
        "pipeline": "l1",
        "budget_chars": 800,
        "answer_model": "local",
        "sample_key": "alice",
        "category": [0],
        "gold_count": 1,
        "retrieval": null,
        "credibility": {
            "ua_refusal_hint": null,
            "cd_score_points": 0,
            "ua_judge": null,
            "cd_judge": null,
            "judge_failed": false
        },
        "rubric": null,
        "persona": null,
        "notes": []
    });
    let dir = root.join(SCORE_DIR);
    std::fs::create_dir_all(&dir).expect("判分目录应可创建");
    std::fs::write(
        dir.join(SCORED_FILE),
        format!(
            "{}\n",
            serde_json::to_string(&scored).expect("判分行应可序列化")
        ),
    )
    .expect("判分行应可写");

    let index = ReviewIndex::build(&root).expect("索引应可建立");
    assert!(index.has_scored_file());
    let scored = index
        .read_scored(0)
        .expect("判分行应可读")
        .expect("应命中判分");
    assert_eq!(scored.question_id, "q0");
    cleanup(&root);
}

/// 判分标记认条件汇总或逐题判分行任一存在的产物。
#[test]
fn score_present_accepts_either_artifact() {
    let root = temp_root("score-flag");
    assert!(!score_present(&root));
    let dir = root.join(SCORE_DIR);
    std::fs::create_dir_all(&dir).expect("判分目录应可创建");
    std::fs::write(dir.join(SCORED_FILE), "").expect("判分行应可写");
    assert!(score_present(&root), "只有 scored.jsonl 也算已判分");
    std::fs::write(dir.join(SCORE_FILE), "{}").expect("条件汇总应可写");
    assert!(score_present(&root));
    cleanup(&root);
}

/// 偏移对应的行被截断时给出路径与 offset。
#[test]
fn read_item_after_truncation_reports_offset() {
    let root = temp_root("truncated");
    write_run(&root, &[("c1", 1)]);
    append_item(&root, &record("c1", "q0", "问题"));
    let index = ReviewIndex::build(&root).expect("索引应可建立");
    std::fs::write(root.join(ITEMS_FILE), "").expect("产物应可截断");

    let error = index.read_item(0).expect_err("读不到应报错");
    let message = error.to_string();
    assert!(message.contains("offset"), "错误应带偏移: {message}");
    assert!(message.contains("items.jsonl"), "错误应带路径: {message}");
    cleanup(&root);
}

/// 非运行目录与错误产物类型都要拒绝。
#[test]
fn build_rejects_non_run_directory() {
    let root = temp_root("nonrun");
    let error = ReviewIndex::build(&root).expect_err("缺 run.json 应拒绝");
    assert!(
        error.to_string().contains("run.json"),
        "错误应带路径: {error}"
    );

    let body = json!({
        "artifact_kind": "other",
        "schema_version": 2,
        "run_id": "run-test",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:00Z",
        "status": "completed",
        "matrix": {
            "run": {
                "corpus": "lifebench",
                "language": "zh",
                "tiers": ["u1"],
                "pipelines": [],
                "budgets": []
            },
            "answer_models": []
        },
        "language": "zh",
        "tiers": [],
        "conditions": [],
        "elapsed_ms": 0,
        "warnings": []
    });
    std::fs::write(root.join(RUN_FILE), body.to_string()).expect("运行状态应可写");
    let error = ReviewIndex::build(&root).expect_err("错误类型应拒绝");
    assert!(
        error.to_string().contains("不是评测运行产物"),
        "错误应说明类型: {error}"
    );
    cleanup(&root);
}
