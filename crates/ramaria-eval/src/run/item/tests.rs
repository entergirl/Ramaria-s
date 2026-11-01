//! crates/ramaria-eval/src/run/item/tests.rs - Ramaria 评测逐题产物单元测试
//!
//! 设计特点:
//! - 成功与失败各一条：锁定字段齐全、`reply` / `recall` / `error` 的 null 语义
//! - 条件行由清单派生：语料 / 模型 / 嵌入 / 模板 / 设备逐项对齐
//! - 追加写锁定"一行一条 JSON 且以换行结尾"
//! - 夹具直接构造结构体，不读写真实评测产物

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::baseline::{BaselineManifest, QrelsRow};
use crate::run::{AnswerModel, AnswerModelKind, Condition, Pipeline};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-item-{tag}-{nanos}"))
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn manifest() -> BaselineManifest {
    let value = json!({
        "artifact_kind": "ramaria_eval_baseline",
        "schema_version": 1,
        "baseline_id": "lifebench-2.0-zh-u1-m1",
        "created_at": "2026-01-01T00:00:00Z",
        "corpus": {
            "name": "lifebench",
            "version": "2.0",
            "language": "zh",
            "file_name": "lifebench.json",
            "sha256": "corpus-sha",
            "size": 1,
            "qa_total": 1,
            "sample_ids": ["s1"]
        },
        "tier": {
            "id": "u1-m1",
            "users": 1,
            "memory_start": "2025-12-01",
            "memory_end": "2025-12-31",
            "query_end": "2026-01-15",
            "span_days": 31,
            "sample_ids": ["s1"]
        },
        "qa": { "total": 1, "in_window": 1 },
        "pipeline": {
            "crate_version": "0.0.0",
            "schema_version": 4,
            "prompt_template_version": "prompt-v1",
            "pipeline_version": "test",
            "code_fingerprint": "fingerprint"
        },
        "memory_model": {
            "kind": "local",
            "model": "memory-model",
            "temperature": 0.2,
            "max_tokens": 512
        },
        "embedding": { "fingerprint": "embed-fp", "dimension": 512 },
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
    });
    serde_json::from_value(value).expect("清单夹具应可构造")
}

fn item() -> RunItem {
    let row: QrelsRow = serde_json::from_value(json!({
        "question_id": "lifebench_v5_user-1_qa0",
        "sample_key": "user-1",
        "ask_time": "2025-12-02",
        "ask_time_ms": 1_765_000_000_000i64,
        "question": "问题",
        "answer": "答案",
        "category": [0, 2],
        "question_type": ["Single_hop"],
        "evidence": ["chat1"],
        "gold_message_ids": ["m1", "m2"],
        "score_points": [{ "description": "要点", "score": 10.0 }]
    }))
    .expect("qrels 夹具应可构造");
    RunItem {
        row,
        tier: "u1-m1".to_string(),
        persona_uid: "lifebench-user-1".to_string(),
    }
}

fn condition() -> Condition {
    Condition {
        id: "u1-m1/l1-l2/800/0".to_string(),
        tier: "u1-m1".to_string(),
        pipeline: Pipeline::L1L2,
        budget_chars: 800,
        answer_model: AnswerModel {
            kind: AnswerModelKind::Local,
            base_url: "http://127.0.0.1:1234/v1".to_string(),
            model: "answer-model".to_string(),
        },
    }
}

fn line() -> ConditionLine {
    ConditionLine::from_manifest(&manifest(), "zh", "ready")
}

fn outcome() -> ramaria_service::EvalChatOutcome {
    ramaria_service::EvalChatOutcome {
        reply: "回复".to_string(),
        session_id: Uuid::new_v4(),
        chars: 2,
        recall: ramaria_service::EvalRecallObservation::default(),
    }
}

// =========================================================
// 构造
// =========================================================

/// 成功行字段齐全，`error` 为 null、`recall` 为对象。
#[test]
fn success_record_keeps_all_fields() {
    let record = RunItemRecord::success(
        &item(),
        &condition(),
        outcome(),
        123,
        1,
        "answer-model",
        line(),
    );

    assert_eq!(record.question_id, "lifebench_v5_user-1_qa0");
    assert_eq!(record.condition_id, "u1-m1/l1-l2/800/0");
    assert_eq!(record.tier, "u1-m1");
    assert_eq!(record.pipeline, "l1-l2");
    assert_eq!(record.budget_chars, 800);
    assert_eq!(record.answer_model, "answer-model");
    assert_eq!(record.sample_key, "user-1");
    assert_eq!(record.persona_uid, "lifebench-user-1");
    assert_eq!(record.ask_time, "2025-12-02");
    assert_eq!(record.as_of_ms, 1_765_000_000_000);
    assert_eq!(record.question, "问题");
    assert_eq!(record.gold_answer, "答案");
    assert_eq!(record.gold_message_ids, vec!["m1", "m2"]);
    assert_eq!(record.category, vec![0, 2]);
    assert_eq!(record.reply.as_deref(), Some("回复"));
    assert_eq!(record.reply_chars, 2);
    assert!(record.recall.is_some());
    assert_eq!(record.elapsed_ms, 123);
    assert!(record.error.is_none());
    assert_eq!(record.attempts, 1);
    assert_eq!(record.conditions.index_status, "ready");

    let value = serde_json::to_value(&record).expect("应可序列化");
    assert!(value["error"].is_null(), "成功行 error 应为 null");
    assert!(value["reply"].is_string());
    assert!(value["recall"].is_object());
    let conditions = value["conditions"].as_object().expect("条件行应为对象");
    for key in [
        "corpus_name",
        "corpus_version",
        "language",
        "corpus_sha256",
        "memory_model",
        "memory_model_kind",
        "memory_temperature",
        "memory_max_tokens",
        "embedding_fingerprint",
        "embedding_dimension",
        "prompt_template_version",
        "schema_version",
        "index_status",
        "device_os",
        "device_arch",
        "device_cores",
    ] {
        assert!(conditions.contains_key(key), "条件行缺少字段 {key}");
    }

    let parsed: RunItemRecord = serde_json::from_value(value).expect("应可反序列化");
    assert_eq!(parsed, record);
}

