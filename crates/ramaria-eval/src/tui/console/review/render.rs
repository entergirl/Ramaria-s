//! crates/ramaria-eval/src/tui/console/review/render.rs - Ramaria 审查屏渲染
//!
//! 设计特点:
//! - 三屏各一函数：运行列表、审查（题单 + 详情）、原对话片段
//! - 题单只构造可见窗口：500 / 6300 题都不按帧生成全量行
//! - 详情滚动在渲染期按换行后的实际行数钳制，滚动键只记偏移
//! - 证据行高亮，普通消息保留说话人，长正文由 Paragraph 换行
//! - 除滚动上界外只读：渲染只回填可滚动最大值

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::run::RunStatus;

use super::{DetailLine, DetailStyle, ReviewState, RunsState, SourceLine, SourceView};
use crate::tui::console::render::{footer, marker, title_style};

/// 题单里题目标识的最大显示宽度（字符）。
const QUESTION_WIDTH: usize = 30;

// =========================================================
// 运行列表
// =========================================================

/// 绘制运行列表。
pub(crate) fn draw_runs(frame: &mut Frame, state: &RunsState) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new("评测台 · 审查").style(title_style()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new("运行列表 · .eval/runs（含 oracle 子运行）")
            .style(Style::new().fg(Color::DarkGray)),
        chunks[1],
    );

    let mut lines: Vec<Line> = Vec::new();
    if state.runs.is_empty() {
        let text = state
            .error
            .clone()
            .unwrap_or_else(|| "（未找到运行）".to_string());
        lines.push(Line::from(Span::styled(text, Style::new().fg(Color::Red))));
    } else {
        let height = chunks[2].height.saturating_sub(2) as usize;
        for index in state.window(height) {
            let run = &state.runs[index];
            let active = index == state.selected;
            lines.push(Line::from(vec![
                Span::styled(marker(active), Style::new().fg(Color::Cyan)),
                Span::styled(
                    run.display_id.clone(),
                    if active { title_style() } else { Style::new() },
                ),
                Span::styled(
                    format!(
                        "  {}  条件 {}  完成 {} / 失败 {}  {}  {}",
                        status_text(run.status),
                        run.conditions,
                        run.completed,
                        run.failed,
                        time_text(&run.updated_at),
                        if run.scored { "已判分" } else { "未判分" }
                    ),
                    Style::new().fg(if run.scored {
                        Color::Green
                    } else {
                        Color::DarkGray
                    }),
                ),
            ]));
        }
        if let Some(error) = &state.error {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::new().fg(Color::Red),
            )));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("运行")),
        chunks[2],
    );
    frame.render_widget(footer("↑↓ 选择  r 重扫  Enter 审查  Esc 返回"), chunks[3]);
}

// =========================================================
// 审查屏
// =========================================================

/// 绘制审查屏：左题单 + 右详情。
pub(crate) fn draw_review(frame: &mut Frame, state: &ReviewState) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new(title_line(state)).style(title_style()),
        chunks[0],
    );
    frame.render_widget(Paragraph::new(info_line(state)), chunks[1]);
    frame.render_widget(Paragraph::new(status_line(state)), chunks[2]);

    if state.loading || state.error.is_some() || state.index.is_none() {
        let text = if state.loading {
            "正在建立索引…（只扫描每行头部）".to_string()
        } else {
            state
                .error
                .clone()
                .unwrap_or_else(|| "（无可审查的运行）".to_string())
        };
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::bordered().title("审查"))
                .wrap(Wrap { trim: false }),
            chunks[3],
        );
        frame.render_widget(footer(review_footer()), chunks[4]);
        return;
    }

    if state.questions().is_empty() {
        let empty_run = state
            .index
            .as_ref()
            .map(|index| index.items.is_empty())
            .unwrap_or(true);
        let message = if empty_run {
            "该运行没有逐题产物（先运行矩阵或等待首题写出）"
        } else {
            "该条件没有题目"
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(Block::bordered().title("审查"))
                .wrap(Wrap { trim: false }),
            chunks[3],
        );
        frame.render_widget(footer(review_footer()), chunks[4]);
        return;
    }

    let body = Layout::horizontal([Constraint::Length(36), Constraint::Min(20)]).split(chunks[3]);
    draw_question_list(frame, state, body[0]);
    draw_detail(frame, state, body[1]);
    frame.render_widget(footer(review_footer()), chunks[4]);
}

/// 标题行：运行标识与条件计数。
fn title_line(state: &ReviewState) -> String {
    match &state.index {
        Some(index) => format!(
            "评测台 · 审查 · {} · {} 条",
            index.run_id,
            index.items.len()
        ),
        None => "评测台 · 审查".to_string(),
    }
}

/// 信息行：条件、题号与判分状态。
fn info_line(state: &ReviewState) -> String {
    let scored = state
        .index
        .as_ref()
        .map(|index| {
            if index.has_scored_file() {
                "逐题已判分"
            } else {
                "未判分（无逐题判分）"
            }
        })
        .unwrap_or("-");
    let condition = state
        .condition()
        .map(|condition| {
            format!(
                "{}  {}/{}",
                condition.id,
                condition.completed + condition.failed,
                condition.total
            )
        })
        .unwrap_or_else(|| "-".to_string());
    let total = state.questions().len();
    let position = if total == 0 {
        "0/0".to_string()
    } else {
        format!("{}/{}", state.question_pos + 1, total)
    };
    format!("条件 {condition}   第 {position} 题   {scored}")
}

/// 状态槽位：单次读取失败等提示，无提示时空一行。
fn status_line(state: &ReviewState) -> Line<'static> {
    match &state.status {
        Some(status) => Line::from(Span::styled(status.clone(), Style::new().fg(Color::Yellow))),
        None => Line::from(""),
    }
}

