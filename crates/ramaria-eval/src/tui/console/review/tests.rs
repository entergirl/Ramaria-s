//! crates/ramaria-eval/src/tui/console/review/tests.rs - Ramaria 审查屏状态与渲染测试
//!
//! 设计特点:
//! - 夹具写真实运行产物，索引走与界面相同的构建入口
//! - 状态迁移优先：首页三项、列表进入审查、条件与题号、原对话逐级返回
//! - 语料用合成样本，`g` 的首次加载与缓存命中各一条
//! - 渲染断言走 TestBackend，只比对去空白后的可见文本

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ramaria_service::ChatRole;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use crate::corpus::{Message, Sample, Session};
use crate::run::{
    ConditionLine, RUN_FILE, RunControl, RunEvent, RunItemRecord, RunReport, RunStarted, RunStatus,
};
use crate::sandbox::SandboxBackendChoice;
use crate::tui::console::UiEvent;
use crate::tui::console::render;
use crate::tui::console::state::{Console, ConsoleAction, HomeEntry, Screen};

use super::{ReviewFocus, ReviewIndex, SourceOpen};

// =========================================================
// 夹具
// =========================================================

fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let root = std::env::temp_dir().join(format!("ramaria-review-console-{tag}-{nanos}"));
    std::fs::create_dir_all(root.join(".eval/runs")).expect("运行目录应可创建");
    root
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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

fn record(condition: &str, question_id: &str, evidence: &[&str]) -> RunItemRecord {
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
        question: "她上次说了什么？".to_string(),
        gold_answer: "她说了旅行计划。".to_string(),
        gold_message_ids: vec!["m1".to_string()],
        evidence: evidence.iter().map(|item| item.to_string()).collect(),
        question_type: vec!["Single_hop".to_string()],
        score_points: Vec::new(),
        category: vec![0],
        reply: Some("她说要去旅行。".to_string()),
        reply_chars: 8,
        recall: None,
        preset_chars: None,
        usage: None,
        elapsed_ms: 100,
        error: None,
        attempts: 1,
        conditions: condition_line(),
    }
}

/// 写一个可被审查的运行根：两个条件、三题。
fn write_run(root: &Path) -> PathBuf {
    let run_root = root.join(".eval/runs/run-test");
    std::fs::create_dir_all(&run_root).expect("运行根应可创建");
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
        "tiers": [{ "id": "u1", "dir": "dir", "manifest_sha256": "hash", "rows": 3 }],
        "conditions": [
            { "id": "c1", "total": 2, "completed": 2, "failed": 0 },
            { "id": "c2", "total": 1, "completed": 1, "failed": 0 }
        ],
        "elapsed_ms": 1000,
        "warnings": []
    });
    std::fs::write(run_root.join(RUN_FILE), body.to_string()).expect("运行状态应可写");

    let mut items = String::new();
    for record in [
        record("c1", "q0", &["chat1"]),
        record("c1", "q1", &["chat1"]),
        record("c2", "q2", &[]),
    ] {
        items.push_str(&serde_json::to_string(&record).expect("逐题产物应可序列化"));
        items.push('\n');
    }
    std::fs::write(run_root.join("items.jsonl"), items).expect("逐题产物应可写");
    run_root
}

