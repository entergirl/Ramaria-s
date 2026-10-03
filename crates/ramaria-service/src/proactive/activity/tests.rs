//! crates/ramaria-service/src/proactive/activity/tests.rs - 活跃时段统计与软加权单元测试
//!
//! 设计特点:
//! - 覆盖归桶 / 归一化 / 权重下限 / 门判定边界（严格小于语义）/ 缓存与样本门槛
//! - 归桶与缓存用例用固定本地时刻构造时间戳，不依赖真实当前时间
//! - 缓存与样本门槛用例使用真实 SQLite 临时库（会话归属按 `sessions.persona_uid`）

use super::*;
use crate::test_support::{engine_with_db, seed_persona, seed_session_with_messages};
use chrono::TimeZone;

/// 浮点近似断言（存活期计算含乘加，容差 1e-9）。
fn approx(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "期望 {expected}，实际 {actual}"
    );
}

/// 构造固定本地时刻的时间戳（毫秒）。
fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
    chrono::Local
        .with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("固定本地时刻应可表示")
        .timestamp_millis()
}

/// 按本地小时归桶：不同小时的样本各自计数，同日同小时累计。
#[test]
fn compute_histogram_buckets_by_local_hour() {
    let ten_thirty = local_ms(2026, 1, 15, 10, 30);
    let eleven = local_ms(2026, 1, 15, 11, 5);

    let histogram = compute_histogram(&[ten_thirty, ten_thirty + 1_000, eleven]);
    assert_eq!(histogram[10], 2, "10 点应累计两条");
    assert_eq!(histogram[11], 1, "11 点应计一条");
    assert_eq!(histogram.iter().sum::<u32>(), 3, "总量应与样本数一致");
    assert_eq!(compute_histogram(&[]), [0u32; 24], "空样本应全零");
}

/// 归一化与权重下限：峰值 1.0 / 零值 0.0；权重下限 = 1 - 强度；强度边界 0 与 1。
#[test]
fn hour_norm_and_weight_floor() {
    let mut histogram = [0u32; 24];
    histogram[9] = 4;
    histogram[10] = 2;

    approx(hour_norm(&histogram, 9), 1.0);
    approx(hour_norm(&histogram, 10), 0.5);
    approx(hour_norm(&histogram, 11), 0.0);

    // 权重下限 = 1 - 强度：零计数小时取到下限
    approx(hour_weight(&histogram, 9, 0.8), 1.0);
    approx(hour_weight(&histogram, 11, 0.8), 0.2);
    // 强度 0：权重恒为下限 1.0；强度 1：权重等于归一化活跃度
    approx(hour_weight(&histogram, 11, 0.0), 1.0);
    approx(hour_weight(&histogram, 10, 1.0), 0.5);

    // 无样本直方图：归一化为 0，权重取下限
    let empty = [0u32; 24];
    approx(hour_norm(&empty, 0), 0.0);
    approx(hour_weight(&empty, 0, 0.8), 0.2);

    // 下限口径一致性：独立函数与 hour_weight 的零计数小时一致，越界强度按边界夹取
    approx(hour_weight(&empty, 0, 0.8), weight_floor(0.8));
    approx(weight_floor(0.8), 0.2);
    approx(weight_floor(0.0), 1.0);
    approx(weight_floor(1.0), 0.0);
    approx(weight_floor(1.5), 0.0);
}

