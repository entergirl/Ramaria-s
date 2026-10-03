//! crates/ramaria-core/src/time_period.rs - Ramaria 本地时段映射工具
//!
//! 设计特点:
//! - 零 I/O 纯函数：只按传入的本地小时数值映射，不读取系统时钟
//! - 六时段中文词汇与 L1 摘要 `time_period` 字段严格对齐，供跨模块同口径引用
//! - 映射区间：23-4 深夜、5-7 清晨、8-11 上午、12-16 下午、17-19 傍晚、20-22 夜间
//! - 防御取模：越界小时先对 24 归一，任意 `u32` 输入均不 panic

// =========================================================
// 时段枚举
// =========================================================

/// 一天内的六时段（与 L1 摘要 `time_period` 同一词汇体系）。
///
/// 变体与中文词对应（按本地小时映射）:
/// - [`TimePeriod::EarlyMorning`] → `清晨`（5-7 时）
/// - [`TimePeriod::Morning`] → `上午`（8-11 时）
/// - [`TimePeriod::Afternoon`] → `下午`（12-16 时）
/// - [`TimePeriod::Evening`] → `傍晚`（17-19 时）
/// - [`TimePeriod::Night`] → `夜间`（20-22 时）
/// - [`TimePeriod::LateNight`] → `深夜`（23-4 时）
///
/// 缺省值:
/// - `Default` 取 [`TimePeriod::EarlyMorning`]（枚举首项），仅用于缺省构造；
///   实际使用路径均经 [`TimePeriod::from_local_hour`] 显式传入当前时段。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TimePeriod {
    /// 清晨
    #[default]
    EarlyMorning,
    /// 上午
    Morning,
    /// 下午
    Afternoon,
    /// 傍晚
    Evening,
    /// 夜间
    Night,
    /// 深夜
    LateNight,
}

impl TimePeriod {
    /// 由本地小时映射时段。
    ///
    /// 映射（按归一后的 0-23 小时）:
    /// - 23 / 0-4 → [`TimePeriod::LateNight`]
    /// - 5-7 → [`TimePeriod::EarlyMorning`]
    /// - 8-11 → [`TimePeriod::Morning`]
    /// - 12-16 → [`TimePeriod::Afternoon`]
    /// - 17-19 → [`TimePeriod::Evening`]
    /// - 20-22 → [`TimePeriod::Night`]
    ///
    /// 参数:
    /// - `hour`: 本地小时；0-23 之外的值先对 24 取模归一（不 panic）。
    ///
    /// 返回:
    /// - 对应时段。
    pub fn from_local_hour(hour: u32) -> TimePeriod {
        match hour % 24 {
            // 23 与 0-4：跨零点的深夜
            23 | 0..=4 => TimePeriod::LateNight,
            5..=7 => TimePeriod::EarlyMorning,
            8..=11 => TimePeriod::Morning,
            12..=16 => TimePeriod::Afternoon,
            17..=19 => TimePeriod::Evening,
            // 剩余仅 20-22
            _ => TimePeriod::Night,
        }
    }

    /// 时段的稳定中文词。
    ///
    /// 说明:
    /// - 六个取值与 L1 摘要 `time_period` 的合法值（清晨 / 上午 / 下午 /
    ///   傍晚 / 夜间 / 深夜）严格一致，供 prompt 语境文案与记忆摘要同词对齐。
    ///
    /// 返回:
    /// - 六个中文词之一。
    pub fn as_str(self) -> &'static str {
        match self {
            TimePeriod::EarlyMorning => "清晨",
            TimePeriod::Morning => "上午",
            TimePeriod::Afternoon => "下午",
            TimePeriod::Evening => "傍晚",
            TimePeriod::Night => "夜间",
            TimePeriod::LateNight => "深夜",
        }
    }
}

// =========================================================
// 测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn from_local_hour_boundaries() {
        // 深夜：跨零点两端
        assert_eq!(TimePeriod::from_local_hour(23), TimePeriod::LateNight);
        assert_eq!(TimePeriod::from_local_hour(0), TimePeriod::LateNight);
        assert_eq!(TimePeriod::from_local_hour(4), TimePeriod::LateNight);
        // 清晨
        assert_eq!(TimePeriod::from_local_hour(5), TimePeriod::EarlyMorning);
        assert_eq!(TimePeriod::from_local_hour(7), TimePeriod::EarlyMorning);
        // 上午
        assert_eq!(TimePeriod::from_local_hour(8), TimePeriod::Morning);
        assert_eq!(TimePeriod::from_local_hour(11), TimePeriod::Morning);
        // 下午
        assert_eq!(TimePeriod::from_local_hour(12), TimePeriod::Afternoon);
        assert_eq!(TimePeriod::from_local_hour(16), TimePeriod::Afternoon);
        // 傍晚
        assert_eq!(TimePeriod::from_local_hour(17), TimePeriod::Evening);
        assert_eq!(TimePeriod::from_local_hour(19), TimePeriod::Evening);
        // 夜间
        assert_eq!(TimePeriod::from_local_hour(20), TimePeriod::Night);
        assert_eq!(TimePeriod::from_local_hour(22), TimePeriod::Night);
    }

    #[test]
    fn from_local_hour_wraps_out_of_range() {
        // 越界值按对 24 取模归一（25 → 1 → 深夜；29 → 5 → 清晨）
        assert_eq!(TimePeriod::from_local_hour(24), TimePeriod::LateNight);
        assert_eq!(TimePeriod::from_local_hour(25), TimePeriod::LateNight);
        assert_eq!(TimePeriod::from_local_hour(29), TimePeriod::EarlyMorning);
        assert_eq!(TimePeriod::from_local_hour(47), TimePeriod::LateNight);
        // u32::MAX 不 panic，且与归一后的同级小时一致
        assert_eq!(
            TimePeriod::from_local_hour(u32::MAX),
            TimePeriod::from_local_hour(u32::MAX % 24)
        );
    }

    #[test]
    fn from_local_hour_covers_all_24_hours() {
        // 0-23 全部可映射；同一小时加上整天倍数后结果不变
        for hour in 0u32..24 {
            let period = TimePeriod::from_local_hour(hour);
            assert_eq!(TimePeriod::from_local_hour(hour + 24), period);
            assert_eq!(TimePeriod::from_local_hour(hour + 24 * 365), period);
        }
    }

    #[test]
    fn as_str_six_distinct_words() {
        let periods = [
            TimePeriod::EarlyMorning,
            TimePeriod::Morning,
            TimePeriod::Afternoon,
            TimePeriod::Evening,
            TimePeriod::Night,
            TimePeriod::LateNight,
        ];
        let words: Vec<&str> = periods.iter().map(|p| p.as_str()).collect();
        let unique: HashSet<&str> = words.iter().copied().collect();
        assert_eq!(unique.len(), 6, "六时段中文词必须互异: {words:?}");
        // 与 L1 摘要词汇逐项对照
        assert_eq!(words, vec!["清晨", "上午", "下午", "傍晚", "夜间", "深夜"]);
    }
}
