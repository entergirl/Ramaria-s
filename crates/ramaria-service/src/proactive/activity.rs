//! crates/ramaria-service/src/proactive/activity.rs - Ramaria 用户活跃时段统计与软加权
//!
//! 设计特点:
//! - 以消息时间直方图建模活跃时段：软加权（非硬窗口），权重设下限防"永不触发"
//! - 用户模型（user 消息）与人格模型（消息级人格归属）同形态、同缓存模式、各自门槛
//! - 唤醒时刻统计：每日首条用户消息的本地分钟中位，供问候类候选判断时机窗口
//! - 时机权重合成：用户活跃权重与人格节律归一化强度按配置强度加权，人格侧为软信号
//! - 滚动窗口 + 样本门槛：样本不足退化不启用（门放行、权重中性）
//! - 直方图按本地小时归桶，缓存于状态键（按本地日期跨日刷新）

use std::collections::HashMap;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;

use super::state::{self, ProactiveState};

// =========================================================
// 活跃时段模型
// =========================================================

/// 活跃时段模型（消息时间直方图）。
///
/// 字段约定:
/// - `histogram`: 按本地小时（0~23）归桶的消息计数；
/// - `total`: 窗口内样本总量（低于门槛时不产出模型）；
/// - `wake_minute_median`: 用户唤醒时刻中位数（本地分钟；None = 无可统计日，
///   人格模型恒为 None）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityModel {
    pub histogram: [u32; 24],
    pub total: u32,
    pub wake_minute_median: Option<u32>,
}

/// 活跃时段门判定结果。
///
/// 变体:
/// - `NotModeled`: 样本不足，未建模（放行，权重中性）；
/// - `Pass { weight }`: 通过并附当前时段权重（供判据输入信号消费）；
/// - `LowWeight { norm }`: 当前小时活跃度低于门槛（本轮不启动）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ActivityGate {
    NotModeled,
    Pass { weight: f64 },
    LowWeight { norm: f64 },
}

// =========================================================
// 统计与判定
// =========================================================

/// 按本地小时归桶统计消息时间戳。
///
/// 参数:
/// - `times`: 消息时间戳列表（Unix 毫秒）。
///
/// 返回:
/// - 长度 24 的小时计数数组（下标 = 本地小时）。
pub(crate) fn compute_histogram(times: &[i64]) -> [u32; 24] {
    let mut histogram = [0u32; 24];
    for &ts in times {
        let hour = state::local_hour(ts) as usize;
        histogram[hour] += 1;
    }
    histogram
}

/// 当前小时的归一化活跃度（计数 / 峰值；无样本时为 0）。
pub(crate) fn hour_norm(histogram: &[u32; 24], hour: u32) -> f64 {
    let peak = histogram.iter().copied().max().unwrap_or(0);
    if peak == 0 {
        return 0.0;
    }
    let count = histogram.get(hour as usize).copied().unwrap_or(0);
    count as f64 / peak as f64
}

/// 计算用户唤醒时刻中位数（每日首条消息的本地分钟中位）。
///
/// 口径:
/// - 按本地日期分组取当日最早时间戳（输入升序时即每日首条）；
/// - 分钟取本地当日分钟数（0~1439）；超范围时间戳安全跳过；
/// - 中位数：奇数条取中间值；偶数条取中间两值平均（向下取整）。
///
/// 参数:
/// - `times`: 消息时间戳列表（Unix 毫秒）。
///
/// 返回:
/// - 无可统计日 → None；否则本地分钟中位数（0~1439）。
pub(crate) fn compute_wake_median(times: &[i64]) -> Option<u32> {
    let mut first_by_date: HashMap<String, i64> = HashMap::new();
    for &ts in times {
        let date = state::local_date_str(ts);
        if date.is_empty() {
            continue;
        }
        first_by_date
            .entry(date)
            .and_modify(|earliest| *earliest = (*earliest).min(ts))
            .or_insert(ts);
    }
    if first_by_date.is_empty() {
        return None;
    }
    let mut minutes: Vec<u32> = first_by_date
        .values()
        .map(|&ts| state::local_minute_of_day(ts))
        .collect();
    minutes.sort_unstable();
    let mid = minutes.len() / 2;
    if minutes.len().is_multiple_of(2) {
        Some((minutes[mid - 1] + minutes[mid]) / 2)
    } else {
        Some(minutes[mid])
    }
}

