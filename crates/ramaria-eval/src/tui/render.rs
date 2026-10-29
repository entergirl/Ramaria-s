//! crates/ramaria-eval/src/tui/render.rs - Ramaria 评测沙盒界面渲染
//!
//! 设计特点:
//! - 只读界面状态；滚动位置在绘制时按内容行数取上限，避免滚出空白
//! - 各屏共用底部快捷键行，屏内只决定标题与主体
//! - 阶段前缀与颜色一一对应：ok 绿 / warn 黄 / failed 红 / skipped 灰
//! - 结果页正文按行拼成文本后交给 Paragraph 换行，不手工折行

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::app::{App, BackendFocus, Focus, ResultTab, Screen};
use super::text::TextBuffer;
use crate::measure::estimate_tokens;
use crate::sandbox::backend::LOCAL_DEFAULT_BASE_URL;
use crate::sandbox::{SandboxBackendMode, SandboxReport, StageStatus, parse_dialogue, trace_lines};

// =========================================================
// 入口
// =========================================================

/// 按当前屏幕绘制一帧。
pub(crate) fn draw(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::Backend => draw_backend(frame, app),
        Screen::Dialogue => draw_dialogue(frame, app),
        Screen::Question => draw_question(frame, app),
        Screen::Running => draw_running(frame, app),
        Screen::Result => draw_result(frame, app),
        Screen::Error => draw_error(frame, app),
    }
}

// =========================================================
// 后端屏
// =========================================================

fn draw_backend(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new("评测沙盒 · 后端").style(title_style()),
        chunks[0],
    );
    frame.render_widget(source_line(app.backend_mode), chunks[1]);
    match app.backend_mode {
        SandboxBackendMode::Inherit => frame.render_widget(inherit_hint(), chunks[2]),
        SandboxBackendMode::Local | SandboxBackendMode::Cloud => {
            draw_backend_fields(frame, app, chunks[2])
        }
    }
    frame.render_widget(status_line(app.status.as_deref()), chunks[3]);
    frame.render_widget(
        footer("Tab 切换  ←→ 改来源  Ctrl+Enter/F5 下一步  Esc 退出"),
        chunks[4],
    );
}

/// 来源行：当前来源高亮，附方向键提示。
fn source_line(active: SandboxBackendMode) -> Line<'static> {
    let mut spans = vec![Span::raw("来源: ")];
    for (index, mode) in [
        SandboxBackendMode::Inherit,
        SandboxBackendMode::Local,
        SandboxBackendMode::Cloud,
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            spans.push(Span::raw(" / "));
        }
        let style = if mode == active {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        spans.push(Span::styled(mode.label(), style));
    }
    spans.push(Span::styled("  ←→ 切换", Style::new().fg(Color::DarkGray)));
    Line::from(spans)
}

/// 继承来源的说明：读取时机与降级口径。
fn inherit_hint() -> Paragraph<'static> {
    Paragraph::new("运行前读取当前用户配置；读取不到时回退本地默认并提示降级")
        .block(Block::bordered().title("说明"))
        .wrap(Wrap { trim: false })
}

/// 独立来源的三个单行输入块。
fn draw_backend_fields(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .split(area);

    let local = app.backend_mode == SandboxBackendMode::Local;
    let base_url_hint = if local {
        format!("（留空使用 {LOCAL_DEFAULT_BASE_URL}）")
    } else {
        "（必填）".to_string()
    };
    let model_title = if local {
        "模型标识（本地留空=自动选择）"
    } else {
        "模型标识（必填）"
    };

    frame.render_widget(
        backend_field(
            "模型服务地址",
            app.backend_focus == BackendFocus::BaseUrl,
            &app.backend_base_url,
            &base_url_hint,
        ),
        chunks[0],
    );
    frame.render_widget(
        backend_field(
            model_title,
            app.backend_focus == BackendFocus::Model,
            &app.backend_model,
            "",
        ),
        chunks[1],
    );
    frame.render_widget(
        backend_field(
            "向量模型目录（可选，留空=向量通道降级）",
            app.backend_focus == BackendFocus::Embedding,
            &app.backend_embedding,
            "",
        ),
        chunks[2],
    );
}

/// 单行输入块：聚焦时显示光标标记，空值显示占位提示。
fn backend_field(
    title: &'static str,
    active: bool,
    buffer: &TextBuffer,
    hint: &str,
) -> Paragraph<'static> {
    let content = if active {
        Line::from(buffer.text_with_cursor())
    } else if buffer.is_empty() {
        Line::from(Span::styled(
            hint.to_string(),
            Style::new().fg(Color::DarkGray),
        ))
    } else {
        Line::from(buffer.text())
    };
    Paragraph::new(content).block(
        Block::bordered()
            .title(title)
            .border_style(border_style(active)),
    )
}

