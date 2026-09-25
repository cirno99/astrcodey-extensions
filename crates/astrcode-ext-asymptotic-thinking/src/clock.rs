//! 时间戳格式化：注入文本里的 `[消息时间：…]` 前缀。
//!
//! 上游用 `new Date().toLocaleString()`（宿主本地时区的可读串）。Rust 侧没有等价的标准库
//! 设施，引入 `chrono` 只为一个时间戳前缀并不划算，因此这里自己做 UTC 历法换算，
//! 输出 `YYYY-MM-DD HH:MM:SS UTC`。**时区是唯一的偏离**：格式与语义都保留了，
//! 只是不再跟随宿主本地时区——带 `UTC` 后缀因而不含歧义。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前时刻的 `YYYY-MM-DD HH:MM:SS UTC`。
pub fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    format_utc(seconds)
}

/// 把 Unix 秒格式化成 `YYYY-MM-DD HH:MM:SS UTC`。
///
/// 历法换算用 Howard Hinnant 的 `civil_from_days`：把「1970-01-01 起的天数」先平移到
/// 0000-03-01 起算，闰年规则就退化成每 146097 天一个周期，不需要逐月查表。
pub fn format_utc(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let seconds_of_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// 天数（1970-01-01 起，可为负）→ 公历年月日。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // 把纪元平移到 0000-03-01，让闰日落在年末。
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 金标准取自 `date -u -d @<seconds>`，覆盖闰年、月末与纪元起点。
    #[test]
    fn known_instants_format_correctly() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(format_utc(1_735_689_599), "2024-12-31 23:59:59 UTC");
        assert_eq!(format_utc(1_735_689_600), "2025-01-01 00:00:00 UTC");
    }

    #[test]
    fn now_is_well_formed() {
        let text = utc_now();
        assert_eq!(text.len(), "2024-01-01 00:00:00 UTC".len());
        assert!(text.ends_with(" UTC"));
        assert_eq!(text.as_bytes()[4], b'-');
        assert_eq!(text.as_bytes()[10], b' ');
    }
}
