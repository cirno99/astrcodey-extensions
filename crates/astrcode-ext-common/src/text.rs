//! 用量文本格式化。
//!
//! 插件只能向宿主输出纯文本（`ExtensionCommandResult::Display` 的 `content`
//! 与状态栏条目的 `text`），没有表格、进度条或配色能力。因此这里的输出都按
//! 「一行内可读」设计。

/// 紧凑的 token 计数：`987` / `1.5K` / `1.23M` / `1.23G`。
///
/// 千位以下保持精确，避免把个位数用量显示成 `0.0K`。上界阈值按「舍入后是否会进位」
/// 选取：`999_999` 若走 K 分支会被 `{:.1}` 进位成 `1000.0K`，因此它应当落进 M 分支。
pub fn format_tokens(tokens: u64) -> String {
    /// `{:.1}K` 会进位成 `1000.0K` 的起点。
    const K_CEILING: u64 = 999_950;
    /// `{:.2}M` 会进位成 `1000.00M` 的起点。
    const M_CEILING: u64 = 999_995_000;

    if tokens < 1_000 {
        return tokens.to_string();
    }
    if tokens < K_CEILING {
        return format!("{:.1}K", tokens as f64 / 1_000.0);
    }
    if tokens < M_CEILING {
        return format!("{:.2}M", tokens as f64 / 1_000_000.0);
    }
    format!("{:.2}G", tokens as f64 / 1_000_000_000.0)
}

/// 百分比，保留一位小数。非有限值（NaN / 无穷）显示为 `n/a`。
pub fn format_percent(ratio: f64) -> String {
    if ratio.is_finite() {
        format!("{:.1}%", ratio * 100.0)
    } else {
        "n/a".to_string()
    }
}

/// 对 `Option<f64>` 的命中率做展示；缺省显示 `n/a`。
pub fn format_optional_percent(ratio: Option<f64>) -> String {
    match ratio {
        Some(ratio) => format_percent(ratio),
        None => "n/a".to_string(),
    }
}

/// 字符数是否**可能**超过 `limit`。
///
/// UTF-8 里一个字符最多 4 字节，所以：
///
/// - 字节数 `<= limit` 时，字符数必然 `<= limit`；
/// - 字节数 `> 4 * limit` 时，字符数必然 `> limit`。
///
/// 只有夹在中间才需要真的把字符数出来。压缩管线上这个判断每轮都要跑，而绝大多数
/// 输出是短 ASCII，因此快路径省下的那趟全文扫描不是小数。
pub fn char_count_exceeds(text: &str, limit: usize) -> bool {
    if text.len() <= limit {
        return false;
    }
    if text.len() > limit.saturating_mul(4) {
        return true;
    }
    text.chars().count() > limit
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_byte_length_shortcut_agrees_with_counting_characters() {
        for text in ["", "a", "abc", "你好", "你好世界", "a你b好", "🙂🙂", "\r\n"] {
            for limit in 0..8 {
                assert_eq!(
                    char_count_exceeds(text, limit),
                    text.chars().count() > limit,
                    "text={text:?} limit={limit}"
                );
            }
        }
    }

    #[test]
    fn a_huge_limit_never_overflows() {
        assert!(!char_count_exceeds("abc", usize::MAX));
    }

    #[test]
    fn counts_below_one_thousand_stay_exact() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(1), "1");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn thousands_use_one_decimal() {
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(999_949), "999.9K");
        assert_eq!(format_tokens(999_950), "1.00M");
        assert_eq!(format_tokens(999_999), "1.00M");
    }

    #[test]
    fn millions_use_two_decimals() {
        assert_eq!(format_tokens(1_000_000), "1.00M");
        assert_eq!(format_tokens(1_234_567), "1.23M");
    }

    #[test]
    fn billions_use_two_decimals() {
        assert_eq!(format_tokens(2_500_000_000), "2.50G");
    }

    #[test]
    fn percent_keeps_one_decimal() {
        assert_eq!(format_percent(0.0), "0.0%");
        assert_eq!(format_percent(0.8734), "87.3%");
        assert_eq!(format_percent(1.0), "100.0%");
    }

    #[test]
    fn percent_rejects_non_finite_values() {
        assert_eq!(format_percent(f64::NAN), "n/a");
        assert_eq!(format_percent(f64::INFINITY), "n/a");
    }

    #[test]
    fn optional_percent_reports_missing_values() {
        assert_eq!(format_optional_percent(None), "n/a");
        assert_eq!(format_optional_percent(Some(0.5)), "50.0%");
    }
}
