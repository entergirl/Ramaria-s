//! crates/ramaria-eval/src/tui/console/tests.rs - Ramaria 评测台状态与渲染测试
//!
//! 设计特点:
//! - 夹具写真实矩阵与档位产物，预览走与运行器相同的加载入口
//! - 状态迁移优先：一个用例覆盖首页、矩阵与运行屏的连续按键
//! - 运行事件用合成结构，不开库、不启动引擎
//! - 渲染断言走 TestBackend，只比对去掉空白后的可见文本
//! - 历史均速用合成 run.json，明确验证中位数与 oracle 子目录不参与

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use crate::baseline::{ARTIFACT_KIND, ARTIFACT_SCHEMA_VERSION, MANIFEST_FILE, QRELS_FILE};
use crate::run::{
    ConditionPlan, ItemEvent, RunControl, RunEvent, RunReport, RunStarted, RunStatus,
};
use crate::sandbox::SandboxBackendChoice;

use super::UiEvent;
use super::matrix::history_pace;
use super::render;
use super::state::{Console, ConsoleAction, HomeEntry, Screen};

// =========================================================
// 夹具
// =========================================================

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let root = std::env::temp_dir().join(format!("ramaria-eval-console-{tag}-{nanos}"));
    std::fs::create_dir_all(root.join(".eval")).expect("评测根应可创建");
    root
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// 100x30 的测试终端上绘制一帧并取回可见文本。
fn render_to_text(console: &Console) -> String {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("测试终端应可创建");
    terminal
        .draw(|frame| render::draw(frame, console))
        .expect("渲染应成功");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

/// 去掉全部空白，避免换行与边距影响断言。
fn compact(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// 合法的矩阵定义（本地模型、单一档位、四条件）。
fn matrix_body(tier: &str) -> String {
    format!(
        r#"[run]
corpus = "lifebench"
language = "zh"
tiers = ["{tier}"]
pipelines = ["layered", "l1"]
budgets = [800, 1600]
question_limit = 5
strict_isolation = false
retry_limit = 1

[[answer_models]]
kind = "local"
base_url = "http://127.0.0.1:1234/v1"
model = "local-model"
"#
    )
}

fn write_matrix(root: &Path, name: &str, tier: &str) {
    std::fs::write(root.join(".eval").join(name), matrix_body(tier)).expect("矩阵夹具应可写");
}

fn manifest(tier: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "artifact_kind": ARTIFACT_KIND,
        "schema_version": ARTIFACT_SCHEMA_VERSION,
        "baseline_id": format!("lifebench-2.0-zh-{tier}"),
        "created_at": "2026-01-01T00:00:00Z",
        "corpus": {
            "name": "lifebench",
            "version": "2.0",
            "language": "zh",
            "file_name": "lifebench.json",
            "sha256": "deadbeef",
            "size": 1,
            "qa_total": 8,
            "sample_ids": ["s1"]
        },
        "tier": {
            "id": tier,
            "users": 1,
            "memory_start": "2025-12-01",
            "memory_end": "2025-12-31",
            "query_end": "2026-01-15",
            "span_days": 31,
            "sample_ids": ["s1"]
        },
        "qa": { "total": 8, "in_window": 8 },
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
    }))
    .expect("清单应可序列化")
}

/// 写一个档位产物：清单 + 指定行数的 qrels。
fn write_tier(root: &Path, tier: &str, rows: usize) {
    let dir = root.join(".eval/baselines/lifebench-2.0-zh").join(tier);
    std::fs::create_dir_all(&dir).expect("档位目录应可创建");
    std::fs::write(dir.join(MANIFEST_FILE), manifest(tier)).expect("清单夹具应可写");
    let mut text = String::new();
    for index in 0..rows {
        let row = json!({
            "question_id": format!("q{index}"),
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
        });
        text.push_str(&serde_json::to_string(&row).expect("qrels 应可序列化"));
        text.push('\n');
    }
    std::fs::write(dir.join(QRELS_FILE), text).expect("qrels 夹具应可写");
}

/// 一个历史运行的 run.json：
fn write_run(runs: &Path, name: &str, elapsed_ms: u64, completed: usize, failed: usize) {
    let dir = runs.join(name);
    std::fs::create_dir_all(&dir).expect("运行目录应可创建");
    let body = json!({
        "elapsed_ms": elapsed_ms,
        "conditions": [{ "completed": completed, "failed": failed }]
    });
    std::fs::write(dir.join("run.json"), body.to_string()).expect("运行状态夹具应可写");
}

