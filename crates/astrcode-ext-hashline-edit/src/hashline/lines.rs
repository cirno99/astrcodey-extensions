//! 行切分、规范化与行尾/BOM 处理。
//!
//! 语义逐条对齐原版：`splitLines` 不产生「尾随换行带来的幽灵行」，`canon` 去掉
//! 行尾空白后再算哈希，因此「同一行只改了尾随空格」不会换锚点。

use std::borrow::Cow;

/// 按 `\n` 切分，且不把结尾的换行算作额外一行。
///
/// ```text
/// ""      -> [""]
/// "a\nb"  -> ["a", "b"]
/// "a\nb\n"-> ["a", "b"]     尾随换行不产生幽灵行
/// "\n\n"  -> ["", ""]
/// ```
pub fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// 哈希用的行规范化：去掉所有 `\r`，再去掉尾随空白。
///
/// 返回 [`Cow`] 而不是 `String`：`trim_end` 给出的是原字符串的子切片，LF 文件里
/// 绝大多数行（无 `\r`、无尾随空白）可以直接借用，一次分配都不做。编辑路径上
/// 每次要对整份文件规范化好几遍，这里是省下的主要部分。
///
/// 只有行内真的含 `\r`（CRLF / CR 文件）时才落到 owned 分支。
pub fn canon(line: &str) -> Cow<'_, str> {
    if !line.as_bytes().contains(&b'\r') {
        return Cow::Borrowed(line.trim_end());
    }
    Cow::Owned(line.replace('\r', "").trim_end().to_owned())
}

/// 把一行压成单行预览并截断，用于错误反馈里的上下文提示。
pub fn clip_line(line: &str) -> String {
    clip_line_with(line, 200)
}

/// [`clip_line`] 的可调上限版本。
pub fn clip_line_with(line: &str, max_len: usize) -> String {
    let flat = line.replace('\n', "\\n");
    if flat.chars().count() > max_len {
        let head: String = flat.chars().take(max_len).collect();
        format!("{head}...")
    } else {
        flat
    }
}

/// 文件的换行风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Lf,
    Crlf,
    Cr,
}

impl Ending {
    /// 序列化进状态文件的字面量，与原版一致。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
            Self::Cr => "\r",
        }
    }

    /// 从状态文件读回；无法识别时按 LF 处理。
    pub fn parse(value: &str) -> Self {
        match value {
            "\r\n" => Self::Crlf,
            "\r" => Self::Cr,
            _ => Self::Lf,
        }
    }
}

/// 探测换行风格：以第一次出现的换行为准，混合时按多数派的直觉取首个。
pub fn detect_ending(content: &str) -> Ending {
    let Some(lf_index) = content.find('\n') else {
        return if content.contains('\r') {
            Ending::Cr
        } else {
            Ending::Lf
        };
    };
    match content.find("\r\n") {
        Some(crlf_index) if crlf_index < lf_index => Ending::Crlf,
        _ => Ending::Lf,
    }
}

/// 把所有换行统一成 LF。
pub fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 把 LF 还原成目标换行风格。
pub fn restore_endings(text: &str, ending: Ending) -> String {
    match ending {
        Ending::Lf => text.to_owned(),
        Ending::Crlf => text.replace('\n', "\r\n"),
        Ending::Cr => text.replace('\n', "\r"),
    }
}

/// 拆出 BOM 与正文。
pub fn strip_bom(content: &str) -> (&str, &str) {
    match content.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", content),
    }
}

