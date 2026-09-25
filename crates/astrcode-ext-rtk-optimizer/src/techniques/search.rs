//! 搜索结果按文件分组。
//!
//! 上游 `techniques/search.ts` 的移植：把 `文件:行:内容` 形态的搜索结果重排成
//! 「文件 + 该文件内的命中行」，并对文件数与每文件命中数设上限。
//!
//! 上游用一条正则解析命中行，本移植改成手写解析：那条正则占了本技术近八成的耗时
//! （5000 行实测 2.9ms / 3.7ms）。手写解析的语义由 `differential_matches_the_regex`
//! 与原正则逐行对拍守住。

use std::collections::BTreeMap;

use super::path::compact_path;

/// 一行搜索命中的解析结果。三个字段都借用输入，解析本身不分配。
struct SearchResult<'a> {
    file: &'a str,
    line_number: &'a str,
    content: &'a str,
}

/// 解析 `文件:行号:内容`。
///
/// 语义与上游的 `^(.+?):([0-9]+)?:(.+)$` 逐位对齐，但不用正则：那条正则占了本函数
/// 近八成的耗时（5000 行实测 2.9ms / 3.7ms）。`differential_matches_the_regex` 拿
/// 原正则做参照对拍，保证换实现不改行为。
///
/// 两个量词的语义决定了解析形状：
///
/// - `.+?` 是惰性的，所以 group 1 是**第一个能让剩余部分匹配上的冒号**之前的部分；
///   某个冒号试不通就要继续试下一个。
/// - `[0-9]+` 是贪婪的，而数字串后面必须紧跟冒号才算命中。比最长数字串更短的候选，
///   其后一位必然还是数字、不可能是冒号，因此**不需要回溯数字串**——只试最长的一串
///   和「组 2 缺席」两种情况即可。
fn parse_search_line(line: &str) -> Option<SearchResult<'_>> {
    let bytes = line.as_bytes();
    let mut search_from = 0usize;

    while let Some(offset) = line[search_from..].find(':') {
        let colon = search_from + offset;
        // `(.+?)` 至少要吃掉一个字符，行首就是冒号时这个候选无效。
        if colon == 0 {
            search_from = 1;
            continue;
        }

        let after_colon = colon + 1;
        let digit_run = bytes[after_colon..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let digits_end = after_colon + digit_run;

        // 贪婪分支：数字串 + 第二个冒号 + 至少一个字符的正文。
        if digit_run > 0 && bytes.get(digits_end) == Some(&b':') && digits_end + 1 < bytes.len() {
            return Some(SearchResult {
                file: &line[..colon],
                line_number: &line[after_colon..digits_end],
                content: &line[digits_end + 1..],
            });
        }

        // 组 2 缺席分支：第二个冒号紧跟第一个。
        if bytes.get(after_colon) == Some(&b':') && after_colon + 1 < bytes.len() {
            return Some(SearchResult {
                file: &line[..colon],
                line_number: "?",
                content: &line[after_colon + 1..],
            });
        }

        search_from = colon + 1;
    }

    None
}

