//! diff 渲染。
//!
//! 输出的每一行都带锚点，`+` 行携带的是**编辑后**的新锚点。这正是链式编辑的关键：
//! 模型看到 diff 里的 `+` 行就等于看到了下一轮可以直接使用的地址，不必重新读文件。

use super::{
    error::EditError,
    hash::{HASH_LEN, HASH_SEP},
    lines::split_lines,
};

/// 渲染一行 diff。`hash` 为 `None` 时用等宽空格占位，保证列对齐。
pub fn fmt_diff_line(prefix: char, line: &str, hash: Option<&str>) -> String {
    match hash {
        Some(hash) => format!("{prefix}{hash}{HASH_SEP}{line}"),
        None => format!("{prefix}{}{HASH_SEP}{line}", " ".repeat(HASH_LEN)),
    }
}

/// 把锚点数组与行数组渲染成 `HASH│content` 区块。
pub fn fmt_region(hashes: &[String], lines: &[&str]) -> Result<String, EditError> {
    if hashes.len() != lines.len() {
        return Err(EditError::internal(format!(
            "fmtRegion: hashes.length ({}) must match lines.length ({}).",
            hashes.len(),
            lines.len()
        )));
    }
    Ok(lines
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{}{HASH_SEP}{line}", hashes[index]))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// 一次替换只会产生一个连续变更区间。
struct DiffRanges {
    first: usize,
    old_last: isize,
    new_last: isize,
}

fn diff_ranges(old_lines: &[&str], new_lines: &[&str]) -> Option<DiffRanges> {
    let min_len = old_lines.len().min(new_lines.len());
    let mut first = 0usize;
    while first < min_len && old_lines[first] == new_lines[first] {
        first += 1;
    }
    if first == min_len && old_lines.len() == new_lines.len() {
        return None;
    }

    let mut old_last = old_lines.len() as isize - 1;
    let mut new_last = new_lines.len() as isize - 1;
    while old_last >= first as isize
        && new_last >= first as isize
        && old_lines[old_last as usize] == new_lines[new_last as usize]
    {
        old_last -= 1;
        new_last -= 1;
    }

    Some(DiffRanges {
        first,
        old_last,
        new_last,
    })
}

/// 生成 `HASH│content` 形式的 diff，以及第一个变更行号（1 起）。
///
/// 内容相同时返回空 diff 与 `None`。
pub fn gen_diff(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
    new_hashes: Option<&[String]>,
    old_hashes: Option<&[String]>,
) -> (String, Option<usize>) {
    let old_lines = split_lines(old_content);
    let new_lines = split_lines(new_content);
    let Some(ranges) = diff_ranges(&old_lines, &new_lines) else {
        return (String::new(), None);
    };
    let DiffRanges {
        first,
        old_last,
        new_last,
    } = ranges;

    let mut output: Vec<String> = Vec::new();
    let context_start = first.saturating_sub(context_lines);
    if context_start > 0 {
        output.push(" ...".to_owned());
    }
    for (index, line) in new_lines
        .iter()
        .enumerate()
        .take(first)
        .skip(context_start)
    {
        output.push(fmt_diff_line(' ', line, hash_at(new_hashes, index)));
    }

    let mut cursor = first as isize;
    while cursor <= old_last {
        let index = cursor as usize;
        output.push(fmt_diff_line('-', old_lines[index], hash_at(old_hashes, index)));
        cursor += 1;
    }
    cursor = first as isize;
    while cursor <= new_last {
        let index = cursor as usize;
        output.push(fmt_diff_line('+', new_lines[index], hash_at(new_hashes, index)));
        cursor += 1;
    }

    let context_end = (new_lines.len() as isize - 1).min(new_last + context_lines as isize);
    cursor = new_last + 1;
    while cursor <= context_end {
        let index = cursor as usize;
        output.push(fmt_diff_line(' ', new_lines[index], hash_at(new_hashes, index)));
        cursor += 1;
    }
    if context_end < new_lines.len() as isize - 1 {
        output.push(" ...".to_owned());
    }

    (output.join("\n"), Some(first + 1))
}

fn hash_at(hashes: Option<&[String]>, index: usize) -> Option<&str> {
    hashes
        .and_then(|hashes| hashes.get(index))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashline::hash::{line_hashes_pure, map_stable_hashes};
    use rustc_hash::FxHashSet;

    #[test]
    fn fmt_region_requires_aligned_arrays() {
        assert_eq!(
            fmt_region(&["aB3".to_owned()], &["x"]).expect("渲染失败"),
            format!("aB3{HASH_SEP}x")
        );
        assert!(fmt_region(&["aB3".to_owned()], &["x", "y"]).is_err());
    }

    #[test]
    fn fmt_diff_line_pads_missing_anchors() {
        assert_eq!(fmt_diff_line('+', "x", Some("aB3")), format!("+aB3{HASH_SEP}x"));
        assert_eq!(fmt_diff_line('-', "x", None), format!("-   {HASH_SEP}x"));
    }

    #[test]
    fn gen_diff_emits_marked_rows_with_anchors() {
        let old_content = "one\ntwo\nthree\n";
        let old_hashes = line_hashes_pure(old_content).expect("分配失败");
        let new_content = "one\nTWO\nthree\n";
        let removed: FxHashSet<String> = [old_hashes[1].clone()].into_iter().collect();
        let new_hashes =
            map_stable_hashes(old_content, &old_hashes, new_content, &removed).expect("映射失败");

        let (diff, first_changed) = gen_diff(
            old_content,
            new_content,
            1,
            Some(&new_hashes),
            Some(&old_hashes),
        );
        assert_eq!(first_changed, Some(2));
        assert!(diff.contains(&format!("-{}{HASH_SEP}two", old_hashes[1])));
        assert!(diff.contains(&format!("+{}{HASH_SEP}TWO", new_hashes[1])));
        assert!(diff.contains(&format!(" {}{HASH_SEP}one", new_hashes[0])));
        assert!(diff.contains(&format!(" {}{HASH_SEP}three", new_hashes[2])));
    }

    #[test]
    fn gen_diff_is_empty_for_identical_content() {
        let (diff, first_changed) = gen_diff("a\n", "a\n", 1, None, None);
        assert!(diff.is_empty());
        assert_eq!(first_changed, None);
    }

    #[test]
    fn gen_diff_marks_truncated_context_on_both_ends() {
        let old_content: String = (1..=20).map(|index| format!("line{index}\n")).collect();
        let mut new_content = old_content.clone();
        new_content = new_content.replace("line10\n", "LINE10\n");
        let (diff, _) = gen_diff(&old_content, &new_content, 1, None, None);
        let lines: Vec<&str> = diff.split('\n').collect();
        assert_eq!(lines.first(), Some(&" ..."));
        assert_eq!(lines.last(), Some(&" ..."));
        assert_eq!(lines.len(), 6); // ... / 上下文 / - / + / 上下文 / ...
    }

    #[test]
    fn gen_diff_handles_pure_insertion() {
        let (diff, first_changed) = gen_diff("a\n", "a\nb\n", 1, None, None);
        assert_eq!(first_changed, Some(2));
        assert!(diff.contains(&format!("+   {HASH_SEP}b")));
        assert!(diff.contains(&format!("    {HASH_SEP}a")));
    }

    #[test]
    fn gen_diff_handles_pure_deletion() {
        let (diff, _) = gen_diff("a\nb\n", "a\n", 1, None, None);
        assert!(diff.contains(&format!("-   {HASH_SEP}b")));
        assert!(!diff.contains("+"));
    }
}
