//! 硬字符截断。
//!
//! 上游 `techniques/truncate.ts` 的移植。注意 `max_length < 3` 时上游返回固定的
//! `"..."`，结果可能长于 `max_length`——这是上游的既有行为，照搬不修。

use astrcode_ext_common::text::char_count_exceeds;
/// 截断到 `max_length` 个字符，超长时以 `...` 收尾。
pub fn truncate(text: &str, max_length: usize) -> String {
    if !char_count_exceeds(text, max_length) {
        return text.to_owned();
    }

    if max_length < 3 {
        return "...".to_owned();
    }

    let mut truncated: String = text.chars().take(max_length - 3).collect();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_returned_verbatim() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("", 0), "");
    }

    #[test]
    fn long_input_is_cut_to_the_limit_including_the_ellipsis() {
        assert_eq!(truncate("abcdef", 4), "a...");
        assert_eq!(truncate("abcdef", 3), "...");
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdefg", 6), "abc...");
    }

    #[test]
    fn degenerate_limits_return_the_fixed_ellipsis() {
        assert_eq!(truncate("abcdef", 2), "...");
        assert_eq!(truncate("abcdef", 0), "...");
    }

    #[test]
    fn counts_characters_not_bytes() {
        assert_eq!(truncate("你好世界", 4), "你好世界");
        assert_eq!(truncate("你好啊世界", 4), "你...");
    }
}