/// 门判定：未建模 / 低活跃度 / 峰值通过；严格小于语义的强度 0 边界。
#[test]
fn evaluate_gate_states() {
    let mut histogram = [0u32; 24];
    histogram[10] = 10; // 峰值
    histogram[11] = 1; // 低活跃
    let model = ActivityModel {
        histogram,
        total: 11,
    };

    assert!(
        matches!(evaluate_gate(None, 10, 0.8), ActivityGate::NotModeled),
        "无模型放行"
    );
    let empty = ActivityModel {
        histogram: [0u32; 24],
        total: 0,
    };
    assert!(
        matches!(
            evaluate_gate(Some(&empty), 0, 0.8),
            ActivityGate::NotModeled
        ),
        "无样本模型视同未建模"
    );

    match evaluate_gate(Some(&model), 11, 0.8) {
        ActivityGate::LowWeight { norm } => approx(norm, 0.1),
        other => panic!("低活跃度应为 LowWeight，实际 {other:?}"),
    }
    match evaluate_gate(Some(&model), 10, 0.8) {
        ActivityGate::Pass { weight } => approx(weight, 1.0),
        other => panic!("峰值小时应通过，实际 {other:?}"),
    }

    // 强度 0：门槛 1.0；非峰值小时（norm < 1.0）不通过
    match evaluate_gate(Some(&model), 11, 0.0) {
        ActivityGate::LowWeight { .. } => {}
        other => panic!("强度 0 的非峰值小时应不通过，实际 {other:?}"),
    }
    // 强度 0：峰值小时 norm = 1.0，`1.0 < 1.0` 为 false → 通过（严格小于边界）
    match evaluate_gate(Some(&model), 10, 0.0) {
        ActivityGate::Pass { weight } => approx(weight, 1.0),
        other => panic!("强度 0 的峰值小时应通过，实际 {other:?}"),
    }
    // 强度 1：门槛 0.0，任意小时通过；零计数小时权重为 0.0
    match evaluate_gate(Some(&model), 5, 1.0) {
        ActivityGate::Pass { weight } => approx(weight, 0.0),
        other => panic!("强度 1 应全通过，实际 {other:?}"),
    }
}

/// 缓存与样本门槛：首次统计写回缓存；当日缓存命中不重复查询；样本不足退化不启用。
#[tokio::test]
async fn load_model_caches_and_respects_sample_floor() {
    let (_engine, storage, dir) = engine_with_db("proactive-activity").await;
    seed_persona(&storage, "char-0001").await;

    // 10 点 2 条 user 消息 + 15 点 1 条 user 消息（角色按 user / assistant 交替）
    let day1 = local_ms(2026, 1, 15, 10, 30);
    let day2 = local_ms(2026, 1, 16, 15, 0);
    seed_session_with_messages(&storage, "char-0001", 3, day1).await;
    seed_session_with_messages(&storage, "char-0001", 2, day2).await;
    let now = day2 + 3_600_000;

    let mut state = ProactiveState::default();
    let model = load_model(storage.as_ref(), &mut state, "char-0001", now, 30, 1)
        .await
        .expect("加载应成功")
        .expect("样本充足应产出模型");
    assert_eq!(model.histogram[10], 2, "10 点应有两条");
    assert_eq!(model.histogram[15], 1, "15 点应有一条");
    assert_eq!(model.total, 3);
    assert_eq!(
        state.hour_histogram,
        Some(model.histogram),
        "直方图应写回状态缓存"
    );
    assert_eq!(
        state.histogram_date,
        state::local_date_str(now),
        "缓存应记录归属日期"
    );

    // 当日缓存命中：直接改缓存值后再次加载 → 复用缓存而不重新查询
    state.hour_histogram = Some([7; 24]);
    let cached = load_model(storage.as_ref(), &mut state, "char-0001", now, 30, 1)
        .await
        .expect("加载应成功")
        .expect("缓存样本充足应产出模型");
    assert_eq!(cached.histogram, [7; 24], "当日缓存命中应直接复用");
    assert_eq!(cached.total, 168);

    // 样本门槛：新状态（无缓存）重新统计，样本数低于门槛 → None（统计仍写回缓存）
    let mut fresh = ProactiveState::default();
    let none = load_model(storage.as_ref(), &mut fresh, "char-0001", now, 30, 100)
        .await
        .expect("加载应成功");
    assert!(none.is_none(), "样本低于门槛应退化不启用");
    assert_eq!(
        fresh.hour_histogram,
        Some(model.histogram),
        "统计应写入缓存"
    );

    let _ = std::fs::remove_dir_all(dir);
}
