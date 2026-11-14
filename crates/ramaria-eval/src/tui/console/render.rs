//! crates/ramaria-eval/src/tui/console/render.rs - Ramaria 评测台界面渲染
//!
//! 设计特点:
//! - 只读界面状态：快捷键只由当前屏幕与结束态决定，不写回状态
//! - 沙盒屏直接复用沙盒渲染，评测台只负责首页 / 矩阵 / 运行中 / 错误四屏
//! - 矩阵预览把计划规模与历史均速拼成一行，缺档位时用红色标出禁止启动
//! - 运行中屏按条件列表 + 事件尾两段展开，长内容由 Paragraph 裁切
//! - 首页与页脚共用样式，保证各屏快捷键位置一致

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::matrix::{MatrixPreview, format_duration};
use super::running::RunningState;
use super::state::{Console, HomeEntry, Screen};

/// 首页条目：标识、名称与一行说明。
const HOME_ENTRIES: [(HomeEntry, &str, &str); 3] = [
    (
        HomeEntry::Sandbox,
        "沙盒",
        "贴一段对话，跑完整管线，看召回 trace 与答案",
    ),
    (
        HomeEntry::Matrix,
        "矩阵运行",
        "按 .eval/matrix*.toml 批量运行，观察进度、中断与续跑",
    ),
    (
        HomeEntry::Review,
        "审查",
        "按运行根逐题查看输入、回复、召回证据与分项得分",
    ),
];

// =========================================================
// 入口
// =========================================================

/// 按当前屏幕绘制一帧。
pub(crate) fn draw(frame: &mut Frame, console: &Console) {
    match console.screen {
        Screen::Home => draw_home(frame, console),
        Screen::Sandbox => crate::tui::render::draw(frame, &console.sandbox),
        Screen::Matrix => draw_matrix(frame, console),
        Screen::Running => draw_running(frame, console),
        Screen::Runs => super::review::draw_runs(frame, &console.runs),
        Screen::Review => super::review::draw_review(frame, &console.review),
        Screen::Source => match &console.review.source {
            Some(view) => super::review::draw_source(frame, view),
            None => frame.render_widget(
                Paragraph::new("（无片段）").style(Style::new().fg(Color::DarkGray)),
                frame.area(),
            ),
        },
        Screen::Error => draw_error(frame, console),
    }
}

// =========================================================
// 首页
// =========================================================

fn draw_home(frame: &mut Frame, console: &Console) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(Paragraph::new("评测台").style(title_style()), chunks[0]);
    frame.render_widget(
        Paragraph::new("首页 · 选择要进入的界面").style(Style::new().fg(Color::DarkGray)),
        chunks[1],
    );

    let lines: Vec<Line> = HOME_ENTRIES
        .iter()
        .map(|(entry, name, detail)| {
            let active = console.home == *entry;
            Line::from(vec![
                Span::styled(marker(active), Style::new().fg(Color::Cyan)),
                Span::styled(*name, if active { title_style() } else { Style::new() }),
                Span::styled(format!("  {detail}"), Style::new().fg(Color::DarkGray)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("功能")),
        chunks[2],
    );
    frame.render_widget(footer("↑↓ 选择  Enter 进入  q 退出"), chunks[3]);
}

// =========================================================
// 矩阵屏
// =========================================================

fn draw_matrix(frame: &mut Frame, console: &Console) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new("评测台 · 矩阵运行").style(title_style()),
        chunks[0],
    );

    let body = Layout::horizontal([Constraint::Length(34), Constraint::Min(20)]).split(chunks[1]);
    draw_matrix_list(frame, console, body[0]);
    draw_matrix_preview(frame, console, body[1]);

    frame.render_widget(footer("↑↓ 选择  r 重扫  Enter 启动  Esc 返回"), chunks[2]);
}