fn samples() -> Vec<Sample> {
    let message = |speaker: &str, dia_id: &str, text: &str| Message {
        role: ChatRole::User,
        speaker: speaker.to_string(),
        dia_id: dia_id.to_string(),
        text: text.to_string(),
    };
    vec![Sample {
        sample_id: "sample_alice".to_string(),
        user_key: "alice".to_string(),
        speaker_a: "Alice".to_string(),
        speaker_b: "Bob".to_string(),
        sessions: vec![Session {
            index: 1,
            date: NaiveDate::from_ymd_opt(2025, 1, 2).expect("日期应合法"),
            messages: vec![
                message("Alice", "2025-01-02_chat1", "她说了旅行计划"),
                message("Bob", "2025-01-02_chat2", "知道了"),
            ],
        }],
        qa: Vec::new(),
    }]
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

fn compact(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// 打开运行列表并选中第一项。
fn open_runs(console: &mut Console) {
    console.handle_key(key(KeyCode::Down));
    console.handle_key(key(KeyCode::Down));
    console.handle_key(key(KeyCode::Enter));
}

/// 进入审查并应用索引。
fn open_review(console: &mut Console, root: &Path) {
    open_runs(console);
    console.handle_key(key(KeyCode::Enter));
    let index = ReviewIndex::build(&root.join(".eval/runs/run-test")).expect("索引应可建立");
    console.apply_ui_event(UiEvent::ReviewLoaded {
        root: root.join(".eval/runs/run-test"),
        result: Ok(Box::new(index)),
    });
}

// =========================================================
// 状态
// =========================================================

/// 首页三项循环。
#[test]
fn home_cycles_three_entries() {
    let root = temp_root("home");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    assert_eq!(console.home, HomeEntry::Sandbox);
    console.handle_key(key(KeyCode::Down));
    assert_eq!(console.home, HomeEntry::Matrix);
    console.handle_key(key(KeyCode::Down));
    assert_eq!(console.home, HomeEntry::Review);
    console.handle_key(key(KeyCode::Down));
    assert_eq!(console.home, HomeEntry::Sandbox);
    console.handle_key(key(KeyCode::Up));
    assert_eq!(console.home, HomeEntry::Review);
    cleanup(&root);
}

/// 运行列表 → 审查：先 loading，回投后建立详情。
#[test]
fn review_loads_index_and_first_item() {
    let root = temp_root("load");
    write_run(&root);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    open_runs(&mut console);
    assert_eq!(console.screen, Screen::Runs);
    assert_eq!(console.runs.runs.len(), 1);
    assert_eq!(console.runs.runs[0].display_id, "run-test");
    assert!(!console.runs.runs[0].scored);

    let action = console.handle_key(key(KeyCode::Enter));
    assert!(matches!(action, ConsoleAction::LoadReview { .. }));
    assert_eq!(console.screen, Screen::Review);
    assert!(console.review.loading);

    let index = ReviewIndex::build(&root.join(".eval/runs/run-test")).expect("索引应可建立");
    console.apply_ui_event(UiEvent::ReviewLoaded {
        root: root.join(".eval/runs/run-test"),
        result: Ok(Box::new(index)),
    });
    assert!(!console.review.loading);
    assert!(console.review.error.is_none());
    assert_eq!(console.review.condition_pos, 0);
    assert_eq!(console.review.question_pos, 0);
    assert!(console.review.detail.is_some());
    assert_eq!(console.review.focus, ReviewFocus::List);
    cleanup(&root);
}

/// 换条件重置题号与滚动；题单窗口只覆盖可见范围。
#[test]
fn review_switches_condition_and_resets_question() {
    let root = temp_root("switch");
    write_run(&root);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    open_review(&mut console, &root);
    console.handle_key(key(KeyCode::Down));
    assert_eq!(console.review.question_pos, 1);

    console.handle_key(key(KeyCode::Right));
    assert_eq!(console.review.condition_pos, 1);
    assert_eq!(console.review.question_pos, 0);
    assert_eq!(console.review.detail_scroll, 0);

    console.handle_key(key(KeyCode::Left));
    assert_eq!(console.review.condition_pos, 0);
    assert_eq!(console.review.window(2), 0..2);
    console.handle_key(key(KeyCode::Down));
    assert_eq!(console.review.window(1), 1..2);
    cleanup(&root);
}

/// `g` 首次返回加载请求，缓存命中后直接切屏。
#[test]
fn review_source_uses_cache_after_first_load() {
    let root = temp_root("source");
    write_run(&root);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    open_review(&mut console, &root);

    let action = console.handle_key(key(KeyCode::Char('g')));
    match action {
        ConsoleAction::LoadCorpus(request) => {
            assert_eq!(request.root, root, "语料根应是评测根而不是运行根");
            assert_eq!(request.key(), "lifebench/2.0/zh");
            assert!(
                request
                    .path()
                    .expect("路径应可拼")
                    .starts_with(root.join(".eval/corpus")),
                "语料应在评测根的 .eval/corpus 下"
            );
        }
        _ => panic!("首次 g 应请求加载语料"),
    }
    assert_eq!(console.screen, Screen::Source);
    assert!(console.review.source.as_ref().expect("片段状态").loading);

    console.apply_ui_event(UiEvent::CorpusLoaded {
        key: "lifebench/2.0/zh".to_string(),
        result: Ok(samples()),
    });
    let view = console.review.source.as_ref().expect("片段状态");
    assert!(!view.loading);
    assert!(view.error.is_none());
    assert!(view.lines.iter().any(|line| line.highlight));
    let lines = view.lines.len();

    console.handle_key(key(KeyCode::Esc));
    assert_eq!(console.screen, Screen::Review);
    let action = console.handle_key(key(KeyCode::Char('g')));
    assert!(matches!(action, ConsoleAction::None));
    assert_eq!(console.screen, Screen::Source);
    assert_eq!(
        console
            .review
            .source
            .as_ref()
            .expect("片段状态")
            .lines
            .len(),
        lines
    );
    cleanup(&root);
}

/// 无证据的题直接给提示片段，不发起语料加载。
#[test]
fn review_source_without_evidence_skips_corpus_load() {
    let root = temp_root("no-evidence");
    write_run(&root);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    open_review(&mut console, &root);
    console.handle_key(key(KeyCode::Right));

    let action = console.handle_key(key(KeyCode::Char('g')));
    assert!(matches!(action, ConsoleAction::None));
    assert_eq!(console.screen, Screen::Source);
    assert!(console.review.corpus.is_none(), "不应加载语料");
    let view = console.review.source.as_ref().expect("片段状态");
    assert!(!view.loading);
    assert!(
        view.lines.iter().any(|line| line.text.contains("不可回答")),
        "应给不可回答提示"
    );
    cleanup(&root);
}

/// 语料回投的 key 与当前口径不符时整体忽略，缓存与片段都不变。
#[test]
fn corpus_result_with_wrong_key_is_ignored() {
    let root = temp_root("wrong-key");
    write_run(&root);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    open_review(&mut console, &root);
    console.handle_key(key(KeyCode::Char('g')));

    console.apply_ui_event(UiEvent::CorpusLoaded {
        key: "lifebench/2.0/en".to_string(),
        result: Ok(samples()),
    });
    let view = console.review.source.as_ref().expect("片段状态");
    assert!(view.loading, "key 不符时不应填片段");
    assert!(view.lines.is_empty());
    assert!(console.review.corpus.is_none(), "key 不符时不应入缓存");
    cleanup(&root);
}

/// 没有逐题产物的运行给明确说明。
#[test]
fn review_shows_message_for_run_without_items() {
    let root = temp_root("empty-run");
    let run_root = write_run(&root);
    std::fs::remove_file(run_root.join("items.jsonl")).expect("产物应可删除");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    open_review(&mut console, &root);

    assert!(console.review.questions().is_empty());
    assert!(console.review.detail.is_none());
    let text = compact(&render_to_text(&console));
    assert!(
        text.contains("该运行没有逐题产物"),
        "应提示空运行:
{text}"
    );
    cleanup(&root);
}

/// 无详情时 `g` 被忽略；Esc 逐级返回。
#[test]
fn review_ignores_source_without_detail() {
    let root = temp_root("nodetail");
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    console.screen = Screen::Review;
    console
        .review
        .begin(root.join(".eval/runs/missing"), root.clone());
    assert!(matches!(console.review.open_source(), SourceOpen::None));
    console.handle_key(key(KeyCode::Esc));
    assert_eq!(console.screen, Screen::Runs);
    console.handle_key(key(KeyCode::Esc));
    assert_eq!(console.screen, Screen::Home);
    cleanup(&root);
}

/// 运行结束态 Enter 进入审查。
#[test]
fn finished_run_enters_review() {
    let root = temp_root("finished");
    let run_root = write_run(&root);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);
    console.begin_run(run_root.clone(), run_root.clone(), RunControl::default());
    console.apply_run_event(RunEvent::Started(RunStarted {
        run_id: "run-test".to_string(),
        root: run_root.clone(),
        total_conditions: 1,
        total_items: 1,
        total_executions: 1,
        conditions: Vec::new(),
    }));
    console.apply_ui_event(UiEvent::RunDone(Ok(Box::new(RunReport {
        run_id: "run-test".to_string(),
        root: run_root.clone(),
        total: 1,
        completed: 1,
        failed: 0,
        elapsed_ms: 10,
        status: RunStatus::Completed,
        warnings: Vec::new(),
    }))));

    let action = console.handle_key(key(KeyCode::Enter));
    assert!(matches!(action, ConsoleAction::LoadReview { .. }));
    assert_eq!(console.screen, Screen::Review);
    cleanup(&root);
}

// =========================================================
// 渲染
// =========================================================

/// 三屏关键文案可见，题单与详情不互相覆盖。
#[test]
fn review_screens_render_expected_text() {
    let root = temp_root("render");
    write_run(&root);
    let mut console = Console::new(root.clone(), SandboxBackendChoice::Inherit);

    let home = compact(&render_to_text(&console));
    assert!(home.contains("审查"), "首页应有审查入口:\n{home}");

    open_runs(&mut console);
    let runs = compact(&render_to_text(&console));
    for needle in ["审查", "运行", "run-test", "未判分"] {
        assert!(runs.contains(needle), "运行列表应包含 {needle}:\n{runs}");
    }

    open_review(&mut console, &root);
    let review = compact(&render_to_text(&console));
    for needle in ["题单", "详情", "输入", "参考答案", "回复", "q0"] {
        assert!(review.contains(needle), "审查屏应包含 {needle}:\n{review}");
    }

    console.handle_key(key(KeyCode::Char('g')));
    console.apply_ui_event(UiEvent::CorpusLoaded {
        key: "lifebench/2.0/zh".to_string(),
        result: Ok(samples()),
    });
    let source = compact(&render_to_text(&console));
    for needle in ["原对话", "alice", ">>", "旅行计划"] {
        assert!(
            source.contains(needle),
            "原对话屏应包含 {needle}:\n{source}"
        );
    }
    cleanup(&root);
}