/// 计算 `result` 相对 `original` 的变更行区间（1 起、闭区间）。
///
/// 内容完全相同时返回 `None`。
pub fn changed_range(original: &str, result: &str) -> Option<(usize, usize)> {
    if original == result {
        return None;
    }
    if original.is_empty() {
        return Some((1, split_lines(result).len()));
    }

    let original_lines = split_lines(original);
    let result_lines = split_lines(result);
    if original_lines == result_lines {
        return None;
    }

    let min_len = original_lines.len().min(result_lines.len());
    let mut first = 0usize;
    while first < min_len && original_lines[first] == result_lines[first] {
        first += 1;
    }

    let mut last_original = original_lines.len() as isize - 1;
    let mut last_result = result_lines.len() as isize - 1;
    while last_original >= first as isize
        && last_result >= first as isize
        && original_lines[last_original as usize] == result_lines[last_result as usize]
    {
        last_original -= 1;
        last_result -= 1;
    }

    Some((first + 1, last_result.max(first as isize) as usize + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_mirrors_the_original_semantics() {
        assert_eq!(split_lines(""), [""]);
        assert_eq!(split_lines("a\nb"), ["a", "b"]);
        assert_eq!(split_lines("a\nb\n"), ["a", "b"]);
        assert_eq!(split_lines("\n\n"), ["", ""]);
        assert_eq!(split_lines("a\r\nb\r\n"), ["a\r", "b\r"]);
    }

    #[test]
    fn canon_strips_cr_and_trailing_whitespace() {
        assert_eq!(canon("  hello  \r"), "  hello");
        assert_eq!(canon("plain"), "plain");
        assert_eq!(canon("\t x \t"), "\t x");
        assert_eq!(canon("a\r \r"), "a");
    }

    /// 无 `\r` 的行必须走借用分支：编辑路径上的分配次数靠它。
    #[test]
    fn canon_borrows_when_the_line_needs_no_rewrite() {
        assert!(matches!(canon("plain"), Cow::Borrowed(_)));
        assert!(matches!(canon("  padded\t"), Cow::Borrowed(_)));
        assert!(matches!(canon("trailing \r"), Cow::Owned(_)));
    }

    #[test]
    fn clip_line_flattens_newlines_and_caps_length() {
        assert_eq!(clip_line("a\nb"), "a\\nb");
        assert!(clip_line(&"x".repeat(500)).chars().count() < 250);
        assert_eq!(clip_line_with("abcdef", 3), "abc...");
        assert_eq!(clip_line_with("abc", 3), "abc");
    }

    #[test]
    fn detect_ending_covers_lf_crlf_cr_and_mixed() {
        assert_eq!(detect_ending("a\r\nb\r\n"), Ending::Crlf);
        assert_eq!(detect_ending("a\nb\n"), Ending::Lf);
        assert_eq!(detect_ending("a\rb\r"), Ending::Cr);
        assert_eq!(detect_ending("no newline"), Ending::Lf);
        // 混合时以先出现的那个为准
        assert_eq!(detect_ending("a\nb\r\n"), Ending::Lf);
        assert_eq!(detect_ending("a\r\nb\n"), Ending::Crlf);
    }

    #[test]
    fn ending_round_trips_through_its_literal() {
        for ending in [Ending::Lf, Ending::Crlf, Ending::Cr] {
            assert_eq!(Ending::parse(ending.as_str()), ending);
        }
        assert_eq!(Ending::parse("garbage"), Ending::Lf);
    }

    #[test]
    fn to_lf_and_restore_endings_round_trip() {
        assert_eq!(to_lf("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(restore_endings("a\nb\n", Ending::Crlf), "a\r\nb\r\n");
        assert_eq!(restore_endings("a\nb\n", Ending::Cr), "a\rb\r");
        assert_eq!(restore_endings("a\nb\n", Ending::Lf), "a\nb\n");
        let original = "a\r\nb\r\n";
        assert_eq!(
            restore_endings(&to_lf(original), detect_ending(original)),
            original
        );
    }

    #[test]
    fn strip_bom_separates_the_marker() {
        assert_eq!(strip_bom("\u{feff}hello"), ("\u{feff}", "hello"));
        assert_eq!(strip_bom("hello"), ("", "hello"));
    }

    #[test]
    fn changed_range_reports_first_and_last_changed_lines() {
        assert_eq!(changed_range("a\nb\nc\n", "a\nB\nc\n"), Some((2, 2)));
        assert_eq!(changed_range("a\nb\n", "a\nb\n"), None);
        assert_eq!(changed_range("", "x\ny\n"), Some((1, 2)));
        // 缩短：删掉中间两行。区间按结果行号给出，末尾对齐后收敛到第 2 行
        assert_eq!(changed_range("a\nb\nc\nd\n", "a\nd\n"), Some((2, 2)));
        // 增长：中间插入
        assert_eq!(changed_range("a\nb\n", "a\nx\ny\nb\n"), Some((2, 3)));
    }
}
