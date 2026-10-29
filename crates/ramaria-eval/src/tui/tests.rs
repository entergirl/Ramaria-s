//! crates/ramaria-eval/src/tui/tests.rs - Ramaria 评测沙盒界面单元测试
//!
//! 设计特点:
//! - 文本缓冲覆盖中文插入、退格与光标移动，确认按字符而非字节处理
//! - 渲染断言走 TestBackend，只比对去掉空白后的可见文本
//! - 报告完全合成：不开库、不装配引擎、不启动运行任务
//! - 不进入原始模式：测试不触碰真实终端，只在内存缓冲区上绘制

use std::path::PathBuf;

use ramaria_service::{
    BudgetTrace, CascadeOutcome, ChannelCandidate, ChannelTrace, DroppedTrace, FusedEntryTrace,
    RecallResult, RecallTrace, RecallWithTrace, RetrievalTrace, SealOutcome, TraceChannel,
    UttTrace,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use uuid::Uuid;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, AppAction, ResultTab, Screen};
use super::render;
use super::text::TextBuffer;
use crate::sandbox::{
    BackendReport, SandboxBackendChoice, SandboxBackendMode, SandboxModelKind, StageKind,
    StageReport, StageStatus,
};

// =========================================================
// 夹具
// =========================================================

/// 在 100x30 的测试终端上绘制一帧并取回可见文本。
fn render_to_text(app: &App) -> String {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("测试终端应可创建");
    terminal
        .draw(|frame| render::draw(frame, app))
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

/// 合成一条覆盖四通道、融合、丢弃、预算与原文通道的轨迹。
fn sample_trace() -> RecallTrace {
    RecallTrace {
        query_chars: 12,
        query_vector_dim: Some(768),
        retrieval: RetrievalTrace {
            channels: vec![
                ChannelTrace {
                    channel: TraceChannel::Vector,
                    active: true,
                    candidates: vec![ChannelCandidate {
                        id: "L1:a".to_string(),
                        raw_score: 0.9,
                        rank: 1,
                    }],
                },
                ChannelTrace {
                    channel: TraceChannel::Bm25,
                    active: true,
                    candidates: vec![ChannelCandidate {
                        id: "L2:7".to_string(),
                        raw_score: 1.25,
                        rank: 1,
                    }],
                },
                ChannelTrace {
                    channel: TraceChannel::Graph,
                    active: false,
                    candidates: Vec::new(),
                },
                ChannelTrace {
                    channel: TraceChannel::Keyword,
                    active: false,
                    candidates: Vec::new(),
                },
            ],
            fused: vec![FusedEntryTrace {
                id: "L2:7".to_string(),
                layer: "l2".to_string(),
                rrf_score: 0.03,
                decay_factor: 0.87,
                decayed_score: 0.026,
                vector_raw: Some(0.9),
                bm25_raw: Some(1.25),
                graph_raw: None,
                keyword_raw: Some(0.4),
            }],
            dropped: vec![DroppedTrace {
                id: "L2:8".to_string(),
                reason: ramaria_service::DropReason::FusionCut,
                score: 0.5,
            }],
        },
        dropped: vec![DroppedTrace {
            id: "L2:9".to_string(),
            reason: ramaria_service::DropReason::MaxMemories,
            score: 0.1,
        }],
        budget: BudgetTrace {
            max_memories: 5,
            candidates: 6,
            kept: 5,
            tokens_before: 120,
            tokens_after: 96,
        },
        utt: UttTrace {
            active: true,
            candidates: vec![ChannelCandidate {
                id: "L0:1".to_string(),
                raw_score: 0.81,
                rank: 1,
            }],
            rendered_chars: 240,
            budget_chars: 1500,
        },
    }
}

/// 合成一份完整报告。
fn sample_report() -> crate::sandbox::SandboxReport {
    let session_id = Uuid::nil();
    crate::sandbox::SandboxReport {
        root: PathBuf::from(".eval/sandbox"),
        backend: BackendReport {
            mode: SandboxBackendMode::Inherit,
            source: "产品默认".to_string(),
            kind: SandboxModelKind::Local,
            model: String::new(),
            base_url: "http://localhost:1234/v1".to_string(),
            llm_status: "可达".to_string(),
            embedding_status: "未配置".to_string(),
        },
        persona_uid: "sandbox-0001".to_string(),
        session_id,
        message_count: 8,
        seal: SealOutcome {
            session_id,
            sealed: true,
            l1_count: 1,
        },
        cascade: Some(CascadeOutcome {
            persona_uid: "sandbox-0001".to_string(),
            l1_consumed: 1,
            l2_events: 2,
            l3_traits: 0,
        }),
        index_docs: Some(3),
        recall: RecallWithTrace {
            result: RecallResult::default(),
            trace: sample_trace(),
        },
        question: "你记得我叫什么吗？".to_string(),
        answer: Some("你叫小林，周末常去爬山。".to_string()),
        answer_error: None,
        warnings: vec!["seal: 封存失败，继续运行".to_string()],
        stages: vec![StageReport {
            kind: StageKind::Recall,
            status: StageStatus::Ok,
            elapsed_ms: 12,
            detail: "召回 2 条，预算保留 2 条".to_string(),
        }],
        total_elapsed_ms: 120,
        recall_elapsed_ms: 30,
        answer_elapsed_ms: 40,
    }
}

// =========================================================
// 文本缓冲
// =========================================================

/// 中文按字符插入与退格，光标左右移动后插入不破坏字符边界，粘贴按压 CRLF 归一。
#[test]
fn text_buffer_edits_by_char_and_normalizes_paste() {
    let mut buffer = TextBuffer::new();
    buffer.insert_str("你好\r\n世界");
    assert_eq!(buffer.char_count(), 5, "CRLF 应归一为一个换行");
    assert_eq!(buffer.text(), "你好\n世界");

    buffer.backspace();
    assert_eq!(buffer.text(), "你好\n世");

    buffer.move_left();
    buffer.insert_char('呀');
    assert_eq!(buffer.text(), "你好\n呀世");

    // 光标在第二行行中：home 回到本行行首而不是全文开头
    buffer.home();
    buffer.insert_str("问候");
    assert_eq!(buffer.text(), "你好\n问候呀世");

    buffer.end();
    buffer.insert_char('。');
    assert_eq!(buffer.text(), "你好\n问候呀世。");
    assert_eq!(
        buffer.text_with_cursor(),
        "你好\n问候呀世。|",
        "末尾光标应显示在末尾"
    );
}

/// 光标标记按字符位置插入，不破坏中文。
#[test]
fn text_buffer_marks_cursor_position() {
    let mut buffer = TextBuffer::new();
    buffer.insert_str("你好");
    buffer.move_left();
    assert_eq!(buffer.text_with_cursor(), "你|好");
}

// =========================================================
// 渲染
// =========================================================

/// 对话屏出现标题、对话块标题与角色前缀提示。
#[test]
fn dialogue_screen_renders_header_and_editor() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    app.screen = Screen::Dialogue;
    let text = compact(&render_to_text(&app));
    for needle in ["评测沙盒", "对话", "user:"] {
        assert!(
            text.contains(needle),
            "对话屏应包含 {needle}，实际:\n{text}"
        );
    }
}