fn started() -> RunStarted {
    RunStarted {
        run_id: "run-test".to_string(),
        root: PathBuf::from(".eval/runs/run-test"),
        total_conditions: 2,
        total_items: 4,
        total_executions: 4,
        conditions: vec![
            ConditionPlan {
                id: "c1".to_string(),
                total: 2,
                completed: 0,
                failed: 0,
            },
            ConditionPlan {
                id: "c2".to_string(),
                total: 2,
                completed: 0,
                failed: 0,
            },
        ],
    }
}

fn item(
    condition: &str,
    question: &str,
    success: bool,
    completed: usize,
    failed: usize,
) -> ItemEvent {
    ItemEvent {
        condition_id: condition.to_string(),
        question_id: question.to_string(),
        success,
        elapsed_ms: 120,
        attempts: 1,
        error_kind: (!success).then(|| "llm".to_string()),
        completed,
        failed,
        total: 2,
    }
}

fn report(status: RunStatus) -> RunReport {
    RunReport {
        run_id: "run-test".to_string(),
        root: PathBuf::from(".eval/runs/run-test"),
        total: 4,
        completed: 2,
        failed: 0,
        elapsed_ms: 400,
        status,
        warnings: Vec::new(),
    }
}

// =========================================================
// 首页
// =========================================================

/// 首页默认高亮沙盒，Enter 进入沙盒屏；首页按 q 退出进程。
#[test]
fn home_enters_sandbox_and_quits() {
    let root = temp_root("home");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    assert_eq!(console.screen, Screen::Home);
    assert_eq!(console.home, HomeEntry::Sandbox);

    assert!(matches!(
        console.handle_key(key(KeyCode::Char('q'))),
        ConsoleAction::Exit
    ));

    assert!(matches!(
        console.handle_key(key(KeyCode::Enter)),
        ConsoleAction::None
    ));
    assert_eq!(console.screen, Screen::Sandbox);
    cleanup(&root);
}

/// 沙盒的退出动作映射为返回首页，不退出进程。
#[test]
fn sandbox_quit_returns_home() {
    let root = temp_root("sandbox-back");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    console.handle_key(key(KeyCode::Enter));
    assert_eq!(console.screen, Screen::Sandbox);

    // 后端屏 Esc 在沙盒内是退出动作
    let action = console.handle_key(key(KeyCode::Esc));
    assert!(matches!(action, ConsoleAction::None));
    assert_eq!(console.screen, Screen::Home);
    cleanup(&root);
}

// =========================================================
// 矩阵
// =========================================================

/// 首页进入矩阵屏：扫描到矩阵并按运行器口径预览条件数与计划题数。
#[test]
fn matrix_screen_previews_plan_counts() {
    let root = temp_root("matrix");
    write_matrix(&root, "matrix.toml", "u1-m1");
    write_tier(&root, "u1-m1", 8);

    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    console.handle_key(key(KeyCode::Down));
    console.handle_key(key(KeyCode::Enter));
    assert_eq!(console.screen, Screen::Matrix);
    assert_eq!(console.matrix.entries.len(), 1, "应扫描到单个矩阵");

    let preview = console.matrix.preview.as_ref().expect("应生成预览");
    assert!(
        preview.error.is_none(),
        "合法矩阵不应有错误: {:?}",
        preview.error
    );
    let counts = preview.counts.as_ref().expect("档位齐全时应有计划规模");
    assert_eq!(counts.conditions, 4, "一档 × 两管线 × 两预算 × 一模型");
    assert_eq!(counts.questions, 5, "题数应受全局上限约束");
    assert_eq!(counts.executions, 20, "计划条目为题数乘条件数");
    assert_eq!(preview.pace.runs, 0, "没有历史运行时应取粗估");
    assert!(preview.eta_ms().is_some());
    cleanup(&root);
}