/// 失败行 `reply` / `recall` 为 null、`error` 为字符串。
#[test]
fn failure_record_nulls_result_fields() {
    let record = RunItemRecord::failure(
        &item(),
        &condition(),
        "模型不可达",
        45,
        3,
        "answer-model",
        line(),
    );

    assert!(record.reply.is_none());
    assert_eq!(record.reply_chars, 0);
    assert!(record.recall.is_none());
    assert_eq!(record.error.as_deref(), Some("模型不可达"));
    assert_eq!(record.attempts, 3);

    let value = serde_json::to_value(&record).expect("应可序列化");
    assert!(value["reply"].is_null(), "失败行 reply 应为 null");
    assert!(value["recall"].is_null(), "失败行 recall 应为 null");
    assert!(value["error"].is_string());
    assert_eq!(value["conditions"]["index_status"], "ready");
}

// =========================================================
// 条件行
// =========================================================

/// 条件行由清单派生，语言与索引状态取调用方入参。
#[test]
fn condition_line_reads_manifest() {
    let line = ConditionLine::from_manifest(&manifest(), "en", "degraded");

    assert_eq!(line.corpus_name, "lifebench");
    assert_eq!(line.corpus_version, "2.0");
    assert_eq!(line.language, "en");
    assert_eq!(line.corpus_sha256, "corpus-sha");
    assert_eq!(line.memory_model, "memory-model");
    assert_eq!(line.memory_model_kind, "local");
    assert_eq!(line.memory_temperature, 0.2);
    assert_eq!(line.memory_max_tokens, 512);
    assert_eq!(line.embedding_fingerprint, "embed-fp");
    assert_eq!(line.embedding_dimension, Some(512));
    assert_eq!(line.prompt_template_version, "prompt-v1");
    assert_eq!(line.schema_version, 4);
    assert_eq!(line.index_status, "degraded");
    assert_eq!(line.device_os, std::env::consts::OS);
    assert_eq!(line.device_arch, std::env::consts::ARCH);
    assert_eq!(
        line.device_cores,
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(0)
    );
}

// =========================================================
// 追加写
// =========================================================

/// 追加写每行一条 JSON 并以换行结尾，可逐行解析回原结构。
#[test]
fn append_writes_one_line_per_record() {
    let dir = temp_dir("append");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let path = dir.join("items.jsonl");

    let success = RunItemRecord::success(
        &item(),
        &condition(),
        outcome(),
        10,
        1,
        "answer-model",
        line(),
    );
    let failure =
        RunItemRecord::failure(&item(), &condition(), "超时", 20, 2, "answer-model", line());
    append(&path, &success).expect("首次追加应成功");
    append(&path, &failure).expect("二次追加应成功");

    let text = std::fs::read_to_string(&path).expect("产物应可读");
    assert!(text.ends_with('\n'), "每行应以换行结尾");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "两行产物");

    let first: RunItemRecord = serde_json::from_str(lines[0]).expect("首行应可解析");
    let second: RunItemRecord = serde_json::from_str(lines[1]).expect("次行应可解析");
    assert_eq!(first, success);
    assert_eq!(second, failure);

    let value: Value = serde_json::from_str(lines[1]).expect("次行应为 JSON 对象");
    assert_eq!(value["attempts"], 2);

    // 运行状态侧按同一份产物判定续跑：成功 / 失败与尝试次数可还原。
    let keys = crate::run::state::scan_items(&path).expect("状态扫描应成功");
    assert_eq!(keys.len(), 2);
    assert!(keys[0].success);
    assert!(!keys[1].success);
    let attempts = crate::run::state::scan_attempts(&path).expect("尝试次数应可读");
    assert_eq!(
        attempts.get(&("u1-m1/l1-l2/800/0".to_string(), failure.question_id.clone())),
        Some(&2)
    );
    cleanup(&dir);
}
