//! rust/crates/ramaria-memory/src/inference/calibration.rs - 定期全量校准触发逻辑
//!
//! 设计特点:
//! - T-INF-014: 增量更新累积 10 轮后（或事件量翻倍）触发全量冷启动推断
//! - 校准触发后重置计数器，重新跑 +
//! - 与旧画像做全量差异对比，差异超过阈值时提示人工确认
//! - 纯逻辑，零 I/O，仅维护计数器状态

// =========================================================
// 配置类型
// =========================================================

/// 全量校准配置。
///
/// 职责:
/// - 管理校准触发条件和差异阈值。
#[derive(Debug, Clone)]
pub struct CalibrationConfig {
    /// 增量更新累积轮数触发阈值，默认 10
    pub round_threshold: u32,
    /// 事件量翻倍触发（当前事件数 ÷ 上次全量校准事件数 ≥ 此值）
    pub event_doubling_ratio: f64,
    /// 画像差异阈值：差异 trait 占比超过此值时提示人工确认，默认 0.3
    pub diff_alert_ratio: f64,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            round_threshold: 10,
            event_doubling_ratio: 2.0,
            diff_alert_ratio: 0.3,
        }
    }
}

// ---- v1.3 配置传播修复：从 ramaria-core 的可序列化配置创建 ----

impl From<ramaria_core::config::CalibrationConf> for CalibrationConfig {
    fn from(conf: ramaria_core::config::CalibrationConf) -> Self {
        Self {
            round_threshold: conf.round_threshold,
            event_doubling_ratio: conf.event_doubling_ratio,
            diff_alert_ratio: conf.diff_alert_ratio,
        }
    }
}

// =========================================================
// 校准状态
// =========================================================

/// 全量校准状态追踪器。
///
/// 职责:
/// - 跟踪自上次全量校准以来的增量更新轮数。
/// - 记录上次全量校准时的事件数（用于翻倍检测）。
///
/// 用法:
/// ```rust
/// use ramaria_memory::inference::calibration::{CalibrationTracker, CalibrationConfig};
/// let config = CalibrationConfig::default();
/// let mut tracker = CalibrationTracker::new(config, 100);
/// tracker.record_incremental_update();
/// assert!(!tracker.should_calibrate(105));
/// ```
#[derive(Debug, Clone)]
pub struct CalibrationTracker {
    config: CalibrationConfig,
    /// 自上次全量校准以来的增量更新轮数
    incremental_rounds: u32,
    /// 上次全量校准时的事件总数
    last_calibration_event_count: usize,
}

impl CalibrationTracker {
    /// 创建新的校准追踪器。
    ///
    /// 参数:
    /// - `config`: 校准配置。
    /// - `initial_event_count`: 初始事件数（通常为首次全量推断时的事件数）。
    pub fn new(config: CalibrationConfig, initial_event_count: usize) -> Self {
        Self {
            config,
            incremental_rounds: 0,
            last_calibration_event_count: initial_event_count,
        }
    }

    /// 记录一次增量更新。
    ///
    /// 用法:
    /// - 每次 增量更新完成后调用。
    pub fn record_incremental_update(&mut self) {
        self.incremental_rounds += 1;
    }

    /// 检查是否应触发全量校准。
    ///
    /// 触发条件（满足任一）:
    /// 1. 增量更新累积轮数 ≥ round_threshold（默认 10）
    /// 2. 当前事件总数 ≥ 上次全量校准事件数 × event_doubling_ratio（默认 2.0）
    ///
    /// 参数:
    /// - `current_event_count`: 当前事件总数。
    ///
    /// 返回:
    /// - true: 应触发全量校准。
    pub fn should_calibrate(&self, current_event_count: usize) -> bool {
        // 条件 1: 轮数阈值
        if self.incremental_rounds >= self.config.round_threshold {
            return true;
        }

        // 条件 2: 事件量翻倍
        if self.last_calibration_event_count > 0 {
            let ratio = current_event_count as f64 / self.last_calibration_event_count as f64;
            if ratio >= self.config.event_doubling_ratio {
                return true;
            }
        }

        false
    }

    /// 标记已完成全量校准，重置计数器。
    ///
    /// 参数:
    /// - `new_event_count`: 本次校准后的事件总数。
    pub fn mark_calibrated(&mut self, new_event_count: usize) {
        self.incremental_rounds = 0;
        self.last_calibration_event_count = new_event_count;
    }

    /// 返回当前增量更新轮数。
    pub fn round_count(&self) -> u32 {
        self.incremental_rounds
    }

    /// 返回上次校准时的事件数。
    pub fn last_event_count(&self) -> usize {
        self.last_calibration_event_count
    }
}

// =========================================================
// 画像差异对比
// =========================================================

/// 全量校准的差异对比结果。
#[derive(Debug, Clone)]
pub struct CalibrationDiff {
    /// 旧画像 trait 总数
    pub old_trait_count: usize,
    /// 新画像 trait 总数
    pub new_trait_count: usize,
    /// 新增 trait 数
    pub added_count: usize,
    /// 废弃 trait 数
    pub deprecated_count: usize,
    /// 保留 trait 数
    pub kept_count: usize,
    /// 差异比例 = (added + deprecated) / max(old_count, 1)
    pub diff_ratio: f64,
    /// 是否需要人工确认（diff_ratio > diff_alert_ratio）
    pub needs_manual_review: bool,
}