/// 时段权重下限：`1 - clamp(active_hours_weight, 0.0, 1.0)`。
///
/// 说明:
/// - 下限防「永不触发」；未建模（样本不足）时调用方以该值作中性权重。
pub(crate) fn weight_floor(active_hours_weight: f64) -> f64 {
    (1.0 - active_hours_weight.clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

/// 当前小时的时段权重：`weight_floor + 强度 × 归一化活跃度`（下限防信号归零）。
pub(crate) fn hour_weight(histogram: &[u32; 24], hour: u32, active_hours_weight: f64) -> f64 {
    let strength = active_hours_weight.clamp(0.0, 1.0);
    weight_floor(strength) + strength * hour_norm(histogram, hour)
}

/// 时机权重合成：用户活跃权重与人格节律归一化强度按强度系数合成。
///
/// 口径:
/// - `rhythm_norm` 为 None（人格样本不足未建模）→ 原样返回 `user_weight`
///   （中性因子，行为与仅用户活跃一致）；
/// - Some(norm)：`floor = 1 - clamp(rhythm_weight, 0.0, 1.0)`；
///   `factor = floor + (1 - floor) × clamp(norm, 0.0, 1.0)`；
///   返回 `user_weight × factor` 并夹取 [0.0, 1.0]（人格节律为软信号，
///   只衰减时机权重，不构成硬否决）。
pub(crate) fn combine_weight(
    user_weight: f64,
    rhythm_norm: Option<f64>,
    rhythm_weight: f64,
) -> f64 {
    let Some(norm) = rhythm_norm else {
        return user_weight;
    };
    let strength = rhythm_weight.clamp(0.0, 1.0);
    let floor = 1.0 - strength;
    let factor = floor + (1.0 - floor) * norm.clamp(0.0, 1.0);
    (user_weight * factor).clamp(0.0, 1.0)
}

/// 活跃时段门判定。
///
/// 口径:
/// - 无模型（样本不足）或无样本 → `NotModeled`（放行，权重中性）；
/// - 归一化活跃度低于门槛（`1 - 强度`，夹取 [0,1]）→ `LowWeight`（本轮不启动）；
/// - 否则 `Pass` 并附当前时段权重（供判据输入信号消费）。
pub(crate) fn evaluate_gate(
    model: Option<&ActivityModel>,
    hour: u32,
    active_hours_weight: f64,
) -> ActivityGate {
    let Some(model) = model else {
        return ActivityGate::NotModeled;
    };
    if model.total == 0 {
        return ActivityGate::NotModeled;
    }
    let strength = active_hours_weight.clamp(0.0, 1.0);
    let norm = hour_norm(&model.histogram, hour);
    let gate = weight_floor(strength);
    if norm < gate {
        return ActivityGate::LowWeight { norm };
    }
    ActivityGate::Pass {
        weight: hour_weight(&model.histogram, hour, strength),
    }
}

/// 加载用户活跃时段模型（带直方图与唤醒时刻缓存）。
///
/// 流程:
/// 1. 状态缓存存在且归属日期为今日 → 直接用缓存（含唤醒时刻字段）；
/// 2. 否则按滚动窗口查询 user 消息时间戳 → 归桶 + 统计唤醒中位
///    → 一次性写回状态缓存（含归属日期）；
/// 3. 样本总量低于门槛 → 返回 None（退化不启用）。
///
/// 兼容性:
/// - 旧版状态 JSON 无唤醒字段：缓存当日命中时唤醒中位为 None（当日问候不产出，
///   次日刷新自愈）。
///
/// 参数:
/// - `storage`: 存储后端（user 消息时间查询）。
/// - `state`: 运行时状态（直方图与唤醒时刻缓存读写）。
/// - `persona_uid`: 人格标识。
/// - `now`: 当前时间（Unix 毫秒）。
/// - `window_days`: 统计滚动窗口（天）。
/// - `min_samples`: 建模最少样本数。
pub(crate) async fn load_model(
    storage: &dyn StorageBackend,
    state: &mut ProactiveState,
    persona_uid: &str,
    now: i64,
    window_days: u32,
    min_samples: u32,
) -> RamariaResult<Option<ActivityModel>> {
    let today = state::local_date_str(now);
    let (histogram, wake_minute_median) = match state.hour_histogram {
        Some(histogram) if state.histogram_date == today => (histogram, state.wake_minute_median),
        _ => {
            let since = now.saturating_sub(window_days as i64 * 86_400_000);
            let times = storage
                .list_user_message_times_since(persona_uid, since)
                .await?;
            let histogram = compute_histogram(&times);
            let wake_minute_median = compute_wake_median(&times);
            state.hour_histogram = Some(histogram);
            state.histogram_date = today;
            state.wake_minute_median = wake_minute_median;
            (histogram, wake_minute_median)
        }
    };

    let total: u32 = histogram.iter().sum();
    if total < min_samples {
        return Ok(None);
    }
    Ok(Some(ActivityModel {
        histogram,
        total,
        wake_minute_median,
    }))
}

/// 加载人格活跃曲线模型（带直方图缓存）。
///
/// 流程:
/// 1. 状态缓存存在且归属日期为今日 → 直接用缓存；
/// 2. 否则按滚动窗口查询人格消息时间戳 → 归桶 → 写回状态缓存（含归属日期）；
/// 3. 样本总量低于门槛 → 返回 None（退化不启用；缓存照写）。
///
/// 说明:
/// - 统计输入按消息级人格归属（不区分角色：人格自身发言均计入）；
/// - 人格模型不统计唤醒时刻：返回模型的 `wake_minute_median` 恒为 None；
/// - 查询失败上抛（调用方兜底）。
///
/// 参数:
/// - `storage`: 存储后端（人格消息时间查询）。
/// - `state`: 运行时状态（人格直方图缓存读写）。
/// - `persona_uid`: 人格标识。
/// - `now`: 当前时间（Unix 毫秒）。
/// - `window_days`: 统计滚动窗口（天）。
/// - `min_samples`: 建模最少样本数。
pub(crate) async fn load_persona_model(
    storage: &dyn StorageBackend,
    state: &mut ProactiveState,
    persona_uid: &str,
    now: i64,
    window_days: u32,
    min_samples: u32,
) -> RamariaResult<Option<ActivityModel>> {
    let today = state::local_date_str(now);
    let histogram = match state.rhythm_histogram {
        Some(histogram) if state.rhythm_histogram_date == today => histogram,
        _ => {
            let since = now.saturating_sub(window_days as i64 * 86_400_000);
            let times = storage
                .list_persona_message_times_since(persona_uid, since)
                .await?;
            let histogram = compute_histogram(&times);
            state.rhythm_histogram = Some(histogram);
            state.rhythm_histogram_date = today;
            histogram
        }
    };

    let total: u32 = histogram.iter().sum();
    if total < min_samples {
        return Ok(None);
    }
    Ok(Some(ActivityModel {
        histogram,
        total,
        wake_minute_median: None,
    }))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
