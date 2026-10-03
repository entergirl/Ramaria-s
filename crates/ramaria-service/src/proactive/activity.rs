//! crates/ramaria-service/src/proactive/activity.rs - Ramaria 用户活跃时段统计与软加权
//!
//! 设计特点:
//! - 以 user 消息时间直方图建模活跃时段：软加权（非硬窗口），权重设下限防"永不触发"
//! - 滚动窗口 + 样本门槛：样本不足退化不启用（门放行、权重中性）
//! - 直方图按本地小时归桶，缓存于状态键（按本地日期跨日刷新）
//! - 统计输入为时间戳列表（存储层只计 user 消息、按画像会话归属）

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::StorageBackend;

use super::state::{self, ProactiveState};

// =========================================================
// 活跃时段模型
// =========================================================

/// 用户活跃时段模型（user 消息时间直方图）。
///
/// 字段约定:
/// - `histogram`: 按本地小时（0~23）归桶的消息计数；
/// - `total`: 窗口内样本总量（低于门槛时不产出模型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityModel {
    pub histogram: [u32; 24],
    pub total: u32,
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

/// 当前小时的时段权重：`(1 - 强度) + 强度 × 归一化活跃度`（下限 = 1 - 强度，防信号归零）。
pub(crate) fn hour_weight(histogram: &[u32; 24], hour: u32, active_hours_weight: f64) -> f64 {
    let strength = active_hours_weight.clamp(0.0, 1.0);
    (1.0 - strength) + strength * hour_norm(histogram, hour)
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
    let gate = (1.0 - strength).clamp(0.0, 1.0);
    if norm < gate {
        return ActivityGate::LowWeight { norm };
    }
    ActivityGate::Pass {
        weight: hour_weight(&model.histogram, hour, strength),
    }
}

/// 加载活跃时段模型（带直方图缓存）。
///
/// 流程:
/// 1. 状态缓存存在且归属日期为今日 → 直接用缓存；
/// 2. 否则按滚动窗口查询 user 消息时间戳 → 归桶 → 写回状态缓存（含归属日期）；
/// 3. 样本总量低于门槛 → 返回 None（退化不启用）。
///
/// 参数:
/// - `storage`: 存储后端（user 消息时间查询）。
/// - `state`: 运行时状态（直方图缓存读写）。
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
    let histogram = match state.hour_histogram {
        Some(histogram) if state.histogram_date == today => histogram,
        _ => {
            let since = now.saturating_sub(window_days as i64 * 86_400_000);
            let times = storage
                .list_user_message_times_since(persona_uid, since)
                .await?;
            let histogram = compute_histogram(&times);
            state.hour_histogram = Some(histogram);
            state.histogram_date = today;
            histogram
        }
    };

    let total: u32 = histogram.iter().sum();
    if total < min_samples {
        return Ok(None);
    }
    Ok(Some(ActivityModel { histogram, total }))
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