// =========================================================
// 对话屏
// =========================================================

fn draw_dialogue(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(Paragraph::new("评测沙盒").style(title_style()), chunks[0]);

    let persona_text = if app.focus == Focus::Persona {
        app.persona.text_with_cursor()
    } else if app.persona.is_empty() {
        "（留空使用内置沙盒人格）".to_string()
    } else {
        app.persona.text()
    };
    frame.render_widget(
        Paragraph::new(persona_text).block(
            Block::bordered()
                .title("人格预设（可选）")
                .border_style(border_style(app.focus == Focus::Persona)),
        ),
        chunks[1],
    );

    let dialogue_text = if app.focus == Focus::Dialogue {
        app.dialogue.text_with_cursor()
    } else {
        app.dialogue.text()
    };
    frame.render_widget(
        Paragraph::new(dialogue_text)
            .block(
                Block::bordered()
                    .title("对话（user:/assistant:）")
                    .border_style(border_style(app.focus == Focus::Dialogue)),
            )
            .wrap(Wrap { trim: false }),
        chunks[2],
    );

    frame.render_widget(status_line(app.status.as_deref()), chunks[3]);
    frame.render_widget(
        footer("Tab 切换  Ctrl+Enter/F5 下一步  Esc 返回后端"),
        chunks[4],
    );
}

// =========================================================
// 提问屏
// =========================================================

fn draw_question(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let turns = match parse_dialogue(&app.dialogue.text()) {
        Ok(turns) => turns.len(),
        Err(_) => 0,
    };
    frame.render_widget(
        Paragraph::new(format!(
            "对话 {turns} 条 / 问题（{} 字符）",
            app.question.char_count()
        ))
        .style(title_style()),
        chunks[0],
    );

    let question_text = app.question.text_with_cursor();
    frame.render_widget(
        Paragraph::new(question_text)
            .block(
                Block::bordered()
                    .title("问题")
                    .border_style(border_style(true)),
            )
            .wrap(Wrap { trim: false }),
        chunks[1],
    );

    frame.render_widget(status_line(app.status.as_deref()), chunks[2]);
    frame.render_widget(footer("Enter 运行  Esc 返回"), chunks[3]);
}

// =========================================================
// 运行屏
// =========================================================

fn draw_running(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let header = if app.running {
        "评测沙盒 · 运行中"
    } else {
        "评测沙盒 · 已结束"
    };
    frame.render_widget(Paragraph::new(header).style(title_style()), chunks[0]);

    let lines: Vec<Line> = app.stages.iter().map(stage_line).collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("阶段")),
        chunks[1],
    );

    frame.render_widget(footer("处理中… Ctrl+C 退出"), chunks[2]);
}

/// 单条阶段行：状态前缀按状态着色，正文与行模式口径一致。
fn stage_line(stage: &crate::sandbox::StageReport) -> Line<'static> {
    let (prefix, color) = match stage.status {
        StageStatus::Ok => ("[ok]", Color::Green),
        StageStatus::Warn => ("[warn]", Color::Yellow),
        StageStatus::Failed => ("[failed]", Color::Red),
        StageStatus::Skipped => ("[skipped]", Color::DarkGray),
    };
    Line::from(vec![
        Span::styled(format!("{prefix} "), Style::new().fg(color)),
        Span::raw(format!(
            "{} {}ms - {}",
            stage.kind.as_str(),
            stage.elapsed_ms,
            stage.detail
        )),
    ])
}

// =========================================================
// 结果屏
// =========================================================

fn draw_result(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(Paragraph::new(tab_line(app.result_tab)), chunks[0]);

    let lines = result_lines(app);
    let max_scroll = lines.len().saturating_sub(1);
    let scroll = usize::from(app.scroll).min(max_scroll);
    let scroll = u16::try_from(scroll).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .block(Block::bordered().title("结果"))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        chunks[1],
    );

    frame.render_widget(footer("Tab/←→ 切换  ↑↓ 滚动  q 退出"), chunks[2]);
}

