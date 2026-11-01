//! crates/ramaria-eval/src/run/plan/tests.rs - Ramaria 评测题目枚举单元测试
//!
//! 设计特点:
//! - 档位夹具写真实 manifest.json 与 qa.jsonl，覆盖成功与三类校验失败
//! - 题集保留不可回答题，顺序与文件一致
//! - 全局上限跨档位累计，截断只发生在题目边界
//! - persona 标识与语料样本的同名函数逐一对比，锁定口径

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_core::error::RamariaError;
use serde_json::{Value, json};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-plan-{tag}-{nanos}"))
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn tier_value(tier_id: &str) -> Value {
    json!({
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

fn row(sample_key: &str, question_id: &str, gold: &[&str]) -> QrelsRow {
    serde_json::from_value(json!({
        "question_id": question_id,
        "sample_key": sample_key,
        "ask_time": "2025-12-02",
        "ask_time_ms": 1_765_000_000_000i64,
        "question": "问题",
        "answer": "答案",
        "category": [0],
        "question_type": ["Single_hop"],
        "evidence": ["chat1"],
        "gold_message_ids": gold,
        "score_points": [{ "description": "要点", "score": 10.0 }]
    }))
    .expect("qrels 夹具应可构造")
}

fn write_rows(dir: &Path, rows: &[QrelsRow]) {
    let mut text = String::new();
    for row in rows {
        text.push_str(&serde_json::to_string(row).expect("qrels 应可序列化"));
        text.push('\n');
    }
    std::fs::write(dir.join(QRELS_FILE), text).expect("qrels 夹具应可写");
}

fn write_tier(root: &Path, tier_id: &str, rows: &[QrelsRow]) -> PathBuf {
    let dir = root.join(tier_id);
    std::fs::create_dir_all(&dir).expect("档位目录应可创建");
    let manifest = serde_json::to_string_pretty(&tier_value(tier_id)).expect("清单应可序列化");
    std::fs::write(dir.join(MANIFEST_FILE), manifest).expect("清单夹具应可写");
    write_rows(&dir, rows);
    dir
}

fn write_manifest(dir: &Path, value: &Value) {
    let text = serde_json::to_string_pretty(value).expect("清单应可序列化");
    std::fs::write(dir.join(MANIFEST_FILE), text).expect("清单夹具应可写");
}

// =========================================================
// 档位加载
// =========================================================

/// 合法档位返回清单与全部题目，且路径可直接生成基线产物。
#[test]
fn load_reads_manifest_and_rows() {
    let dir = temp_dir("load");
    let rows = vec![
        row("user-1", "lifebench_v5_user-1_qa0", &["m1", "m2"]),
        row("user-1", "lifebench_v5_user-1_qa1", &["m3"]),
    ];
    let tier_dir = write_tier(&dir, "u1-m1", &rows);

    let tier = BaselineTier::load(&tier_dir).expect("合法档位应可加载");
    assert_eq!(tier.manifest.tier.id, "u1-m1");
    assert_eq!(tier.rows.len(), 2);
    assert_eq!(tier.rows[0].question_id, "lifebench_v5_user-1_qa0");
    assert_eq!(tier.rows[0].gold_message_ids, vec!["m1", "m2"]);
    assert_eq!(tier.product().db, tier_dir.join("ramaria.db"));
    assert_eq!(tier.product().config, tier_dir.join("config.toml"));
    cleanup(&dir);
}

/// 产物类型不符时拒绝加载。
#[test]
fn load_rejects_wrong_artifact_kind() {
    let dir = temp_dir("kind");
    let tier_dir = write_tier(&dir, "u1-m1", &[row("u", "q", &["m"])]);
    let mut value = tier_value("u1-m1");
    value["artifact_kind"] = json!("other_artifact");
    write_manifest(&tier_dir, &value);

    let error = BaselineTier::load(&tier_dir).expect_err("产物类型不符应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains(ARTIFACT_KIND),
        "文案应含期望类型"
    );
    cleanup(&dir);
}

/// 清单版本不符时拒绝加载。
#[test]
fn load_rejects_wrong_schema_version() {
    let dir = temp_dir("version");
    let tier_dir = write_tier(&dir, "u1-m1", &[row("u", "q", &["m"])]);
    let mut value = tier_value("u1-m1");
    value["schema_version"] = json!(ARTIFACT_SCHEMA_VERSION + 1);
    write_manifest(&tier_dir, &value);

    let error = BaselineTier::load(&tier_dir).expect_err("版本不符应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(error.to_string().contains("版本不符"), "文案应说明版本不符");
    cleanup(&dir);
}

/// 目录名与清单档位不一致时拒绝加载。
#[test]
fn load_rejects_directory_tier_mismatch() {
    let dir = temp_dir("mismatch");
    let tier_dir = write_tier(&dir, "u1-m1", &[row("u", "q", &["m"])]);
    write_manifest(&tier_dir, &tier_value("u1-q1"));

    let error = BaselineTier::load(&tier_dir).expect_err("目录名不一致应报错");
    assert!(
        error.to_string().contains("目录名与清单不一致"),
        "文案应说明目录名不一致，实际 {error}"
    );
    cleanup(&dir);
}

/// qrels 行不可解析或为空行时拒绝加载。
#[test]
fn load_rejects_malformed_rows() {
    let dir = temp_dir("rows");
    let tier_dir = write_tier(&dir, "u1-m1", &[row("u", "q", &["m"])]);
    std::fs::write(tier_dir.join(QRELS_FILE), "{\"question_id\": 1}\n\n").expect("坏夹具应可写");

    let error = BaselineTier::load(&tier_dir).expect_err("坏行应报错");
    assert!(
        matches!(&error, RamariaError::Serialization { .. }),
        "应为 Serialization，实际 {error:?}"
    );
    assert!(error.to_string().contains("第 1 行"), "文案应含行号");
    cleanup(&dir);
}

// =========================================================
// 题目展开
// =========================================================

/// 题目按档位顺序展开，全局上限跨档位累计。
#[test]
fn plan_items_keeps_tier_order_and_applies_limit() {
    let dir = temp_dir("plan");
    let first = write_tier(
        &dir,
        "u1-m1",
        &[row("user-1", "q0", &["m1"]), row("user-1", "q1", &[])],
    );
    let second = write_tier(
        &dir,
        "u1-q1",
        &[row("user-1", "q2", &["m2"]), row("user-1", "q3", &["m3"])],
    );
    let tiers = vec![
        BaselineTier::load(&first).expect("首档应可加载"),
        BaselineTier::load(&second).expect("次档应可加载"),
    ];

    let all = plan_items(&tiers, None);
    let ids: Vec<&str> = all
        .iter()
        .map(|item| item.row.question_id.as_str())
        .collect();
    assert_eq!(ids, vec!["q0", "q1", "q2", "q3"]);
    assert!(all[1].row.gold_message_ids.is_empty(), "不可回答题应保留");
    let tier_ids: Vec<&str> = all.iter().map(|item| item.tier.as_str()).collect();
    assert_eq!(
        tier_ids,
        vec!["u1-m1", "u1-m1", "u1-q1", "u1-q1"],
        "题目应带上所属档位标识"
    );

    let limited = plan_items(&tiers, Some(3));
    let ids: Vec<&str> = limited
        .iter()
        .map(|item| item.row.question_id.as_str())
        .collect();
    assert_eq!(ids, vec!["q0", "q1", "q2"], "上限应跨档位累计");
    assert!(plan_items(&tiers, Some(0)).is_empty());
    cleanup(&dir);
}

/// persona 标识由 sample_key 推导，与语料样本口径一致。
#[test]
fn plan_items_persona_uid_matches_sample() {
    let dir = temp_dir("persona");
    let tier_dir = write_tier(&dir, "u1-m1", &[row("user-1", "q0", &["m1"])]);
    let tier = BaselineTier::load(&tier_dir).expect("档位应可加载");
    let items = plan_items(&[tier], None);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].persona_uid, "lifebench-user-1");

    let sample = crate::corpus::Sample {
        sample_id: "sample".to_string(),
        user_key: "user-1".to_string(),
        speaker_a: "甲".to_string(),
        speaker_b: "乙".to_string(),
        sessions: Vec::new(),
        qa: Vec::new(),
    };
    assert_eq!(
        items[0].persona_uid,
        sample.persona_uid(),
        "题目枚举与语料的 persona 口径应一致"
    );
    cleanup(&dir);
}
