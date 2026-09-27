//! 应用编辑：把锚点范围换算成字节区间，拼出新内容。
//!
//! 这里是「行号 vs 字节偏移」的换算点。锚点是行地址，但真正改动文件必须落到字节
//! 区间上，还要处理三种边界：删除整段、删除到文件末尾（含「末行没有换行符」的
//! 情况）、以及空文件插入。

use std::collections::BTreeSet;

use rustc_hash::FxHashSet;

use super::{
    anchor::{
        BoundaryDup, MismatchFeedback, ResolvedEdit, assert_range_served, fmt_mismatch_with_hashes,
        val_edit,
    },
    error::{EditError, ErrorCode},
    hash::{Anchor, line_hashes_pure},
    lines::changed_range,
    request::{EditRequest, strip_bare_prefixes, strip_diff_prefixes, swap_reversed_ranges},
};

/// 行数组与每行的起始字节偏移。
struct LineIndex<'a> {
    lines: Vec<&'a str>,
    starts: Vec<usize>,
}

/// 建立行索引。`starts[i]` 是第 i 行（0 起）在内容里的起始字节位置。
fn build_index(content: &str) -> LineIndex<'_> {
    let lines = super::lines::split_lines(content);
    let mut starts = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for (index, line) in lines.iter().enumerate() {
        starts.push(offset);
        offset += line.len();
        if index < lines.len() - 1 {
            offset += 1; // '\n'
        }
    }
    LineIndex { lines, starts }
}

/// 一次成功（或 noop）的编辑结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// 编辑后的完整内容；noop 时等于原内容。
    pub content: String,
    pub first_changed_line: Option<usize>,
    pub last_changed_line: Option<usize>,
    /// 自动纠正产生的警告，按发生顺序排列。
    pub warnings: Vec<String>,
    /// 从 `replacement_text` 里自动删掉的边界重复行。
    pub auto_fixes: Vec<BoundaryDup>,
    /// 替换内容与原内容逐字相同时给出。
    pub noop: Option<NoopEdit>,
}

/// noop 的说明信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoopEdit {
    /// 范围起点锚点。
    pub loc: String,
    pub current_content: String,
}

/// 字节区间：用 `replacement` 替换 `content[start..end]`。
enum Span {
    /// 替换内容与原内容逐字相同，无需改动。
    Noop {
        loc: String,
        current_content: String,
    },
    Replace {
        start: usize,
        end: usize,
        replacement: String,
    },
}

