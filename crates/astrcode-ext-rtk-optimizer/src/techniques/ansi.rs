//! ANSI 转义序列剥离。
//!
//! 上游 `techniques/ansi.ts` 的三条替换逐条照搬：CSI 序列、OSC 序列（`\x07` 结尾）、
//! OSC 序列（`\x1b\\` 结尾）。

use std::{borrow::Cow, sync::LazyLock};

use regex::Regex;

static CSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("CSI pattern is valid"));
static OSC_BEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\][0-9;]*(?:\x07|\x1b\\)").expect("OSC pattern is valid"));
static OSC_ANY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").expect("OSC pattern is valid")
});

/// 剥离全部 ANSI 转义序列。
pub fn strip_ansi(text: &str) -> String {
    let text = CSI.replace_all(text, "");
    let text = OSC_BEL.replace_all(&text, "");
    OSC_ANY.replace_all(&text, "").into_owned()
}

/// 不含 `ESC` 时直接借用原串。
///
/// 常见路径（普通文本输出）本来就不含转义序列，返回 `Cow::Borrowed` 既省掉三趟正则，
/// 也省掉一次整段拷贝——调用方拿到的就是输入本身，比较时连内容都不用比。
pub fn strip_ansi_fast(text: &str) -> Cow<'_, str> {
    if !text.contains('\x1b') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(strip_ansi(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_path_borrows_the_input() {
        assert!(matches!(strip_ansi_fast("plain text"), Cow::Borrowed(_)));
        assert!(matches!(strip_ansi_fast(""), Cow::Borrowed(_)));
        assert!(matches!(strip_ansi_fast("\x1b[31mred"), Cow::Owned(_)));
    }

    #[test]
    fn fast_path_returns_the_input_unchanged() {
        assert_eq!(strip_ansi_fast("plain text"), "plain text");
        assert_eq!(strip_ansi_fast(""), "");
    }

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi_fast("\x1b[31merror\x1b[0m: bad"), "error: bad");
        assert_eq!(strip_ansi_fast("\x1b[1;32mok\x1b[0m"), "ok");
    }

    #[test]
    fn strips_osc_sequences_with_either_terminator() {
        assert_eq!(strip_ansi_fast("\x1b]0;title\x07rest"), "rest");
        assert_eq!(strip_ansi_fast("\x1b]8;;http://x\x1b\\link"), "link");
    }

    #[test]
    fn leaves_plain_escape_free_text_untouched() {
        let text = "no escapes here: [31m not a code";
        assert_eq!(strip_ansi_fast(text), text);
    }
}