/// 左侧题单：只构造可见窗口内的行。
fn draw_question_list(frame: &mut Frame, state: &ReviewState, area: Rect) {
    let title = match state.condition() {
        Some(condition) => format!("题单 · {}", condition.id),
        None => "题单".to_string(),
    };
    let height = area.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let questions = state.questions();
    if questions.is_empty() {
        lines.push(Line::from(Span::styled(
            "（本条件没有题目）",
            Style::new().fg(Color::DarkGray),
        )));
    }
    for position in state.window(height) {
        let question_id = state
            .index
            .as_ref()
            .and_then(|index| index.item(questions[position]))
            .map(|entry| entry.question_id.as_str())
            .unwrap_or("-");
        let selected = position == state.question_pos;
        lines.push(Line::from(vec![
            Span::styled(marker(selected), Style::new().fg(Color::Cyan)),
            Span::raw(truncate(question_id, QUESTION_WIDTH)),
            Span::styled(
                format!("  {}/{}", position + 1, questions.len()),
                Style::new().fg(Color::DarkGray),
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}

/// 右侧详情：按换行后的实际行数钳制滚动。
fn draw_detail(frame: &mut Frame, state: &ReviewState, area: Rect) {
    let title = match state.current_ref() {
        Some(entry) => {
            let scored = match state.detail.as_ref().map(|detail| detail.scored.is_some()) {
                Some(true) => "已判分",
                Some(false) => "未判分",
                None => "-",
            };
            format!("详情 · {} · {scored}", entry.question_id)
        }
        None => "详情".to_string(),
    };
    let lines: Vec<Line> = match &state.detail {
        Some(detail) => detail.lines.iter().map(detail_line).collect(),
        None => vec![Line::from(Span::styled(
            "（选择题目查看详情）",
            Style::new().fg(Color::DarkGray),
        ))],
    };
    let paragraph = Paragraph::new(lines)
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false });
    let width = area.width.saturating_sub(2);
    let total = paragraph.line_count(width);
    let inner = area.height.saturating_sub(2) as usize;
    let max = total.saturating_sub(inner);
    state
        .detail_max_scroll
        .set(u16::try_from(max).unwrap_or(u16::MAX));
    let scroll = (state.detail_scroll as usize).min(max);
    frame.render_widget(paragraph.scroll((scroll as u16, 0)), area);
}

/// 详情行 → 渲染行。
fn detail_line(line: &DetailLine) -> Line<'static> {
    let style = match line.style {
        DetailStyle::Normal => Style::new(),
        DetailStyle::Heading => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        DetailStyle::Good => Style::new().fg(Color::Green),
        DetailStyle::Bad => Style::new().fg(Color::Red),
        DetailStyle::Muted => Style::new().fg(Color::DarkGray),
    };
    Line::from(Span::styled(line.text.clone(), style))
}

/// 审查屏页脚。
fn review_footer() -> String {
    "Tab 焦点  ↑↓ 选题/滚动  ←→ 条件  PgUp/PgDn 翻页  Home/End  g 原对话  r 重建  Esc 返回"
        .to_string()
}

// =========================================================
// 原对话片段
// =========================================================

/// 绘制原对话片段。
pub(crate) fn draw_source(frame: &mut Frame, view: &SourceView) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new("评测台 · 审查 · 原对话").style(title_style()),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(format!("样本 {}   证据行以 >> 标记", view.sample_key))
            .style(Style::new().fg(Color::DarkGray)),
        chunks[1],
    );

    let paragraph = if view.loading {
        Paragraph::new("正在加载语料（首次约 26MB）…")
            .block(Block::bordered().title("原对话"))
            .wrap(Wrap { trim: false })
    } else if let Some(error) = &view.error {
        Paragraph::new(error.clone())
            .style(Style::new().fg(Color::Red))
            .block(Block::bordered().title("原对话"))
            .wrap(Wrap { trim: false })
    } else {
        let lines: Vec<Line> = view.lines.iter().map(source_line).collect();
        Paragraph::new(lines)
            .block(Block::bordered().title("原对话"))
            .wrap(Wrap { trim: false })
    };

    let width = chunks[2].width.saturating_sub(2);
    let total = paragraph.line_count(width);
    let inner = chunks[2].height.saturating_sub(2) as usize;
    let max = total.saturating_sub(inner);
    view.max_scroll.set(u16::try_from(max).unwrap_or(u16::MAX));
    let scroll = (view.scroll as usize).min(max);
    frame.render_widget(paragraph.scroll((scroll as u16, 0)), chunks[2]);
    frame.render_widget(
        footer("↑↓ 滚动  PgUp/PgDn 翻页  Home/End  Esc 返回审查"),
        chunks[3],
    );
}

/// 片段行 → 渲染行。
fn source_line(line: &SourceLine) -> Line<'static> {
    let style = if line.highlight {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else if line.text.starts_with("——") {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new()
    };
    Line::from(Span::styled(line.text.clone(), style))
}

// =========================================================
// 文本辅助
// =========================================================

/// 状态中文。
fn status_text(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "运行中",
        RunStatus::Completed => "已完成",
        RunStatus::Aborted => "已中断",
    }
}

/// RFC3339 时间 → 人读短文本（保留到分钟）。
fn time_text(raw: &str) -> String {
    let text = raw.trim_end_matches('Z').replace('T', " ");
    text.chars().take(16).collect()
}

/// 按字符截断，超长补省略号。
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut output: String = text.chars().take(width.saturating_sub(1)).collect();
    output.push('…');
    output
}