/// 应用一次锚点编辑。
///
/// `precomputed_hashes` 是调用方已经算好的锚点（通常来自快照缓存）；`served` 是
/// served 守卫用的集合，`None` 表示跳过该闸门（仅测试路径会用）。
pub fn apply_edit(
    content: &str,
    edit: &EditRequest,
    precomputed_hashes: Option<&[Anchor]>,
    file_path: Option<&str>,
    served: Option<&BTreeSet<Anchor>>,
) -> Result<Applied, EditError> {
    let index = build_index(content);
    let computed_hashes;
    let file_hashes: &[Anchor] = match precomputed_hashes {
        Some(hashes) => hashes,
        None => {
            computed_hashes = line_hashes_pure(content)?;
            &computed_hashes
        },
    };

    let mut warnings = Vec::new();

    // 先做三项自动纠正，顺序与原版一致：对调反向范围 → 剥裸前缀 → 剥 diff 标记。
    let range_fixed = swap_reversed_ranges(edit, file_hashes, &mut warnings);
    let bare_fixed = strip_bare_prefixes(&range_fixed, file_hashes, &mut warnings);
    let prefix_fixed = strip_diff_prefixes(&bare_fixed, &mut warnings);

    let outcome = val_edit(&prefix_fixed, &index.lines, file_hashes)?;
    let Some(initial_resolved) = outcome.resolved else {
        return Err(anchor_feedback(
            &outcome.mismatches,
            &index.lines,
            file_hashes,
            file_path,
        )?);
    };

    let mut resolved = initial_resolved;
    let mut auto_fixes: Vec<BoundaryDup> = Vec::new();

    if !outcome.boundary_dups.is_empty() {
        // 同一个替换行可能同时被多种规则命中，按行号去重后从后往前删。
        let mut seen: FxHashSet<usize> = FxHashSet::default();
        let mut unique: Vec<&BoundaryDup> = outcome
            .boundary_dups
            .iter()
            .filter(|dup| seen.insert(dup.replacement_line_index))
            .collect();
        unique.sort_by(|left, right| {
            right
                .replacement_line_index
                .cmp(&left.replacement_line_index)
        });

        let mut corrected = prefix_fixed.clone();
        for dup in unique {
            let position = dup.replacement_line_index;
            if position >= corrected.content_lines.len() {
                continue;
            }
            corrected.content_lines.remove(position);
            auto_fixes.push(BoundaryDup {
                kind: dup.kind,
                replacement_line_index: position,
            });
        }

        let corrected_outcome = val_edit(&corrected, &index.lines, file_hashes)?;
        let Some(corrected_resolved) = corrected_outcome.resolved else {
            return Err(anchor_feedback(
                &corrected_outcome.mismatches,
                &index.lines,
                file_hashes,
                file_path,
            )?);
        };
        resolved = corrected_resolved;
    }

    if let Some(served) = served {
        assert_range_served(&resolved, &index.lines, file_hashes, served, file_path)?;
    }

    match res_to_span(&resolved, content, &index) {
        Span::Noop {
            loc,
            current_content,
        } => Ok(Applied {
            content: content.to_owned(),
            first_changed_line: None,
            last_changed_line: None,
            warnings,
            auto_fixes,
            noop: Some(NoopEdit {
                loc,
                current_content,
            }),
        }),
        Span::Replace {
            start,
            end,
            replacement,
        } => {
            let result = format!("{}{replacement}{}", &content[..start], &content[end..]);
            assert_not_empty(content, &result)?;
            let range = changed_range(content, &result);
            Ok(Applied {
                content: result,
                first_changed_line: range.map(|(first, _)| first),
                last_changed_line: range.map(|(_, last)| last),
                warnings,
                auto_fixes,
                noop: None,
            })
        },
    }
}

/// 把锚点校验失败转成带反馈锚点的错误。
fn anchor_feedback(
    mismatches: &[super::anchor::Mismatch],
    file_lines: &[&str],
    file_hashes: &[Anchor],
    file_path: Option<&str>,
) -> Result<EditError, EditError> {
    let MismatchFeedback { code, body, hashes } =
        fmt_mismatch_with_hashes(mismatches, file_lines, file_hashes, file_path)?;
    Ok(EditError::new(code, body).with_feedback(hashes))
}

/// 把已解析的范围换算成字节区间。
fn res_to_span(edit: &ResolvedEdit, content: &str, index: &LineIndex<'_>) -> Span {
    let start_line = edit.hash_bounds[0].line;
    let end_line = edit.hash_bounds[1].line;
    let original_lines = &index.lines[start_line - 1..end_line];

    if original_lines.len() == edit.content_lines.len()
        && original_lines
            .iter()
            .zip(&edit.content_lines)
            .all(|(line, replacement)| *line == replacement.as_str())
    {
        return Span::Noop {
            loc: edit.hash_bounds[0].hash.clone(),
            current_content: original_lines.join("\n"),
        };
    }

    if !edit.content_lines.is_empty() {
        return Span::Replace {
            start: index.starts[start_line - 1],
            end: index.starts[end_line - 1] + index.lines[end_line - 1].len(),
            replacement: edit.content_lines.join("\n"),
        };
    }

    if start_line == 1 && end_line == index.lines.len() {
        return Span::Replace {
            start: 0,
            end: content.len(),
            replacement: String::new(),
        };
    }

    if end_line < index.lines.len() {
        return Span::Replace {
            start: index.starts[start_line - 1],
            end: index.starts[end_line],
            replacement: String::new(),
        };
    }

    if content.ends_with('\n') {
        return Span::Replace {
            start: index.starts[start_line - 1],
            end: content.len(),
            replacement: String::new(),
        };
    }

    // 删到文件末尾、且末尾没有换行符：连上一行的换行一起吃掉，
    // 否则会留下一个多余的空行。上一行本身为空时不动，避免把空行也删掉。
    let previous_line = if start_line >= 2 {
        Some(index.lines[start_line - 2])
    } else {
        None
    };
    Span::Replace {
        start: match previous_line {
            Some("") => index.starts[start_line - 1],
            _ => index.starts[start_line - 1].saturating_sub(1),
        },
        end: content.len(),
        replacement: String::new(),
    }
}

