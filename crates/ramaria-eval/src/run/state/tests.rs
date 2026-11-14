//! crates/ramaria-eval/src/run/state/tests.rs - Ramaria 评测运行状态单元测试
//!
//! 设计特点:
//! - 新建与重入各一条：新建即落盘，重入保留计数并把状态拨回运行中
//! - 矩阵比对锁定"换矩阵即拒绝复用旧目录"
//! - 计数重建锁定同一复合键以最后一行为准、完成与失败互斥
//! - 逐题扫描覆盖正常、空文件、缺失文件与坏行带行号

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::run::{AnswerModel, AnswerModelKind, Pipeline, RunSection};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-state-{tag}-{nanos}"))
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn matrix(budgets: Vec<u32>) -> RunMatrix {
    RunMatrix {
        run: RunSection {
            corpus: "lifebench".to_string(),
            language: "zh".to_string(),
            tiers: vec!["u1-m1".to_string()],
            pipelines: vec![Pipeline::Layered],
            budgets,
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
    }
}

fn tiers() -> Vec<RunTierRef> {
    vec![RunTierRef {
        id: "u1-m1".to_string(),
        dir: "baselines/u1-m1".to_string(),
        manifest_sha256: "deadbeef".to_string(),
        rows: 3,
    }]
}

fn totals() -> Vec<(String, usize)> {
    vec![("u1-m1/layered/800/0".to_string(), 3)]
}

fn read_state(dir: &Path) -> RunState {
    let text = std::fs::read_to_string(dir.join(RUN_FILE)).expect("状态文件应可读");
    serde_json::from_str(&text).expect("状态文件应可解析")
}

// =========================================================
// 新建与重入
// =========================================================

/// 新目录建立运行状态并立即落盘，字段取矩阵与入参。
#[test]
fn init_writes_running_state() {
    let dir = temp_dir("init");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let matrix = matrix(vec![800]);

    let state = RunState::load_or_init(&dir, &matrix, &tiers(), &totals()).expect("新建状态应成功");

    assert_eq!(state.artifact_kind, RUN_ARTIFACT_KIND);
    assert_eq!(state.schema_version, RUN_SCHEMA_VERSION);
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.language, "zh");
    assert_eq!(
        state.run_id,
        dir.file_name().and_then(|name| name.to_str()).unwrap(),
        "运行标识取目录名"
    );
    assert_eq!(state.created_at, state.updated_at);
    assert!(state.created_at.ends_with('Z'), "时间应为 UTC RFC3339");
    assert_eq!(state.conditions.len(), 1);
    assert_eq!(state.conditions[0].total, 3);
    assert_eq!(state.conditions[0].completed, 0);
    assert!(state.warnings.is_empty());

    let persisted = read_state(&dir);
    assert_eq!(persisted, state, "落盘内容应与返回值一致");
    cleanup(&dir);
}

/// 重入保留计数与档位引用，状态拨回运行中。
#[test]
fn reload_keeps_counts_and_resets_status() {
    let dir = temp_dir("reload");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let matrix = matrix(vec![800]);

    let mut first =
        RunState::load_or_init(&dir, &matrix, &tiers(), &totals()).expect("新建状态应成功");
    first.status = RunStatus::Completed;
    first.conditions[0].completed = 2;
    first.conditions[0].failed = 1;
    first.elapsed_ms = 1_234;
    first.warnings.push("索引未就绪".to_string());
    first.save(&dir).expect("状态应可写");

    let again = RunState::load_or_init(&dir, &matrix, &tiers(), &totals()).expect("重入应成功");
    assert_eq!(again.status, RunStatus::Running, "重入应回到运行中");
    assert_eq!(again.conditions[0].completed, 2);
    assert_eq!(again.conditions[0].failed, 1);
    assert_eq!(again.elapsed_ms, 1_234);
    assert_eq!(again.warnings.len(), 1);
    cleanup(&dir);
}

/// 矩阵变化时拒绝复用旧目录。
#[test]
fn reload_rejects_matrix_mismatch() {
    let dir = temp_dir("mismatch");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    RunState::load_or_init(&dir, &matrix(vec![800]), &tiers(), &totals()).expect("新建状态应成功");

    let error = RunState::load_or_init(&dir, &matrix(vec![1600]), &tiers(), &totals())
        .expect_err("矩阵不一致应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("矩阵"),
        "文案应说明矩阵不一致，实际 {error}"
    );
    assert!(
        error.to_string().contains("--output"),
        "文案应给出处置建议，实际 {error}"
    );
    cleanup(&dir);
}

