//! 路径缩短。
//!
//! 上游 `techniques/path-utils.ts` 的移植：超长路径保留盘符/UNC 前缀与末尾一两段，
//! 中间用 `…` 省略。所有切分按字符进行，不切断多字节字符。

/// 路径里出现 `\` 且没有 `/` 时按 Windows 分隔符处理，否则按 `/`。
fn detect_path_separator(path: &str) -> char {
    if path.contains('\\') && !path.contains('/') {
        '\\'
    } else {
        '/'
    }
}

/// 抽出必须原样保留的前缀：盘符、UNC 共享前缀、根分隔符。
fn detect_path_prefix(path: &str, separator: char) -> String {
    let characters: Vec<char> = path.chars().collect();

    if characters.len() >= 3
        && characters[0].is_ascii_alphabetic()
        && characters[1] == ':'
        && matches!(characters[2], '\\' | '/')
    {
        // 与上游一致：保留「盘符 + 冒号 + 分隔符」，而不是把冒号丢掉。
        return format!("{}{}{separator}", characters[0], characters[1]);
    }

    if path.starts_with("\\\\") || path.starts_with("//") {
        let parts: Vec<&str> = path
            .split(['\\', '/'])
            .filter(|part| !part.is_empty())
            .collect();
        return if parts.len() >= 2 {
            format!("{separator}{separator}{}{separator}{}{separator}", parts[0], parts[1])
        } else {
            format!("{separator}{separator}")
        };
    }

    if path.starts_with('/') || path.starts_with('\\') {
        return separator.to_string();
    }

    String::new()
}

fn join_path_segments(prefix: &str, separator: char, segments: &[String]) -> String {
    if segments.is_empty() {
        return prefix.to_owned();
    }
    let joined = segments.join(&separator.to_string());
    if prefix.is_empty() {
        joined
    } else {
        format!("{prefix}{joined}")
    }
}

/// 取末尾 `count` 个字符；不足时返回整串。
fn tail(characters: &[char], count: usize) -> String {
    let start = characters.len().saturating_sub(count);
    characters[start..].iter().collect()
}

fn tail_of(text: &str, count: usize) -> String {
    let characters: Vec<char> = text.chars().collect();
    tail(&characters, count)
}

/// 把超长路径缩短到 `max_length` 个字符以内。
pub fn compact_path(path: &str, max_length: usize) -> String {
    let characters: Vec<char> = path.chars().collect();
    if characters.len() <= max_length {
        return path.to_owned();
    }

    if max_length < 2 {
        return characters[..max_length].iter().collect();
    }

    let separator = detect_path_separator(path);
    let prefix = detect_path_prefix(path, separator);
    let prefix_length = prefix.chars().count();
    let segments: Vec<String> = characters[prefix_length..]
        .iter()
        .collect::<String>()
        .split(['\\', '/'])
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect();

    let last_segment = segments
        .last()
        .cloned()
        .unwrap_or_else(|| tail(&characters, max_length - 1));
    let previous_segment = (segments.len() >= 2).then(|| segments[segments.len() - 2].clone());

    let mut ellipsis_segments = vec!["…".to_owned()];
    if let Some(previous) = &previous_segment {
        ellipsis_segments.push(previous.clone());
    }
    ellipsis_segments.push(last_segment.clone());

    let candidates = [
        join_path_segments(&prefix, separator, &ellipsis_segments),
        join_path_segments("", separator, &ellipsis_segments),
        join_path_segments("", separator, &["…".to_owned(), last_segment.clone()]),
        format!("…{}", tail(&characters, max_length - 1)),
    ];

    for candidate in &candidates {
        if candidate.chars().count() <= max_length {
            return candidate.clone();
        }
    }

    format!("…{}", tail_of(&last_segment, max_length - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths_are_returned_verbatim() {
        assert_eq!(compact_path("src/main.rs", 40), "src/main.rs");
        assert_eq!(compact_path("", 0), "");
    }

    #[test]
    fn long_paths_keep_the_prefix_and_the_last_two_segments() {
        let path = "/home/user/projects/deep/nested/tree/src/module/file.rs";
        let compacted = compact_path(path, 30);
        assert!(compacted.chars().count() <= 30, "{compacted}");
        assert!(compacted.starts_with("/…/"), "{compacted}");
        assert!(compacted.ends_with("file.rs"), "{compacted}");
    }

    #[test]
    fn degenerate_limits_slice_the_head() {
        assert_eq!(compact_path("/a/b/c", 1), "/");
        assert_eq!(compact_path("/a/b/c", 0), "");
    }

    #[test]
    fn windows_drive_prefixes_are_preserved() {
        let path = r"C:\Users\me\projects\deep\nested\src\file.rs";
        let compacted = compact_path(path, 30);
        assert!(compacted.chars().count() <= 30, "{compacted}");
        assert!(compacted.starts_with(r"C:\"), "{compacted}");
        assert!(compacted.ends_with("file.rs"), "{compacted}");
    }

    /// UNC 前缀很长，`max_length` 装不下时会被整个省掉——与上游的候选顺序一致。
    #[test]
    fn unc_prefixes_are_preserved_when_they_fit() {
        let path = r"\\server\share\deep\nested\folder\file.rs";

        let compacted = compact_path(path, 40);
        assert!(compacted.chars().count() <= 40, "{compacted}");
        assert!(compacted.starts_with(r"\\server\share\"), "{compacted}");
        assert!(compacted.ends_with("file.rs"), "{compacted}");

        let squeezed = compact_path(path, 30);
        assert!(squeezed.chars().count() <= 30, "{squeezed}");
        assert!(squeezed.starts_with('…'), "{squeezed}");
    }

    #[test]
    fn a_single_very_long_segment_is_tail_truncated() {
        let path = format!("/{}", "x".repeat(200));
        let compacted = compact_path(&path, 20);
        assert_eq!(compacted.chars().count(), 20);
        assert!(compacted.starts_with('…'));
    }

    #[test]
    fn multibyte_paths_are_never_cut_mid_character() {
        let path = "/项目/很深的目录/另一个目录/源代码/模块/文件.rs";
        let compacted = compact_path(path, 20);
        assert!(compacted.chars().count() <= 20, "{compacted}");
        assert!(compacted.ends_with("文件.rs"), "{compacted}");
    }
}