/// 左侧矩阵文件列表。
fn draw_matrix_list(frame: &mut Frame, console: &Console, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if console.matrix.entries.is_empty() {
        lines.push(Line::from(Span::styled(
            console
                .matrix
                .error
                .clone()
                .unwrap_or_else(|| "未找到矩阵".to_string()),
            Style::new().fg(Color::Red),
        )));
    } else {
        for (index, path) in console.matrix.entries.iter().enumerate() {
            let active = index == console.matrix.selected;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("matrix")
                .to_string();
            lines.push(Line::from(vec![
                Span::styled(marker(active), Style::new().fg(Color::Cyan)),
                Span::styled(name, if active { title_style() } else { Style::new() }),
            ]));
        }
    }
    if let Some(error) = &console.matrix.error {
        if !console.matrix.entries.is_empty() {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::new().fg(Color::Red),
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title("矩阵"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 右侧预览：轴、计划规模、均速与启动限制。
fn draw_matrix_preview(frame: &mut Frame, console: &Console, area: Rect) {
    let lines = match &console.matrix.preview {
        Some(preview) => preview_lines(preview),
        None => vec![Line::from(Span::styled(
            "（无可用矩阵）",
            Style::new().fg(Color::DarkGray),
        ))],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title("预览"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// 预览正文。
fn preview_lines(preview: &MatrixPreview) -> Vec<Line<'static>> {
    let mut lines = vec![
        field("文件", preview.path.display().to_string()),
        field("语料", format!("{} / {}", preview.corpus, preview.language)),
        field("档位", join_preview(&preview.tiers)),
        field("管线", join_preview(&preview.pipelines)),
        field(
            "预算",
            preview
                .budgets
                .iter()
                .map(|budget| format!("{budget} 字符"))
                .collect::<Vec<_>>()
                .join(" / "),
        ),
        field("模型", join_preview(&preview.models)),
        field("题集", question_scope(preview)),
        field(
            "隔离",
            if preview.strict_isolation {
                "严格（按条件复制基线库）".to_string()
            } else {
                "默认（共用一个工作库）".to_string()
            },
        ),
    ];

    match &preview.counts {
        Some(counts) => {
            lines.push(field(
                "规模",
                format!(
                    "条件 {} / 计划题数 {} / 计划条目 {}",
                    counts.conditions, counts.questions, counts.executions
                ),
            ));
            let tiers = counts
                .per_tier
                .iter()
                .map(|(tier, count)| format!("{tier} {count}"))
                .collect::<Vec<_>>()
                .join(" / ");
            if !tiers.is_empty() {
                lines.push(field("每档题数", tiers));
            }
            let pace = preview.pace.label();
            match preview.eta_ms() {
                Some(ms) => lines.push(field(
                    "耗时",
                    format!("{pace}，预计 {}", format_duration(ms)),
                )),
                None => lines.push(field("耗时", pace)),
            }
        }
        None => lines.push(field("规模", "无法计算（档位未就绪）".to_string())),
    }

    if let Some(error) = &preview.error {
        lines.push(Line::from(Span::styled(
            format!("无法启动: {error}"),
            Style::new().fg(Color::Red),
        )));
    }
    lines
}

/// 题集口径：抽样与上限二选一。
fn question_scope(preview: &MatrixPreview) -> String {
    if let Some(limit) = preview.question_limit {
        format!("全局上限 {limit} 题")
    } else if let Some(sample) = preview.question_sample {
        format!("每档抽样 {sample} 题")
    } else {
        "不限题数".to_string()
    }
}

/// 列表拼接；空列表显示占位。
fn join_preview(values: &[String]) -> String {
    if values.is_empty() {
        "（未声明）".to_string()
    } else {
        values.join(" / ")
    }
}

// =========================================================
// 运行中屏
// =========================================================

fn draw_running(frame: &mut Frame, console: &Console) {
    let running = &console.running;
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let run_id = running.run_id();
    let title = if run_id.is_empty() {
        format!("矩阵运行 · {}", running.status_text())
    } else {
        format!("矩阵运行 · {} · {run_id}", running.status_text())
    };
    frame.render_widget(Paragraph::new(title).style(title_style()), chunks[0]);
    frame.render_widget(Paragraph::new(progress_line(running)), chunks[1]);
    frame.render_widget(Paragraph::new(condition_line(running)), chunks[2]);

    let conditions: Vec<Line> = running
        .conditions()
        .iter()
        .map(|entry| {
            Line::from(vec![
                Span::styled(
                    format!("{}{}", marker(entry.active), entry.id),
                    if entry.active {
                        title_style()
                    } else {
                        Style::new()
                    },
                ),
                Span::raw(format!(
                    "  {}/{}",
                    entry.completed + entry.failed,
                    entry.planned
                )),
                Span::styled(
                    format!("  失败 {}", entry.failed),
                    Style::new().fg(if entry.failed > 0 {
                        Color::Red
                    } else {
                        Color::DarkGray
                    }),
                ),
            ])
        })
        .collect();
    let conditions = if conditions.is_empty() {
        vec![Line::from(Span::styled(
            "（等待运行事件）",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        conditions
    };
    let title = match running.root() {
        Some(root) => format!("条件 · 输出 {}", root.display()),
        None => "条件".to_string(),
    };
    frame.render_widget(
        Paragraph::new(conditions).block(Block::bordered().title(title)),
        chunks[3],
    );

    let tail: Vec<Line> = running
        .tail()
        .map(|line| Line::from(line.to_string()))
        .collect();
    frame.render_widget(
        Paragraph::new(tail).block(Block::bordered().title("事件")),
        chunks[4],
    );

    frame.render_widget(footer(running_footer(running)), chunks[5]);
}

/// 进度行：题集规模、已定案条目、完成 / 失败、已用时间与剩余估计。
fn progress_line(running: &RunningState) -> String {
    let mut line = format!(
        "题集 {} 题 / 条目 {}/{}   完成 {} / 失败 {}   已用 {}   预计剩余 {}",
        running.total_items(),
        running.done_total(),
        running.total_executions(),
        running.completed(),
        running.failed(),
        running.elapsed_text(),
        running.eta_text()
    );
    if running.warnings() > 0 {
        line.push_str(&format!("   告警 {}", running.warnings()));
    }
    line
}

/// 条件行：当前条件序号与本题进度。
fn condition_line(running: &RunningState) -> String {
    let total = running.total_conditions();
    match running
        .condition_index()
        .and_then(|index| running.conditions().get(index).map(|entry| (index, entry)))
    {
        Some((index, entry)) => format!(
            "条件 {}/{}  {}  {}/{}  本次待跑 {}",
            index + 1,
            total,
            entry.id,
            entry.completed + entry.failed,
            entry.planned,
            entry.pending
        ),
        None => format!("条件 0/{total}  等待开始"),
    }
}

/// 页脚：运行中提示中断，结束态提示续跑与返回。
fn running_footer(running: &RunningState) -> String {
    if !running.is_finished() {
        if running.is_cancel_requested() {
            "中断中…等待在飞题完成  Ctrl+C 退出".to_string()
        } else {
            "Esc 中断  Ctrl+C 退出".to_string()
        }
    } else {
        "R 续跑（同一输出根）  Enter 进入审查  Esc 返回首页".to_string()
    }
}

// =========================================================
// 错误屏
// =========================================================

fn draw_error(frame: &mut Frame, console: &Console) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new("矩阵运行失败")
            .style(Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
        chunks[0],
    );

    let message = console
        .error
        .clone()
        .unwrap_or_else(|| "原因未知".to_string());
    frame.render_widget(
        Paragraph::new(message)
            .block(Block::bordered().title("错误"))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );

    frame.render_widget(footer("Esc 返回首页  q 退出"), chunks[2]);
}

// =========================================================
// 公共样式
// =========================================================

pub(super) fn title_style() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

pub(super) fn marker(active: bool) -> &'static str {
    if active { "> " } else { "  " }
}

pub(super) fn footer(text: impl Into<String>) -> Paragraph<'static> {
    Paragraph::new(text.into()).style(Style::new().fg(Color::DarkGray))
}

/// 一行「标题: 值」，标题固定宽度便于对齐。
pub(super) fn field(title: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{title}: "), Style::new().fg(Color::DarkGray)),
        Span::raw(value),
    ])
}