// =========================================================
// 计数重建
// =========================================================

/// 同一复合键以最后一行为准，完成与失败互斥。
#[test]
fn recount_uses_last_record_per_key() {
    let dir = temp_dir("recount");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let totals = vec![("c1".to_string(), 2usize), ("c2".to_string(), 1usize)];
    let mut state = RunState::load_or_init(&dir, &matrix(vec![800]), &tiers(), &totals)
        .expect("新建状态应成功");

    state.recount(&[
        ("c1".to_string(), "q1".to_string(), true),
        ("c1".to_string(), "q1".to_string(), false),
        ("c1".to_string(), "q2".to_string(), true),
        ("c2".to_string(), "q3".to_string(), false),
    ]);

    assert_eq!(state.conditions[0].completed, 1, "q1 以最后一行为准");
    assert_eq!(state.conditions[0].failed, 1);
    assert_eq!(state.conditions[1].completed, 0);
    assert_eq!(state.conditions[1].failed, 1);

    // 同一键再写成功行：结果翻转为完成，不与失败重复计数。
    state.recount(&[("c1".to_string(), "q1".to_string(), true)]);
    assert_eq!(state.conditions[0].completed, 1);
    assert_eq!(state.conditions[0].failed, 0);
    assert_eq!(state.conditions[0].total, 2, "计数重建不改计划数");
    cleanup(&dir);
}

// =========================================================
// 逐题扫描
// =========================================================

/// 正常文件按顺序返回复合键与结果类别。
#[test]
fn scan_items_reads_rows_in_order() {
    let dir = temp_dir("scan");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let path = dir.join("items.jsonl");
    std::fs::write(
        &path,
        "{\"condition_id\":\"c1\",\"question_id\":\"q1\",\"error\":null}\n\
         {\"condition_id\":\"c1\",\"question_id\":\"q2\",\"error\":\"超时\"}\n\
         {\"condition_id\":\"c1\",\"question_id\":\"q3\",\"success\":false}\n",
    )
    .expect("夹具应可写");

    let keys = scan_items(&path).expect("扫描应成功");
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0].question_id, "q1");
    assert!(keys[0].success, "error 为空应视为成功");
    assert!(!keys[1].success, "error 非空应视为失败");
    assert!(!keys[2].success, "显式 success 优先");
    cleanup(&dir);
}

/// 空文件与缺失文件都视为没有已完成题目。
#[test]
fn scan_items_accepts_empty_and_missing() {
    let dir = temp_dir("empty");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let path = dir.join("items.jsonl");
    std::fs::write(&path, "").expect("空夹具应可写");

    assert!(scan_items(&path).expect("空文件应成功").is_empty());
    assert!(
        scan_items(&dir.join("missing.jsonl"))
            .expect("缺失文件应成功")
            .is_empty()
    );
    assert!(
        scan_attempts(&dir.join("missing.jsonl"))
            .expect("缺失文件应成功")
            .is_empty()
    );
    cleanup(&dir);
}

/// 坏行报校验错误并带行号。
#[test]
fn scan_items_reports_line_number() {
    let dir = temp_dir("bad");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let path = dir.join("items.jsonl");
    std::fs::write(
        &path,
        "{\"condition_id\":\"c1\",\"question_id\":\"q1\",\"error\":null}\n\
         {\"condition_id\":1}\n",
    )
    .expect("夹具应可写");

    let error = scan_items(&path).expect_err("坏行应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("第 2 行"),
        "文案应带行号，实际 {error}"
    );
    cleanup(&dir);
}

/// 尝试次数按同一键最后一行为准。
#[test]
fn scan_attempts_uses_last_row() {
    let dir = temp_dir("attempts");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let path = dir.join("items.jsonl");
    let rows = [
        json!({"condition_id": "c1", "question_id": "q1", "success": false, "attempts": 1}),
        json!({"condition_id": "c1", "question_id": "q1", "success": false, "attempts": 3}),
        json!({"condition_id": "c1", "question_id": "q2", "success": true, "attempts": 1}),
    ];
    let mut text = String::new();
    for row in rows {
        text.push_str(&serde_json::to_string(&row).expect("夹具应可序列化"));
        text.push('\n');
    }
    std::fs::write(&path, text).expect("夹具应可写");

    let attempts = scan_attempts(&path).expect("扫描应成功");
    assert_eq!(
        attempts.get(&("c1".to_string(), "q1".to_string())),
        Some(&3)
    );
    assert_eq!(
        attempts.get(&("c1".to_string(), "q2".to_string())),
        Some(&1)
    );
    cleanup(&dir);
}
