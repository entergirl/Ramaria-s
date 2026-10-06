//! crates/ramaria-cli/src/commands/probe/baseline.rs - 主动对话数值基线探针
//!
//! 设计特点:
//! - 只读统计：口径与聚合在服务层单点实现（`Engine::proactive_stats`），本层只做调用与渲染
//! - 文本形态按人格分组呈现投递 / 回应 / 判据计数；`--json` 输出完整报告信封
//! - 时间戳按本地时区渲染，与按日分桶的本地日期口径一致
//! - 隐私：只呈现计数与时间戳，不读取消息内容

use std::sync::Arc;

use chrono::{Local, LocalResult, TimeZone};
use ramaria_service::{Engine, ProactiveStatsReport};

/// 执行数值基线统计（只读）并输出。
///
/// 参数:
/// - `engine`: 服务层引擎（存储句柄来源）。
/// - `window_hours`: 回应判定窗口（小时；0 = 不设上界）。
/// - `json`: 输出 `--json` 信封（完整报告）而非文本表格。
///
/// 返回:
/// - `Ok(())`: 已输出；统计查询失败时返回错误（错误文案由入口统一映射）。
pub(crate) async fn run_baseline(
    engine: &Arc<Engine>,
    window_hours: u32,
    json: bool,
) -> anyhow::Result<()> {
    let report = engine
        .proactive_stats(window_hours)
        .await
        .map_err(|e| anyhow::Error::new(e).context("采集主动对话数值基线失败"))?;

    if json {
        return crate::json::emit_ok(&report);
    }
    print!("{}", render_text(&report));
    Ok(())
}

// =========================================================
// 文本渲染
// =========================================================

/// 渲染文本报告（stdout 只输出数据）。
fn render_text(report: &ProactiveStatsReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "主动对话数值基线：{}（回应窗口 {}）\n",
        local_time_text(Some(report.generated_at)),
        window_text(report.window_hours)
    ));
    out.push_str("  说明: 回应 = 窗口内该人格首条本地用户消息（排除导入）；窗口 0h = 不限\n");

    let config = &report.config;
    out.push_str(&format!(
        "配置口径: enabled={} 每日上限={} 合计日上限={} 冷却={}h 最小空闲={}h 判据={}/节流{}h 宽限={}d 退避={}d 免打扰={}\n",
        config.enabled,
        config.daily_limit,
        config.daily_total_limit,
        config.cooldown_hours,
        config.min_idle_hours,
        if config.judge_enabled { "on" } else { "off" },
        config.judge_interval_hours,
        config.startup_grace_days,
        config.silence_backoff_days,
        quiet_hours_text(&config.quiet_hours),
    ));
    out.push_str(&format!(
        "全局记账: 当日 {} 条（{}）\n",
        report.global.daily_count,
        if report.global.daily_date.is_empty() {
            "-".to_string()
        } else {
            report.global.daily_date.clone()
        }
    ));

    for row in &report.personas {
        out.push_str(&format!("\n人格 {}（{}）\n", row.uid, row.kind));
        out.push_str(&format!(
            "  投递 {} 条 · 已回应 {} 条（{}）· 回应中位 {}\n",
            row.deliveries,
            row.responded,
            rate_text(row.response_rate),
            duration_text(row.median_response_ms)
        ));
        out.push_str(&format!(
            "  状态: 最近投递 {} · 当日 {} 条 · 退避 {}\n",
            local_time_text(row.last_sent_at),
            row.daily_count,
            row.silence_streak
        ));
        out.push_str(&format!(
            "  判据: yes {} / no {} · 最近 {} · 宽限基准 {}\n",
            row.judge_yes_count,
            row.judge_no_count,
            local_time_text(row.last_judge_at),
            local_time_text(row.first_seen_at)
        ));
        if !row.daily.is_empty() {
            let daily = row
                .daily
                .iter()
                .map(|bucket| format!("{}={}", bucket.date, bucket.count))
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("  按日: {daily}\n"));
        }
    }

    out.push_str(&format!(
        "\n合计: 人格 {} 个 · 投递 {} 条 · 已回应 {} 条（{}）· 回应中位 {} · 判据 yes {} / no {}\n",
        report.totals.personas,
        report.totals.deliveries,
        report.totals.responded,
        rate_text(report.totals.response_rate),
        duration_text(report.totals.median_response_ms),
        report.totals.judge_yes_count,
        report.totals.judge_no_count
    ));
    out
}

/// 回应窗口文本（0 = 不限）。
fn window_text(hours: u32) -> String {
    if hours == 0 {
        "不限".to_string()
    } else {
        format!("{hours}h")
    }
}

/// 免打扰时段文本（空串 = 未设置）。
fn quiet_hours_text(quiet_hours: &str) -> &str {
    let trimmed = quiet_hours.trim();
    if trimmed.is_empty() { "无" } else { trimmed }
}

/// 本地时间文本（`YYYY-MM-DD HH:MM`；空值 / 超范围时间戳显示 `-`）。
fn local_time_text(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "-".to_string();
    };
    match Local.timestamp_millis_opt(ms) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => {
            dt.format("%Y-%m-%d %H:%M").to_string()
        }
        LocalResult::None => "-".to_string(),
    }
}

/// 比率文本（`41.7%`；无样本显示 `-`）。
fn rate_text(rate: Option<f64>) -> String {
    match rate {
        Some(rate) => format!("{:.1}%", rate * 100.0),
        None => "-".to_string(),
    }
}

/// 时长文本（小时，一位小数；无样本显示 `-`）。
fn duration_text(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => format!("{:.1}h", ms as f64 / 3_600_000.0),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests;