/// 把搜索结果按文件分组。没有可解析的命中行时返回 `None`。
pub fn group_search_results(output: &str, max_results: usize) -> Option<String> {
    let mut results: Vec<SearchResult<'_>> = Vec::new();
    for line in output.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(result) = parse_search_line(line) {
            results.push(result);
        }
    }

    if results.is_empty() {
        return None;
    }

    // 按文件分组。`BTreeMap` 的键序就是文件名字节序，与上游 `localeCompare` 在 ASCII
    // 输入下的结果一致，因此分组后不需要再排一次。
    let mut by_file: BTreeMap<&str, Vec<&SearchResult<'_>>> = BTreeMap::new();
    for result in &results {
        by_file.entry(result.file).or_default().push(result);
    }

    let mut text = format!("{} matches in {} files:\n\n", results.len(), by_file.len());
    let mut shown = 0usize;

    for (file, matches) in &by_file {
        if shown >= max_results {
            break;
        }
        text.push_str(&format!(
            "> {} ({} matches):\n",
            compact_path(file, 50),
            matches.len()
        ));
        for found in matches.iter().take(10) {
            let cleaned = found.content.trim();
            if cleaned.chars().count() > 70 {
                let clipped: String = cleaned.chars().take(67).collect();
                text.push_str(&format!("    {}: {clipped}...\n", found.line_number));
            } else {
                text.push_str(&format!("    {}: {cleaned}\n", found.line_number));
            }
            shown += 1;
        }
        if matches.len() > 10 {
            text.push_str(&format!("  +{} more\n", matches.len() - 10));
        }
        text.push('\n');
    }

    if results.len() > shown {
        text.push_str(&format!("... +{} more\n", results.len() - shown));
    }

    Some(text)
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use regex::Regex;

    use super::*;

    /// 差分对照：手写解析必须与原正则逐位一致。
    ///
    /// 语料覆盖两个量词的全部边界——行首/行尾的冒号、连续冒号、数字串贴着行尾、
    /// 多字节字符、Windows 盘符、URL 端口、超长数字串等。
    #[test]
    fn differential_matches_the_regex() {
        static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(.+?):([0-9]+)?:(.+)$").expect("search line is valid")
        });

        let corpus = [
            "",
            ":",
            "::",
            ":::",
            "a:1:b",
            "a::b",
            "a:1",
            "a:",
            "a::",
            ":a:1:b",
            "ab:cd:ef",
            "ab:12:ef",
            "a:12:34:b",
            "a:12x:y",
            "a:1:2",
            "src/a.ts:1:const a = 1;",
            "src/a.ts::odd",
            "src/a.ts:1:key: value: more",
            "src/a.ts:1:",
            "src/a.ts::",
            "src/a.ts:1",
            "src/a.ts",
            "src/a.ts:0:x",
            "src/a.ts:007:x",
            "crates/错/误.rs:12: 内容",
            "错:1:误",
            "错误:1:内容:尾巴",
            "1:2:3",
            ":1:2",
            "a:1:2:3:4",
            "a:b:1:c",
            "a:1:b:2",
            "  a:1:b  ",
            "a:1:b\r",
            "https://example.com:8080/path:x",
            "C:\\path\\file.rs:10:fn main",
            "a:999999999999999999999999:x",
            "a:+1:x",
            "a:1:x:y",
        ];

        for line in corpus {
            let expected = REFERENCE.captures(line).map(|captures| {
                (
                    captures.get(1).map_or("", |value| value.as_str()).to_owned(),
                    captures.get(2).map_or("?", |value| value.as_str()).to_owned(),
                    captures.get(3).map_or("", |value| value.as_str()).to_owned(),
                )
            });
            let actual = parse_search_line(line).map(|parsed| {
                (
                    parsed.file.to_owned(),
                    parsed.line_number.to_owned(),
                    parsed.content.to_owned(),
                )
            });
            assert_eq!(actual, expected, "行 {line:?} 的解析与原正则不一致");
        }
    }

    #[test]
    fn unparseable_output_returns_none() {
        assert_eq!(group_search_results("", 50), None);
        assert_eq!(group_search_results("no colons here\n", 50), None);
    }

    #[test]
    fn groups_matches_by_file_and_counts_them() {
        let output = "\
src/b.ts:3:const b = 1;
src/a.ts:1:const a = 1;
src/a.ts:2:const a2 = 2;
";
        let grouped = group_search_results(output, 50).unwrap();
        assert!(grouped.starts_with("3 matches in 2 files:\n\n"));
        // 文件按名字排序：a 在 b 前。
        let a_index = grouped.find("> src/a.ts (2 matches):").unwrap();
        let b_index = grouped.find("> src/b.ts (1 matches):").unwrap();
        assert!(a_index < b_index);
        assert!(grouped.contains("    1: const a = 1;"));
        assert!(grouped.contains("    2: const a2 = 2;"));
    }

    #[test]
    fn lines_without_a_line_number_use_a_question_mark() {
        let grouped = group_search_results("src/a.ts::odd\n", 50).unwrap();
        assert!(grouped.contains("    ?: odd"));
    }

    #[test]
    fn content_with_colons_is_kept_whole() {
        let grouped = group_search_results("src/a.ts:1:key: value: more\n", 50).unwrap();
        assert!(grouped.contains("    1: key: value: more"));
    }

    #[test]
    fn long_content_is_clipped_to_seventy_characters() {
        let line = format!("src/a.ts:1:{}\n", "x".repeat(200));
        let grouped = group_search_results(&line, 50).unwrap();
        assert!(grouped.contains(&format!("    1: {}...", "x".repeat(67))));
    }

    #[test]
    fn per_file_matches_are_capped_at_ten() {
        let mut output = String::new();
        for index in 0..15 {
            output.push_str(&format!("src/a.ts:{index}:hit\n"));
        }
        let grouped = group_search_results(&output, 50).unwrap();
        assert!(grouped.contains("> src/a.ts (15 matches):"));
        assert!(grouped.contains("  +5 more\n"));
    }

    #[test]
    fn the_global_result_budget_stops_emitting_files() {
        let mut output = String::new();
        for index in 0..5 {
            output.push_str(&format!("src/f{index}.ts:1:hit\n"));
        }
        let grouped = group_search_results(&output, 2).unwrap();
        assert!(grouped.contains("5 matches in 5 files:"));
        assert!(grouped.contains("> src/f0.ts"));
        assert!(grouped.contains("> src/f1.ts"));
        assert!(!grouped.contains("> src/f2.ts"));
        assert!(grouped.contains("... +3 more\n"));
    }
}
