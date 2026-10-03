//! crates/ramaria-service/src/proactive/schedule/quiet.rs - Ramaria 免打扰时段解析与判定
//!
//! 设计特点:
//! - 分钟口径的区间判定，支持跨零点（起点含、终点不含、等值窗口视为未配置）
//! - 输入为 `HH:MM-HH:MM` 文本：空串 / 非法输入降级为 None（调用方按无免打扰处理）
//! - 纯逻辑无 I/O：解析与判定均为纯函数，边界用例可直接以分钟数驱动

// =========================================================
// 免打扰时段
// =========================================================

/// 免打扰时段（分钟口径，支持跨零点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct QuietHours {
    pub(super) start_minute: u32,
    pub(super) end_minute: u32,
}

impl QuietHours {
    /// 判断当日某一分钟是否落在免打扰区间。
    ///
    /// 口径:
    /// - start < end：`[start, end)`；
    /// - start > end：`[start, 1440) ∪ [0, end)`（跨零点）；
    /// - start == end：空窗口（视为未配置）。
    pub(super) fn contains(&self, minute_of_day: u32) -> bool {
        if self.start_minute == self.end_minute {
            return false;
        }
        if self.start_minute < self.end_minute {
            minute_of_day >= self.start_minute && minute_of_day < self.end_minute
        } else {
            minute_of_day >= self.start_minute || minute_of_day < self.end_minute
        }
    }
}

/// 解析 `HH:MM-HH:MM`（空串 / 非法 / 等值窗口 → None，降级不报错）。
pub(super) fn parse_quiet_hours(raw: &str) -> Option<QuietHours> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (start_raw, end_raw) = raw.split_once('-')?;
    let start_minute = parse_hhmm(start_raw.trim())?;
    let end_minute = parse_hhmm(end_raw.trim())?;
    if start_minute == end_minute {
        return None;
    }
    Some(QuietHours {
        start_minute,
        end_minute,
    })
}

/// 解析 `HH:MM` 为当日分钟（时 0-23 / 分 0-59，否则 None）。
fn parse_hhmm(s: &str) -> Option<u32> {
    let (hour_raw, minute_raw) = s.split_once(':')?;
    let hour: u32 = hour_raw.trim().parse().ok()?;
    let minute: u32 = minute_raw.trim().parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}