/// 计算全量校准前后画像的差异。
///
/// 策略:
/// - 基于 trait_label 精确匹配（与 inferrer 的后处理一致）。
/// - 差异比例 = (新增数 + 废弃数) / max(旧总数, 1)。
///
/// 参数:
/// - `old_labels`: 旧画像的 trait_label 集合。
/// - `new_labels`: 新画像的 trait_label 集合。
/// - `config`: 校准配置。
///
/// 返回:
/// - CalibrationDiff。
pub fn compute_calibration_diff(
    old_labels: &[String],
    new_labels: &[String],
    config: &CalibrationConfig,
) -> CalibrationDiff {
    use std::collections::HashSet;

    let old_set: HashSet<&str> = old_labels.iter().map(|s| s.as_str()).collect();
    let new_set: HashSet<&str> = new_labels.iter().map(|s| s.as_str()).collect();

    let added_count = new_set.difference(&old_set).count();
    let deprecated_count = old_set.difference(&new_set).count();
    let kept_count = old_set.intersection(&new_set).count();

    let old_count = old_labels.len();
    let new_count = new_labels.len();
    let diff_ratio = if old_count > 0 {
        (added_count + deprecated_count) as f64 / old_count as f64
    } else {
        0.0
    };
    let needs_manual_review = diff_ratio > config.diff_alert_ratio;

    CalibrationDiff {
        old_trait_count: old_count,
        new_trait_count: new_count,
        added_count,
        deprecated_count,
        kept_count,
        diff_ratio,
        needs_manual_review,
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- CalibrationTracker ----

    #[test]
    fn tracker_new_starts_at_zero() {
        let config = CalibrationConfig::default();
        let tracker = CalibrationTracker::new(config, 100);
        assert_eq!(tracker.round_count(), 0);
        assert_eq!(tracker.last_event_count(), 100);
    }

    #[test]
    fn tracker_should_calibrate_by_rounds() {
        let config = CalibrationConfig::default();
        let mut tracker = CalibrationTracker::new(config, 100);
        // 9 轮不应触发
        for _ in 0..9 {
            tracker.record_incremental_update();
        }
        assert!(!tracker.should_calibrate(100));
        // 第 10 轮应触发
        tracker.record_incremental_update();
        assert!(tracker.should_calibrate(100));
    }

    #[test]
    fn tracker_should_calibrate_by_doubling() {
        let config = CalibrationConfig::default();
        let tracker = CalibrationTracker::new(config, 100);
        // 事件量翻倍，应触发
        assert!(tracker.should_calibrate(200));
    }

    #[test]
    fn tracker_should_not_calibrate_below_thresholds() {
        let config = CalibrationConfig::default();
        let mut tracker = CalibrationTracker::new(config, 100);
        tracker.record_incremental_update();
        // 仅 1 轮，事件量未翻倍
        assert!(!tracker.should_calibrate(150));
    }

    #[test]
    fn tracker_mark_calibrated_resets() {
        let config = CalibrationConfig::default();
        let mut tracker = CalibrationTracker::new(config, 100);
        for _ in 0..10 {
            tracker.record_incremental_update();
        }
        assert!(tracker.should_calibrate(100));
        tracker.mark_calibrated(120);
        assert_eq!(tracker.round_count(), 0);
        assert_eq!(tracker.last_event_count(), 120);
        assert!(!tracker.should_calibrate(120));
    }

    #[test]
    fn tracker_doubling_with_zero_initial() {
        let config = CalibrationConfig::default();
        let tracker = CalibrationTracker::new(config, 0);
        // 初始事件数为 0 时不应因翻倍触发
        assert!(!tracker.should_calibrate(10));
    }

    // ---- CalibrationDiff ----

    #[test]
    fn calibration_diff_no_change() {
        let config = CalibrationConfig::default();
        let old = vec!["温和".to_string(), "幽默".to_string()];
        let new = vec!["温和".to_string(), "幽默".to_string()];
        let diff = compute_calibration_diff(&old, &new, &config);
        assert_eq!(diff.added_count, 0);
        assert_eq!(diff.deprecated_count, 0);
        assert_eq!(diff.kept_count, 2);
        assert!((diff.diff_ratio - 0.0).abs() < 1e-10);
        assert!(!diff.needs_manual_review);
    }

    #[test]
    fn calibration_diff_all_new() {
        let config = CalibrationConfig::default();
        let old: Vec<String> = vec![];
        let new = vec!["温和".to_string(), "幽默".to_string()];
        let diff = compute_calibration_diff(&old, &new, &config);
        assert_eq!(diff.added_count, 2);
        assert_eq!(diff.old_trait_count, 0);
        assert!((diff.diff_ratio - 0.0).abs() < 1e-10);
    }

    #[test]
    fn calibration_diff_needs_review() {
        let config = CalibrationConfig::default();
        let old = vec!["温和".to_string(), "幽默".to_string(), "尽责".to_string()];
        let new = vec!["激进".to_string(), "开放".to_string()];
        let diff = compute_calibration_diff(&old, &new, &config);
        // added=2, deprecated=3, diff_ratio=5/3≈1.67 > 0.3
        assert!(diff.needs_manual_review);
        assert!(diff.diff_ratio > 0.3);
    }

    #[test]
    fn calibration_diff_partial_change() {
        let config = CalibrationConfig::default();
        let old = vec![
            "温和".to_string(),
            "幽默".to_string(),
            "尽责".to_string(),
            "社交".to_string(),
            "理性".to_string(),
        ];
        let new = vec!["温和".to_string(), "幽默".to_string(), "冲动".to_string()];
        let diff = compute_calibration_diff(&old, &new, &config);
        // kept=2 (温和, 幽默), added=1 (冲动), deprecated=3 (尽责, 社交, 理性)
        assert_eq!(diff.kept_count, 2);
        assert_eq!(diff.added_count, 1);
        assert_eq!(diff.deprecated_count, 3);
        // diff_ratio = 4/5 = 0.8 > 0.3
        assert!(diff.needs_manual_review);
    }
}
