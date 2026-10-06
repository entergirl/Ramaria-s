//! crates/ramaria-cli/src/commands/probe/baseline/tests.rs - 数值基线探针渲染单元测试
//!
//! 设计特点:
//! - 渲染口径用例不依赖引擎：用合成报告验证文本形态与数值格式
//! - 覆盖零数据（比率为 `-`）与有数据（计数 / 比率 / 中位 / 按日）两类形态
//! - 数值格式口径：比率一位小数百分比、时长一位小数小时、空值统一 `-`

use super::*;
use ramaria_core::config::ProactiveConfig;
use ramaria_service::proactive::{
    ProactiveDailyCount, ProactiveGlobalStats, ProactivePersonaStats, ProactiveStatsTotals,
};

/// 合成报告：一个带投递 / 回应 / 判据计数的人格。
fn sample_report() -> ProactiveStatsReport {
    ProactiveStatsReport {
        generated_at: 1_700_000_000_000,
        window_hours: 24,
        config: ProactiveConfig::default(),
        global: ProactiveGlobalStats {
            daily_total_limit: 0,
            daily_count: 1,
            daily_date: "2026-10-06".to_string(),
        },
        personas: vec![ProactivePersonaStats {
            uid: "char-0001".to_string(),
            name: "测试人格".to_string(),
            kind: "char".to_string(),
            deliveries: 4,
            responded: 1,
            response_rate: Some(0.25),
            median_response_ms: Some(90 * 60_000),
            daily: vec![
                ProactiveDailyCount {
                    date: "2026-10-04".to_string(),
                    count: 1,
                },
                ProactiveDailyCount {
                    date: "2026-10-05".to_string(),
                    count: 3,
                },
            ],
            last_sent_at: Some(1_700_000_000_000),
            daily_count: 1,
            daily_date: "2026-10-06".to_string(),
            silence_streak: 2,
            judge_yes_count: 3,
            judge_no_count: 5,
            last_judge_at: None,
            first_seen_at: None,
        }],
        totals: ProactiveStatsTotals {
            personas: 1,
            deliveries: 4,
            responded: 1,
            response_rate: Some(0.25),
            median_response_ms: Some(90 * 60_000),
            judge_yes_count: 3,
            judge_no_count: 5,
        },
    }
}

/// 数值格式口径：比率 / 时长 / 窗口 / 空值。
#[test]
fn format_helpers_follow_display_contract() {
    assert_eq!(rate_text(Some(0.5)), "50.0%");
    assert_eq!(rate_text(Some(0.0)), "0.0%");
    assert_eq!(rate_text(None), "-");
    assert_eq!(duration_text(Some(90 * 60_000)), "1.5h");
    assert_eq!(duration_text(None), "-");
    assert_eq!(window_text(0), "不限");
    assert_eq!(window_text(24), "24h");
    assert_eq!(quiet_hours_text("  "), "无");
    assert_eq!(quiet_hours_text("22:00-08:00"), "22:00-08:00");
    assert_eq!(local_time_text(None), "-");
    // 有效时间戳按本地时区渲染（不锁定具体时区，只验证形态）
    assert_eq!(
        local_time_text(Some(1_700_000_000_000)).len(),
        "YYYY-MM-DD HH:MM".len()
    );
}

/// 文本报告：分组结构、计数与按日分桶均可见。
#[test]
fn render_text_contains_counts_and_daily_buckets() {
    let text = render_text(&sample_report());

    assert!(text.contains("回应窗口 24h"), "应带回应窗口口径: {text}");
    assert!(text.contains("人格 char-0001（char）"), "应按人格分组");
    assert!(
        text.contains("投递 4 条 · 已回应 1 条（25.0%）"),
        "应带投递与回应计数"
    );
    assert!(text.contains("回应中位 1.5h"), "应带回应延迟中位");
    assert!(text.contains("退避 2"), "应带退避计数");
    assert!(text.contains("判据: yes 3 / no 5"), "应带判据计数");
    assert!(
        text.contains("按日: 2026-10-04=1 2026-10-05=3"),
        "应按日分桶输出"
    );
    assert!(
        text.contains("合计: 人格 1 个 · 投递 4 条 · 已回应 1 条（25.0%）"),
        "应带全人格合计"
    );
}

/// 零数据形态：比率为 `-`，无投递人格不输出按日行。
#[test]
fn render_text_handles_zero_data() {
    let mut report = sample_report();
    report.personas[0].deliveries = 0;
    report.personas[0].responded = 0;
    report.personas[0].response_rate = None;
    report.personas[0].median_response_ms = None;
    report.personas[0].daily.clear();
    report.totals.deliveries = 0;
    report.totals.responded = 0;
    report.totals.response_rate = None;
    report.totals.median_response_ms = None;

    let text = render_text(&report);
    assert!(
        text.contains("投递 0 条 · 已回应 0 条（-）· 回应中位 -"),
        "零数据应按 `-` 降级: {text}"
    );
    assert!(!text.contains("按日:"), "无投递不应输出按日行");
}