/// 拒绝把一个非空文件清空。
fn assert_not_empty(original: &str, result: &str) -> Result<(), EditError> {
    if !original.is_empty() && result.is_empty() {
        return Err(EditError::new(
            ErrorCode::WouldEmpty,
            "Cannot empty a non-empty file via replace. Use `write` if you need to clear the file.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashline::{HASH_SEP, hash::line_hashes_pure, request::HashRef};

    const SAMPLE: &str = "function hello() {\n  console.log(\"world\");\n}\n\n// end\n";

    fn hashes(content: &str) -> Vec<Anchor> {
        line_hashes_pure(content).expect("分配失败")
    }

    fn edit(from: impl AsRef<str>, to: impl AsRef<str>, lines: &[&str]) -> EditRequest {
        EditRequest {
            content_lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            hash_bounds: [
                HashRef {
                    hash: from.as_ref().to_owned(),
                },
                HashRef {
                    hash: to.as_ref().to_owned(),
                },
            ],
        }
    }

    #[test]
    fn replaces_a_range_addressed_by_anchors() {
        let anchors = hashes(SAMPLE);
        let request = edit(&anchors[1], &anchors[1], &["  console.log(\"hi\");"]);
        let applied = apply_edit(SAMPLE, &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(
            applied.content,
            "function hello() {\n  console.log(\"hi\");\n}\n\n// end\n"
        );
        assert_eq!(applied.first_changed_line, Some(2));
        assert_eq!(applied.last_changed_line, Some(2));
        assert!(applied.noop.is_none());
    }

    #[test]
    fn detects_a_noop() {
        let anchors = hashes(SAMPLE);
        let request = edit(&anchors[1], &anchors[1], &["  console.log(\"world\");"]);
        let applied = apply_edit(SAMPLE, &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(applied.content, SAMPLE);
        let noop = applied.noop.expect("应当判定为 noop");
        assert_eq!(noop.loc, anchors[1].as_str());
        assert_eq!(noop.current_content, "  console.log(\"world\");");
    }

    #[test]
    fn rejects_stale_anchors_with_feedback() {
        let anchors = hashes(SAMPLE);
        let request = edit("ZZZ", "ZZZ", &["x"]);
        let error = apply_edit(SAMPLE, &request, Some(&anchors), Some("sample.js"), None)
            .expect_err("应当报过期锚点");
        assert_eq!(error.code(), ErrorCode::StaleAnchor);
        assert!(error.feedback_hashes().is_empty());
    }

    #[test]
    fn rejects_ambiguous_anchors() {
        let fake = [Anchor::new("aB3"), Anchor::new("aB3"), Anchor::new("cD4")];
        let request = edit("aB3", "aB3", &["x"]);
        let error = apply_edit("a\nb\nc\n", &request, Some(&fake), None, None)
            .expect_err("应当报歧义锚点");
        assert_eq!(error.code(), ErrorCode::AmbiguousAnchor);
    }

    #[test]
    fn refuses_to_empty_a_non_empty_file() {
        let anchors = hashes(SAMPLE);
        let request = edit(&anchors[0], &anchors[4], &[]);
        let error = apply_edit(SAMPLE, &request, Some(&anchors), None, None)
            .expect_err("应当拒绝清空文件");
        assert_eq!(error.code(), ErrorCode::WouldEmpty);
    }

    #[test]
    fn inserts_into_an_empty_file_via_the_single_empty_anchor() {
        let anchors = hashes("");
        assert_eq!(anchors.len(), 1);
        let request = edit(&anchors[0], &anchors[0], &["hello"]);
        let applied = apply_edit("", &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(applied.content, "hello");
        assert_eq!(applied.first_changed_line, Some(1));
        assert_eq!(applied.last_changed_line, Some(1));
    }

    #[test]
    fn swaps_reversed_ranges_with_a_warning() {
        let anchors = hashes(SAMPLE);
        let request = edit(&anchors[2], &anchors[0], &["x"]);
        let applied = apply_edit(SAMPLE, &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(applied.content, "x\n\n// end\n");
        assert!(
            applied
                .warnings
                .iter()
                .any(|warning| warning.contains("were reversed"))
        );
    }

    #[test]
    fn strips_hash_prefixes_pasted_into_replacement_text() {
        let anchors = hashes(SAMPLE);
        let request = edit(
            &anchors[1],
            &anchors[1],
            &[&format!("{}{HASH_SEP}  console.log(\"hi\");", anchors[1])],
        );
        let applied = apply_edit(SAMPLE, &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(
            applied.content,
            "function hello() {\n  console.log(\"hi\");\n}\n\n// end\n"
        );
        assert!(
            applied
                .warnings
                .iter()
                .any(|warning| warning.starts_with("[E_BARE_HASH_PREFIX]"))
        );
    }

    #[test]
    fn auto_removes_boundary_duplicates() {
        // 范围是第 2 行，但替换内容把第 3 行也写了一遍 → 自动删掉
        let anchors = hashes(SAMPLE);
        let request = edit(
            &anchors[1],
            &anchors[1],
            &["  console.log(\"hi\");", "}"],
        );
        let applied = apply_edit(SAMPLE, &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(
            applied.content,
            "function hello() {\n  console.log(\"hi\");\n}\n\n// end\n"
        );
        assert_eq!(applied.auto_fixes.len(), 1);
        assert_eq!(applied.auto_fixes[0].kind, "trailing");
    }

    #[test]
    fn enforces_the_served_range_guard() {
        let anchors = hashes(SAMPLE);
        let served: BTreeSet<Anchor> = [anchors[0], anchors[3]].into_iter().collect();
        let request = edit(&anchors[1], &anchors[1], &["  console.log(\"hi\");"]);
        let error = apply_edit(SAMPLE, &request, Some(&anchors), Some("sample.js"), Some(&served))
            .expect_err("应当被 served 守卫拦下");
        assert_eq!(error.code(), ErrorCode::RangeStale);
        assert!(!error.feedback_hashes().is_empty());
    }

    #[test]
    fn deletes_a_range_ending_at_the_last_line_without_trailing_newline() {
        let content = "a\nb\nc";
        let anchors = hashes(content);
        let request = edit(&anchors[1], &anchors[2], &[]);
        let applied = apply_edit(content, &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(applied.content, "a");
    }

    #[test]
    fn deletes_a_trailing_range_when_the_file_ends_with_a_newline() {
        let content = "a\nb\nc\n";
        let anchors = hashes(content);
        let request = edit(&anchors[1], &anchors[2], &[]);
        let applied = apply_edit(content, &request, Some(&anchors), None, None).expect("编辑失败");
        assert_eq!(applied.content, "a\n");
    }

    #[test]
    fn deletes_the_whole_file_content_only_via_write() {
        // 全范围删除会被 E_WOULD_EMPTY 拦下，这是有意的
        let content = "a\n";
        let anchors = hashes(content);
        let request = edit(&anchors[0], &anchors[0], &[]);
        let error = apply_edit(content, &request, Some(&anchors), None, None)
            .expect_err("应当拒绝清空");
        assert_eq!(error.code(), ErrorCode::WouldEmpty);
    }

    #[test]
    fn line_index_tracks_byte_offsets() {
        let index = build_index("ab\ncd\n");
        assert_eq!(index.lines, ["ab", "cd"]);
        assert_eq!(index.starts, [0, 3]);
        let index = build_index("错\n误\n");
        assert_eq!(index.starts, [0, 4]); // 中文各占 3 字节
    }
}
