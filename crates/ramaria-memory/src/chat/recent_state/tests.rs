//! crates/ramaria-memory/src/chat/recent_state/tests.rs - 近期状态判定模块单元测试
//!
//! 设计特点:
//! - 位于 chat::recent_state 模块内，经 use super::* 取用被测项。
//! - 由 recent_state.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。

use super::*;
use uuid::Uuid;

/// 固定基准时间（2023-11-14 22:13:20 UTC）。
const NOW: i64 = 1_700_000_000_000;

/// 构造带效价 / 显著性的 L1（发生时间显式指定）。
fn l1_at(valence: f64, salience: f64, occurred_at: i64) -> MemoryL1 {
    let mut m = MemoryL1::new(Uuid::nil(), "摘要".to_string(), None);
    m.valence = valence;
    m.salience = salience;
    m.created_at = occurred_at;
    m.occurred_at = Some(occurred_at);
    m
}

/// 空样本 → 不判定方向。
#[test]
fn empty_samples_yield_none() {
    assert_eq!(compute_recent_mood(&[], NOW, 7), None);
}

/// 窗口过滤：窗口外的样本不计入（否则方向会被翻转为负向）。
#[test]
fn samples_outside_window_are_excluded() {
    let inside = l1_at(0.5, 1.0, NOW - 1_000);
    let outside = l1_at(-0.9, 1.0, NOW - 8 * 86_400_000);
    assert_eq!(
        compute_recent_mood(&[inside, outside], NOW, 7),
        Some(RecentMood::Positive),
        "窗口外样本不应参与计算"
    );
}

/// 窗口边界：恰好等于 `now - window_days` 的样本计入（闭区间）。
#[test]
fn sample_on_window_boundary_is_counted() {
    let boundary = l1_at(0.6, 1.0, NOW - 7 * 86_400_000);
    assert_eq!(
        compute_recent_mood(&[boundary], NOW, 7),
        Some(RecentMood::Positive)
    );
}

/// 高显著性主导：加权方向与简单平均相反时以加权为准。
#[test]
fn high_salience_dominates_weighted_valence() {
    // 简单平均 = (0.8 - 0.8) / 2 = 0（若回退平均会得 None）
    let strong_positive = l1_at(0.8, 0.9, NOW);
    let weak_negative = l1_at(-0.8, 0.1, NOW);
    assert_eq!(
        compute_recent_mood(&[strong_positive, weak_negative], NOW, 7),
        Some(RecentMood::Positive),
        "高显著正向样本应主导方向"
    );

    let strong_negative = l1_at(-0.8, 0.9, NOW);
    let weak_positive = l1_at(0.8, 0.1, NOW);
    assert_eq!(
        compute_recent_mood(&[strong_negative, weak_positive], NOW, 7),
        Some(RecentMood::Negative),
        "高显著负向样本应主导方向"
    );
}

/// 中性带：窗口加权效价落在阈值带内 → 不注入。
#[test]
fn neutral_band_yields_none() {
    let mild_positive = l1_at(0.1, 1.0, NOW);
    let mild_negative = l1_at(-0.1, 1.0, NOW);
    assert_eq!(
        compute_recent_mood(&[mild_positive, mild_negative], NOW, 7),
        None
    );

    // 边界值：恰好 ±0.15 视为中性，超出即判定方向
    let on_boundary = l1_at(0.15, 1.0, NOW);
    assert_eq!(compute_recent_mood(&[on_boundary], NOW, 7), None);
    let over_boundary = l1_at(0.16, 1.0, NOW);
    assert_eq!(
        compute_recent_mood(&[over_boundary], NOW, 7),
        Some(RecentMood::Positive)
    );
}

/// 权重和为零（salience 全零）→ 回退简单平均。
#[test]
fn zero_weights_fall_back_to_simple_average() {
    let positive_a = l1_at(0.8, 0.0, NOW);
    let positive_b = l1_at(0.4, 0.0, NOW);
    assert_eq!(
        compute_recent_mood(&[positive_a, positive_b], NOW, 7),
        Some(RecentMood::Positive),
        "全零权重应回退简单平均（均值 0.6）"
    );

    let negative = l1_at(-0.7, 0.0, NOW);
    assert_eq!(
        compute_recent_mood(&[negative], NOW, 7),
        Some(RecentMood::Negative)
    );
}

/// 正负符号：单样本方向直接反映效价符号。
#[test]
fn sign_of_valence_decides_direction() {
    assert_eq!(
        compute_recent_mood(&[l1_at(0.5, 0.5, NOW)], NOW, 7),
        Some(RecentMood::Positive)
    );
    assert_eq!(
        compute_recent_mood(&[l1_at(-0.5, 0.5, NOW)], NOW, 7),
        Some(RecentMood::Negative)
    );
}

/// 发生时间缺失（None）时以 created_at 参与窗口过滤。
#[test]
fn missing_occurred_at_falls_back_to_created_at() {
    let mut m = l1_at(0.5, 1.0, NOW);
    m.occurred_at = None;
    m.created_at = NOW - 1_000;
    assert_eq!(
        compute_recent_mood(&[m], NOW, 7),
        Some(RecentMood::Positive)
    );
}
