//! crates/ramaria-eval/src/run/runner/tests.rs - Ramaria 评测运行器单元测试
//!
//! 设计特点:
//! - 断点判定覆盖五类输入：未跑、已完成、失败未超限、失败超限、无尝试记录
//! - 计划计数按档位聚合，锁定条件只跑本题集档位
//! - 无待跑题的条件只发结束事件：计数断言覆盖，装配跳过由代码审查覆盖
//! - 报告摘要的收尾状态单独断言，确认中断不会渲染成完成
//! - 运行根与矩阵提示为纯函数断言，不读写 `.eval/` 产物

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ramaria_core::error::RamariaError;

use crate::baseline::QrelsRow;
use crate::run::{AnswerModel, AnswerModelKind, Condition, ConditionState, Pipeline, RunSection};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn key(condition: &str, question: &str) -> (String, String) {
    (condition.to_string(), question.to_string())
}

fn item(tier: &str, question_id: &str) -> RunItem {
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
        tier: tier.to_string(),
        persona_uid: "lifebench-user-1".to_string(),
    }
}

// =========================================================
// 断点判定
// =========================================================

/// 未跑过的题不跳过。
#[test]
fn should_skip_keeps_unrun_item() {
    assert!(!should_skip(
        &HashSet::new(),
        &HashMap::new(),
        &key("c1", "q1"),
        2
    ));
}

/// 已完成的题跳过。
#[test]
fn should_skip_drops_completed_item() {
    let mut done = HashSet::new();
    done.insert(key("c1", "q1"));
    assert!(should_skip(&done, &HashMap::new(), &key("c1", "q1"), 2));
}

/// 失败但未超限的题继续重跑。
#[test]
fn should_skip_keeps_failed_within_limit() {
    let mut attempts = HashMap::new();
    attempts.insert(key("c1", "q1"), 2);
    assert!(!should_skip(
        &HashSet::new(),
        &attempts,
        &key("c1", "q1"),
        2
    ));
}

/// 失败且尝试次数超过上限的题跳过。
#[test]
fn should_skip_drops_failed_over_limit() {
    let mut attempts = HashMap::new();
    attempts.insert(key("c1", "q1"), 3);
    assert!(should_skip(&HashSet::new(), &attempts, &key("c1", "q1"), 2));
}

/// 无尝试记录（空表或他人条目）不跳过。
#[test]
fn should_skip_ignores_missing_attempts() {
    let mut attempts = HashMap::new();
    attempts.insert(key("c2", "q9"), 5);
    assert!(!should_skip(
        &HashSet::new(),
        &attempts,
        &key("c1", "q1"),
        2
    ));
}

// =========================================================
// 计划计数
// =========================================================

/// 每个条件的计划数取本题集档位内的题目数。
#[test]
fn condition_totals_follow_tier_items() {
    let conditions = vec![
        Condition {
            id: "u1-m1/layered/800/0".to_string(),
            tier: "u1-m1".to_string(),
            pipeline: Pipeline::Layered,
            budget_chars: 800,
            answer_model: AnswerModel {
                kind: AnswerModelKind::Local,
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                model: "m".to_string(),
            },
        },
        Condition {
            id: "u1-q1/layered/800/0".to_string(),
            tier: "u1-q1".to_string(),
            pipeline: Pipeline::Layered,
            budget_chars: 800,
            answer_model: AnswerModel {
                kind: AnswerModelKind::Local,
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                model: "m".to_string(),
            },
        },
    ];
    let items = vec![
        item("u1-m1", "q1"),
        item("u1-m1", "q2"),
        item("u1-q1", "q3"),
    ];

    let totals = condition_totals(&conditions, &items);
    assert_eq!(totals[0], ("u1-m1/layered/800/0".to_string(), 2));
    assert_eq!(totals[1], ("u1-q1/layered/800/0".to_string(), 1));
}

/// 矩阵缺失提示带期望路径与必需字段。
#[test]
fn matrix_missing_message_names_path_and_fields() {
    let message = matrix_missing(Path::new(".eval/matrix.toml"));
    assert!(message.contains(".eval/matrix.toml"));
    assert!(message.contains("corpus"));
    assert!(message.contains("answer_models"));
}

/// 未给输出路径时运行根落在 `.eval/runs/` 下。
#[test]
fn run_root_defaults_under_runs() {
    let cwd = Path::new("workspace");
    let root = run_root(cwd, None);
    assert_eq!(root.parent(), Some(cwd.join(".eval/runs").as_path()));
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("运行根应有目录名");
    assert!(
        name.starts_with("run-"),
        "缺省运行标识应带前缀，实际 {name}"
    );

    let explicit = cwd.join("custom");
    assert_eq!(run_root(cwd, Some(explicit.clone())), explicit);
}

/// 运行入口在矩阵缺失时返回校验错误（不触碰网络与引擎）。
#[tokio::test]
async fn run_rejects_missing_matrix() {
    let dir = std::env::temp_dir().join(format!("ramaria-eval-runner-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let missing = dir.join("missing-matrix.toml");

    let error = run(None, Some(missing.clone()), None)
        .await
        .expect_err("矩阵缺失应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(error.to_string().contains("missing-matrix.toml"));
    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 报告摘要
// =========================================================

/// 报告摘要带收尾状态：中断与完成可区分，计数行保持不变。
#[test]
fn report_render_shows_run_status() {
    let report = RunReport {
        run_id: "run-test".to_string(),
        root: PathBuf::from(".eval/runs/run-test"),
        total: 10,
        completed: 7,
        failed: 1,
        elapsed_ms: 1234,
        status: RunStatus::Aborted,
        warnings: Vec::new(),
    };
    let aborted = report.render();
    assert!(
        aborted.contains("运行 run-test 结束"),
        "标题应使用中性收尾词，实际:\n{aborted}"
    );
    assert!(
        aborted.contains("状态: 中断"),
        "中断运行应显示中断状态，实际:\n{aborted}"
    );
    assert!(
        aborted.contains("完成 7 / 失败 1 / 计划 10"),
        "计数行应保持不变，实际:\n{aborted}"
    );

    let completed = RunReport {
        status: RunStatus::Completed,
        ..report
    };
    assert!(
        completed.render().contains("状态: 完成"),
        "跑完的运行应显示完成状态"
    );
}

// =========================================================
// 无待跑题的条件
// =========================================================

/// 只有断点判定和状态两列的最小状态（不落盘）。
fn state_with(condition: &str, total: usize, completed: usize, failed: usize) -> RunState {
    RunState {
        artifact_kind: "ramaria_eval_run".to_string(),
        schema_version: 2,
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
                retry_limit: 1,
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
            id: condition.to_string(),
            total,
            completed,
            failed,
        }],
        elapsed_ms: 0,
        warnings: Vec::new(),
    }
}

/// 条件无待跑题时收口：ConditionDone 带的是状态里的当前计数。
#[test]
fn condition_done_event_carries_current_counts() {
    let mut state = state_with("c1", 4, 3, 1);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let control = RunControl::new(sender);

    emit_condition_done(&mut state, &control, "c1");

    match receiver.try_recv() {
        Ok(RunEvent::ConditionDone {
            id,
            completed,
            failed,
        }) => {
            assert_eq!(id, "c1");
            assert_eq!(completed, 3, "完成数应取状态当前值");
            assert_eq!(failed, 1, "失败数应取状态当前值");
        }
        _ => panic!("应收到 ConditionDone 事件"),
    }
}
