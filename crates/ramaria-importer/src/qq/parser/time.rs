//! crates/ramaria-importer/src/qq/parser/time.rs - Unix 毫秒时间戳日期换算
//!
//! 设计特点:
//! - 基于自 epoch（1970-01-01）以来的天数手动推算年月日，不依赖 chrono 时区
//! - 严格按公历闰年规则（4/100/400）计算
//! - 输出固定 `YYYY-MM-DD` 格式，供报告时间范围展示
//! - 输入任意 i64 均返回格式化字符串，无 panic 路径

// =========================================================
// 工具函数
// =========================================================

/// 将 Unix 毫秒时间戳格式化为日期字符串（YYYY-MM-DD）。
///
/// 说明:
/// - 基于自 epoch（1970-01-01）以来的天数手动计算年月日，不依赖 chrono 时区。
/// - 严格按公历闰年规则计算。
pub(super) fn ts_ms_to_date(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let days_since_epoch = secs / 86400;
    let mut y = 1970i64;
    let mut remaining_days = days_since_epoch;

    // 计算年份
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }

    // 计算月份和日期
    let month_days = if is_leap(y) {
        MONTH_DAYS_LEAP
    } else {
        MONTH_DAYS
    };
    let mut m = 0usize;
    while m < 12 && remaining_days >= month_days[m] {
        remaining_days -= month_days[m];
        m += 1;
    }
    let month = m + 1;
    let day = remaining_days + 1;

    format!("{y:04}-{month:02}-{day:02}")
}

/// 公历闰年判断。
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// 每月天数（非闰年）。
const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
/// 每月天数（闰年）。
const MONTH_DAYS_LEAP: [i64; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