/// 档位缺失时预览带错误并禁止启动。
#[test]
fn matrix_missing_tier_blocks_start() {
    let root = temp_root("matrix-missing");
    write_matrix(&root, "matrix.toml", "u9-x");

    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    console.handle_key(key(KeyCode::Down));
    console.handle_key(key(KeyCode::Enter));
    assert_eq!(console.screen, Screen::Matrix);

    let preview = console.matrix.preview.as_ref().expect("矩阵本身应可解析");
    assert!(preview.error.is_some(), "缺档位应带错误");
    assert!(preview.counts.is_none(), "缺档位时不应给出计划规模");
    assert!(
        console.matrix.start_request(&root).is_none(),
        "缺档位时 Enter 不应启动"
    );
    assert!(matches!(
        console.handle_key(key(KeyCode::Enter)),
        ConsoleAction::None
    ));
    cleanup(&root);
}

/// 历史均速取中位数，零条目的运行与 oracle 子目录都不参与。
#[test]
fn history_pace_takes_median() {
    let root = temp_root("pace");
    let runs = root.join(".eval/runs");
    write_run(&runs, "fast", 2_000, 4, 0);
    write_run(&runs, "slow", 3_000, 2, 0);
    write_run(&runs, "empty", 9_000, 0, 0);
    // oracle 子目录的 run.json 不在运行根一级目录，不应被读到
    let oracle = runs.join("fast/oracle");
    std::fs::create_dir_all(&oracle).expect("oracle 目录应可创建");
    std::fs::write(
        oracle.join("run.json"),
        json!({ "elapsed_ms": 90_000, "conditions": [{ "completed": 1, "failed": 0 }] })
            .to_string(),
    )
    .expect("oracle 夹具应可写");

    let pace = history_pace(&runs);
    assert_eq!(pace.runs, 2, "只有两次运行有定案条目");
    assert!(
        (pace.per_item_ms - 1_000.0).abs() < 1e-9,
        "均速应为 500ms 与 1500ms 的中位数，实际 {}",
        pace.per_item_ms
    );
    assert!(pace.label().contains("按历史 2 次运行"), "文案应带样本数");
    cleanup(&root);
}

// =========================================================
// 运行中
// =========================================================

/// 运行事件驱动计数、事件尾、告警数与剩余估计。
#[test]
fn running_events_update_progress() {
    let root = temp_root("running-events");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    let output = root.join(".eval/runs/run-test");
    console.begin_run(
        root.join(".eval/matrix.toml"),
        output.clone(),
        RunControl::default(),
    );
    assert_eq!(console.screen, Screen::Running);
    assert_eq!(console.running.status_text(), "运行中");

    console.apply_run_event(RunEvent::Started(started()));
    assert_eq!(console.running.total_executions(), 4);
    assert_eq!(console.running.total_items(), 4);

    console.apply_run_event(RunEvent::ConditionStarted {
        index: 0,
        total: 2,
        id: "c1".to_string(),
        items: 2,
    });
    console.apply_run_event(RunEvent::ItemDone(item("c1", "q0", true, 1, 0)));
    console.apply_run_event(RunEvent::ItemDone(item("c1", "q1", false, 1, 1)));
    console.apply_run_event(RunEvent::Warning {
        condition: Some("c1".to_string()),
        message: "索引未就绪".to_string(),
    });
    console.apply_run_event(RunEvent::ConditionDone {
        id: "c1".to_string(),
        completed: 1,
        failed: 1,
    });

    assert_eq!(console.running.completed(), 1);
    assert_eq!(console.running.failed(), 1);
    assert_eq!(console.running.done_total(), 2);
    assert_eq!(console.running.warnings(), 1);
    assert!(
        console.running.tail().count() <= super::running::TAIL_LIMIT,
        "事件尾不应超过上限"
    );
    assert!(
        console.running.tail().any(|line| line.contains("q1")),
        "事件尾应包含逐题结果"
    );
    assert!(console.running.eta().is_some(), "已定案条目应能估算剩余");
    cleanup(&root);
}