/// 结果页标签行，当前页高亮。
fn tab_line(active: ResultTab) -> Line<'static> {
    let tabs = [
        (ResultTab::Overview, "概览"),
        (ResultTab::Trace, "召回 trace"),
        (ResultTab::Answer, "答案"),
    ];
    let mut spans = Vec::new();
    for (index, (tab, label)) in tabs.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" | "));
        }
        let style = if tab == active {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

/// 结果页正文；按当前分页返回可读文本行。
///
/// 说明:
/// - 概览与答案页只读报告字段；轨迹页复用行模式的轨迹展开，末尾补服务层注入规模；
/// - 界面与按键共用本函数计数，保证「到底」与内容长度一致。
pub(crate) fn result_lines(app: &App) -> Vec<String> {
    let Some(report) = &app.result else {
        return vec!["（暂无结果）".to_string()];
    };
    match app.result_tab {
        ResultTab::Overview => overview_lines(report),
        ResultTab::Trace => trace_page_lines(report),
        ResultTab::Answer => answer_lines(report),
    }
}

fn overview_lines(report: &SandboxReport) -> Vec<String> {
    let mut lines = vec![
        format!("人格: {}", report.persona_uid),
        format!(
            "会话: {} / 消息 {} 条",
            report.session_id, report.message_count
        ),
        format!("库路径: {}", report.root.display()),
        format!(
            "后端: {}（{}）",
            report.backend.source,
            report.backend.kind.label()
        ),
        format!("模型: {}", report.backend.model_text()),
        format!("地址: {}", report.backend.base_url),
        format!("连接: {}", report.backend.llm_status),
        format!("向量: {}", report.backend.embedding_status),
        if report.seal.sealed {
            format!("封存: 已完成，L1 摘要 {} 条", report.seal.l1_count)
        } else if report
            .warnings
            .iter()
            .any(|warning| warning.starts_with("seal:"))
        {
            "封存: 失败（见告警）".to_string()
        } else {
            "封存: 未完成（本次调用未抢占到封存）".to_string()
        },
        match &report.cascade {
            Some(outcome) => format!(
                "级联: L1 消费 {} 条 / L2 事件 {} 条 / L3 标签 {} 个",
                outcome.l1_consumed, outcome.l2_events, outcome.l3_traits
            ),
            None => "级联: 未执行（失败已记为告警）".to_string(),
        },
        match report.index_docs {
            Some(total) => format!("索引: {total} 篇文档"),
            None => "索引: 未重建（失败已记为告警）".to_string(),
        },
        format!(
            "耗时: 总计 {} ms（召回 {} ms / 回答 {} ms）",
            report.total_elapsed_ms, report.recall_elapsed_ms, report.answer_elapsed_ms
        ),
        format!("问题: {}", report.question),
        String::new(),
        "阶段:".to_string(),
    ];

    for stage in &report.stages {
        lines.push(format!(
            "  [{}] {} {}ms - {}",
            stage.status.as_str(),
            stage.kind.as_str(),
            stage.elapsed_ms,
            stage.detail
        ));
    }

    if !report.warnings.is_empty() {
        lines.push(String::new());
        lines.push("警告:".to_string());
        for warning in &report.warnings {
            lines.push(format!("  - {warning}"));
        }
    }

    lines
}

fn trace_page_lines(report: &SandboxReport) -> Vec<String> {
    let mut lines = trace_lines(&report.recall.trace);
    lines.push(format!(
        "服务层注入: {} 条 / {} tokens / {} 字符",
        report.recall.result.items.len(),
        estimate_tokens(&report.recall.result.context),
        report.recall.result.context.chars().count()
    ));
    lines
}

fn answer_lines(report: &SandboxReport) -> Vec<String> {
    let mut lines = vec![
        format!("问题: {}", report.question),
        String::new(),
        "答案:".to_string(),
    ];
    match &report.answer {
        Some(answer) => lines.push(answer.replace("||", "\n")),
        None => {
            let reason = report.answer_error.as_deref().unwrap_or("原因未知");
            lines.push(format!("（未生成）{reason}"));
        }
    }
    lines
}

// =========================================================
// 失败屏
// =========================================================

fn draw_error(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new("运行失败").style(Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
        chunks[0],
    );

    let message = app.error.clone().unwrap_or_else(|| "原因未知".to_string());
    frame.render_widget(
        Paragraph::new(message)
            .block(Block::bordered().title("错误"))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );

    frame.render_widget(footer("Esc 返回后端  q 退出"), chunks[2]);
}

// =========================================================
// 公共样式
// =========================================================

fn title_style() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

fn border_style(active: bool) -> Style {
    if active {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

fn footer(text: &'static str) -> Paragraph<'static> {
    Paragraph::new(text).style(Style::new().fg(Color::DarkGray))
}

fn status_line(status: Option<&str>) -> Paragraph<'_> {
    match status {
        Some(message) => Paragraph::new(message.to_string()).style(Style::new().fg(Color::Red)),
        None => Paragraph::new(""),
    }
}
