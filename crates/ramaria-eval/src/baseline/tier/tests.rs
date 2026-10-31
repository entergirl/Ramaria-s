//! crates/ramaria-eval/src/baseline/tier/tests.rs - Ramaria 评测基线档位测试
//!
//! 设计特点:
//! - 五个档位的用户数、窗口与跨度逐项断言，锁定固定常量
//! - `all` 的构建顺序与检查点单独断言：u1 / u5 复用全年库的窗口
//! - 窗口判定覆盖两端边界与越界一天
//! - 样本选择断言前缀语义与不足报错
//! - 夹具只构造样本外壳，不读语料文件

use chrono::NaiveDate;
use ramaria_core::error::RamariaError;

use super::*;

// =========================================================
// 夹具
// =========================================================

/// 构造一个只有身份字段的样本。
fn sample(sample_id: &str, user_key: &str) -> Sample {
    Sample {
        sample_id: sample_id.to_string(),
        user_key: user_key.to_string(),
        speaker_a: "甲".to_string(),
        speaker_b: "乙".to_string(),
        sessions: Vec::new(),
        qa: Vec::new(),
    }
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("夹具日期应合法")
}

// =========================================================
// 解析
// =========================================================

/// 五个档位标识解析为用户数与跨度。
#[test]
fn parse_tier_accepts_five_ids() {
    let cases = [
        ("u1-m1", 1usize, 31i64, date(2025, 12, 1)),
        ("u1-q1", 1, 92, date(2025, 10, 1)),
        ("u1-y1", 1, 365, date(2025, 1, 1)),
        ("u5-y1", 5, 365, date(2025, 1, 1)),
        ("u10-y1", 10, 365, date(2025, 1, 1)),
    ];
    for (raw, users, span, start) in cases {
        match parse_tier(raw).expect("已登记档位应可解析") {
            TierSelection::One(spec) => {
                assert_eq!(spec.id, raw);
                assert_eq!(spec.users, users, "用户数不符: {raw}");
                assert_eq!(spec.span_days(), span, "跨度不符: {raw}");
                assert_eq!(spec.memory_start, start, "起点不符: {raw}");
                assert_eq!(spec.memory_end, date(2025, 12, 31), "终点固定为年末");
                assert_eq!(spec.query_end, date(2026, 1, 15), "查询截止日固定");
            }
            other => panic!("应解析为单档位: {other:?}"),
        }
    }
}

/// 大小写与空白不敏感；`all` 解析为全量计划。
#[test]
fn parse_tier_accepts_all_and_normalizes_case() {
    assert_eq!(
        parse_tier(" ALL ").expect("all 应可解析"),
        TierSelection::All
    );
    assert!(matches!(
        parse_tier("U1-Y1").expect("大小写不敏感"),
        TierSelection::One(spec) if spec.id == "u1-y1"
    ));
}

/// 未知档位返回校验错误并列出合法取值。
#[test]
fn parse_tier_rejects_unknown() {
    let error = parse_tier("u2-y1").expect_err("未知档位应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(error.to_string().contains("u1-m1"), "文案应列合法档位");
}

// =========================================================
// 构建计划
// =========================================================

/// `all` 先建单月与季度，再在一次全年构建中落三个检查点。
#[test]
fn all_plan_orders_steps_and_checkpoints() {
    let plan = TierSelection::All.plan();
    assert_eq!(plan.len(), 3, "all 应为三步建库");

    assert_eq!(plan[0].window.id, "u1-m1");
    assert_eq!(plan[0].sample_count, 1);
    assert_eq!(plan[0].checkpoints.len(), 1);
    assert_eq!(plan[0].checkpoints[0].tier.id, "u1-m1");

    assert_eq!(plan[1].window.id, "u1-q1");
    assert_eq!(plan[1].sample_count, 1);

    let yearly = &plan[2];
    assert_eq!(yearly.window.id, "u10-y1");
    assert_eq!(yearly.sample_count, 10, "全年一次灌入十个样本");
    let checkpoints: Vec<(usize, &str)> = yearly
        .checkpoints
        .iter()
        .map(|checkpoint| (checkpoint.after_samples, checkpoint.tier.id))
        .collect();
    assert_eq!(
        checkpoints,
        vec![(1, "u1-y1"), (5, "u5-y1"), (10, "u10-y1")],
        "全年检查点应与样本完成数对齐"
    );
    for checkpoint in &yearly.checkpoints {
        assert_eq!(
            checkpoint.tier.memory_start, yearly.window.memory_start,
            "检查点复用同一条全年库"
        );
        assert_eq!(checkpoint.tier.query_end, yearly.window.query_end);
    }
}

/// 单档位只建一次、只落一个检查点。
#[test]
fn single_tier_plan_has_one_checkpoint() {
    let plan = TierSelection::One(U5_Y1).plan();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].sample_count, 5);
    assert_eq!(plan[0].checkpoints.len(), 1);
    assert_eq!(plan[0].checkpoints[0].after_samples, 5);
    assert_eq!(plan[0].checkpoints[0].tier.id, "u5-y1");
}

// =========================================================
// 样本选择与窗口
// =========================================================

/// 样本选择取语料顺序前缀。
#[test]
fn select_samples_keeps_corpus_order() {
    let samples = vec![
        sample("甲", "jia"),
        sample("乙", "yi"),
        sample("丙", "bing"),
    ];
    let selected = select_samples(&samples, 2).expect("样本充足应成功");
    assert_eq!(
        selected
            .iter()
            .map(|s| s.sample_id.as_str())
            .collect::<Vec<_>>(),
        vec!["甲", "乙"]
    );
}

/// 样本不足返回校验错误。
#[test]
fn select_samples_rejects_short_corpus() {
    let samples = vec![sample("甲", "jia")];
    let error = select_samples(&samples, 2).expect_err("样本不足应报错");
    assert!(
        error.to_string().contains("样本不足"),
        "文案应说明样本不足，实际 {error}"
    );
}

/// 消息窗口按建库窗口两端闭区间判定。
#[test]
fn includes_date_covers_build_window() {
    assert!(U1_M1.includes_date(date(2025, 12, 1)));
    assert!(U1_M1.includes_date(date(2025, 12, 31)));
    assert!(!U1_M1.includes_date(date(2025, 11, 30)));
    assert!(!U1_M1.includes_date(date(2026, 1, 1)));
    assert!(!U1_Q1.includes_date(date(2025, 9, 30)));
    assert!(U1_Q1.includes_date(date(2025, 10, 1)));
}

/// 提问窗口止于查询截止日，允许晚于建库终点。
#[test]
fn includes_question_covers_query_window() {
    assert!(U1_M1.includes_question(date(2025, 12, 1)));
    assert!(U1_M1.includes_question(date(2026, 1, 15)));
    assert!(!U1_M1.includes_question(date(2025, 11, 30)));
    assert!(!U1_M1.includes_question(date(2026, 1, 16)));
}

/// 基线标识由语料、版本、语言与档位拼成。
#[test]
fn baseline_id_joins_parts() {
    assert_eq!(
        U1_Y1.baseline_id("lifebench", "2.0", "zh"),
        "lifebench-2.0-zh-u1-y1"
    );
}