/// 续跑时 Started 带出上次进度：完成数、已定案数与本题待跑数按初始计数还原。
#[test]
fn started_restores_previous_progress() {
    let root = temp_root("running-resume");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    console.begin_run(
        root.join(".eval/matrix.toml"),
        root.join(".eval/runs/run-test"),
        RunControl::default(),
    );
    console.apply_run_event(RunEvent::Started(RunStarted {
        run_id: "run-test".to_string(),
        root: PathBuf::from(".eval/runs/run-test"),
        total_conditions: 2,
        total_items: 4,
        total_executions: 4,
        conditions: vec![
            ConditionPlan {
                id: "c1".to_string(),
                total: 2,
                completed: 1,
                failed: 0,
            },
            ConditionPlan {
                id: "c2".to_string(),
                total: 2,
                completed: 0,
                failed: 0,
            },
        ],
    }));

    assert_eq!(console.running.completed(), 1, "完成数应取续跑初始值");
    assert_eq!(console.running.done_total(), 1, "已定案数应取续跑初始值");
    assert_eq!(
        console.running.conditions()[0].pending,
        1,
        "本次待跑数应为计划题数减初始完成与失败"
    );
    cleanup(&root);
}

/// 取消只置位开关；结束后 R 用同一矩阵与输出根续跑，Enter 进入审查。
#[test]
fn finished_run_reruns_same_output_root() {
    let root = temp_root("running-rerun");
    let matrix = root.join(".eval/matrix.toml");
    let output = root.join(".eval/runs/run-test");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    console.begin_run(matrix.clone(), output.clone(), RunControl::default());
    console.apply_run_event(RunEvent::Started(started()));

    console.handle_key(key(KeyCode::Esc));
    assert!(console.running.is_cancel_requested());
    assert_eq!(console.running.status_text(), "中断中…等待在飞题完成");
    assert!(!console.running.is_finished(), "中断等待期间仍算运行中");

    console.apply_ui_event(UiEvent::RunDone(Ok(Box::new(report(RunStatus::Aborted)))));
    assert_eq!(console.running.status_text(), "已中断");

    match console.handle_key(key(KeyCode::Char('r'))) {
        ConsoleAction::StartRun {
            matrix: next_matrix,
            output: next_output,
        } => {
            assert_eq!(next_matrix, matrix, "续跑应沿用本次矩阵");
            assert_eq!(next_output, output, "续跑应沿用同一输出根");
        }
        _ => panic!("结束态按 r 应返回启动动作"),
    }

    match console.handle_key(key(KeyCode::Enter)) {
        ConsoleAction::LoadReview { root } => assert_eq!(
            root,
            PathBuf::from(".eval/runs/run-test"),
            "应审查运行器上报的输出根"
        ),
        _ => panic!("结束态按 Enter 应进入审查"),
    }
    assert_eq!(console.screen, Screen::Review);
    cleanup(&root);
}

// =========================================================
// 渲染
// =========================================================

/// 首页、矩阵与运行中三屏的关键文案可见。
#[test]
fn screens_render_expected_text() {
    let root = temp_root("render");
    write_matrix(&root, "matrix.toml", "u1-m1");
    write_tier(&root, "u1-m1", 8);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);

    let home = compact(&render_to_text(&console));
    for needle in ["评测台", "沙盒", "矩阵运行"] {
        assert!(home.contains(needle), "首页应包含 {needle}，实际:\n{home}");
    }

    console.handle_key(key(KeyCode::Down));
    console.handle_key(key(KeyCode::Enter));
    console.handle_key(key(KeyCode::Enter));
    // Enter 在矩阵屏返回启动动作，由事件循环切屏；测试里手动切到运行屏。
    console.begin_run(
        root.join(".eval/matrix.toml"),
        root.join(".eval/runs/run-test"),
        RunControl::default(),
    );
    console.apply_run_event(RunEvent::Started(started()));
    console.apply_run_event(RunEvent::ConditionStarted {
        index: 0,
        total: 2,
        id: "c1".to_string(),
        items: 2,
    });
    console.apply_run_event(RunEvent::ItemDone(item("c1", "q0", true, 1, 0)));

    let matrix_text = {
        let mut preview_console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
        preview_console.handle_key(key(KeyCode::Down));
        preview_console.handle_key(key(KeyCode::Enter));
        compact(&render_to_text(&preview_console))
    };
    for needle in ["矩阵运行", "matrix.toml", "条件4", "计划题数5"] {
        assert!(
            matrix_text.contains(needle),
            "矩阵屏应包含 {needle}，实际:\n{matrix_text}"
        );
    }

    let running = compact(&render_to_text(&console));
    for needle in ["矩阵运行", "运行中", "完成1", "事件", "q0"] {
        assert!(
            running.contains(needle),
            "运行屏应包含 {needle}，实际:\n{running}"
        );
    }
    cleanup(&root);
}