/// conhost 不上报 Ctrl+Enter 时，F5 走同一条前进路径。
#[test]
fn f5_advances_from_dialogue_to_question() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    app.screen = Screen::Dialogue;
    app.dialogue.insert_str("user: 你好\nassistant: 在的");

    let action = app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));

    assert!(matches!(action, AppAction::None));
    assert_eq!(app.screen, Screen::Question, "F5 应从对话屏进入提问屏");
}

// =========================================================
// 后端屏
// =========================================================

/// 后端屏列出三种来源，并说明继承来源的降级口径。
#[test]
fn backend_screen_renders_sources() {
    let app = App::new(SandboxBackendChoice::Inherit);
    assert_eq!(app.screen, Screen::Backend, "启动应停在后端屏");

    let text = compact(&render_to_text(&app));
    for needle in [
        "评测沙盒·后端",
        "来源:",
        "继承当前配置",
        "本地模型",
        "云端模型",
        "读取不到时回退本地默认",
    ] {
        assert!(
            text.contains(needle),
            "后端屏应包含 {needle}，实际:\n{text}"
        );
    }
}

/// 继承来源按 F5 直接进入对话屏。
#[test]
fn f5_advances_from_backend_to_dialogue() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    let action = app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
    assert!(matches!(action, AppAction::None));
    assert_eq!(app.screen, Screen::Dialogue);
}

/// 云端来源填好地址与模型后，选择结果映射为独立云端配置。
#[test]
fn cloud_backend_choice_maps_to_independent_cloud() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    // 继承 → 本地 → 云端；本地默认地址在切到云端时清空
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.backend_mode, SandboxBackendMode::Cloud);
    app.backend_base_url.insert_str("https://cloud.example/v1");
    app.backend_model.insert_str("model-a");

    assert_eq!(
        app.backend_choice(),
        SandboxBackendChoice::Independent {
            kind: SandboxModelKind::Cloud,
            base_url: "https://cloud.example/v1".to_string(),
            model_id: "model-a".to_string(),
            embedding_model_path: None,
        }
    );
}

/// 云端来源缺模型标识时不进入对话屏，状态行给出提示。
#[test]
fn cloud_backend_blocks_advance_without_model() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.backend_base_url.insert_str("https://cloud.example/v1");

    let action = app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));

    assert!(matches!(action, AppAction::None));
    assert_eq!(app.screen, Screen::Backend, "缺模型标识应留在后端屏");
    let status = app.status.as_deref().unwrap_or("");
    assert!(
        status.contains("模型标识"),
        "状态行应提示模型标识，实际: {status}"
    );
}

/// 结果轨迹页展示四通道、融合条目与服务层注入规模。
#[test]
fn result_trace_page_shows_channels_and_injection() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    app.set_result(sample_report());
    app.result_tab = ResultTab::Trace;

    let text = compact(&render_to_text(&app));
    for needle in ["通道vector", "L2:7", "tokens"] {
        assert!(
            text.contains(needle),
            "轨迹页应包含 {needle}，实际:\n{text}"
        );
    }
}

/// 结果概览页展示后端来源、模型占位与向量状态，只用本地 / 云端词汇。
#[test]
fn result_overview_page_shows_backend_summary() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    app.set_result(sample_report());
    app.result_tab = ResultTab::Overview;

    let text = compact(&render_to_text(&app));
    for needle in ["后端:产品默认（本地模型）", "模型:自动选择", "向量:未配置"]
    {
        assert!(
            text.contains(needle),
            "概览页应包含 {needle}，实际:\n{text}"
        );
    }
}

/// 结果答案页展示答案全文。
#[test]
fn result_answer_page_shows_answer() {
    let mut app = App::new(SandboxBackendChoice::Inherit);
    app.set_result(sample_report());
    app.result_tab = ResultTab::Answer;

    let text = compact(&render_to_text(&app));
    assert!(
        text.contains("你叫小林，周末常去爬山。"),
        "答案页应包含答案，实际:\n{text}"
    );
}
