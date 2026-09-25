//! 锚点校验与错误反馈渲染。
//!
//! 两道闸门，顺序不能换：
//!
//! 1. [`val_edit`] —— 两个锚点必须在当前文件里**唯一**命中。过期报 `E_STALE_ANCHOR`，
//!    重复报 `E_AMBIGUOUS_ANCHOR`，两者都随错误附上当前上下文的新锚点，模型下一轮
//!    可以直接用。
//! 2. [`assert_range_served`] —— 被替换范围内的每一行都必须是模型**真正见过**的
//!    （`hashline_read` 输出过、或 diff 里出现过）。这道闸门拦住「凭记忆编一个锚点」：
//!    编出来的哈希即使碰巧存在，也不在 served 集合里。

use std::borrow::Cow;
use std::collections::BTreeSet;

use astrcode_ext_common::arena::with_scratch;
use bumpalo::Bump;
use bumpalo::collections::Vec as ArenaVec;
use rustc_hash::FxHashMap;

use super::{
    error::{EditError, ErrorCode},
    hash::HASH_SEP,
    lines::{canon, clip_line},
    request::{EditRequest, HashRef},
};

/// `E_RANGE_STALE` 反馈里最多展示多少行，避免超长范围把错误正文撑爆。
const MAX_RANGE_STALE_LINES: usize = 100;

/// 已经定位到具体行的锚点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAnchor {
    /// 1 起。
    pub line: usize,
    pub hash: String,
}

/// 锚点已解析的编辑请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEdit {
    pub content_lines: Vec<String>,
    pub hash_bounds: [ResolvedAnchor; 2],
}

/// 锚点解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// 该哈希在当前文件里不存在。
    NotFound {
        hash: String,
        /// 另一半锚点解析成功时填上，用于展示「你大概想改的是这里」。
        context: Option<ResolvedAnchor>,
    },
    /// 该哈希匹配多行。
    Ambiguous { hash: String, candidates: Vec<usize> },
}

/// 替换内容与范围边界重叠的一行。
///
/// 模型常见的手误是「范围已经包含了某行，`replacement_text` 里又写了一遍」。
/// 原版会把这类行自动删掉，而不是让模型重来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryDup {
    pub kind: &'static str,
    pub replacement_line_index: usize,
}

/// [`val_edit`] 的结果。
#[derive(Debug, Clone)]
pub struct ValOutcome {
    /// 两个锚点都解析成功时给出；否则为 `None`，此时 `mismatches` 非空。
    pub resolved: Option<ResolvedEdit>,
    pub mismatches: Vec<Mismatch>,
    pub boundary_dups: Vec<BoundaryDup>,
}

/// 校验两个锚点，并收集「替换内容与边界重复」的候选行。
pub fn val_edit(
    edit: &EditRequest,
    file_lines: &[&str],
    file_hashes: &[String],
) -> Result<ValOutcome, EditError> {
    // 锚点索引是调用内部的临时量：每个锚点一个候选行下标的小表，整份文件建一遍。
    with_scratch(|bump| val_edit_in(bump, edit, file_lines, file_hashes))
}

fn val_edit_in(
    bump: &Bump,
    edit: &EditRequest,
    file_lines: &[&str],
    file_hashes: &[String],
) -> Result<ValOutcome, EditError> {
    assert_aligned(file_lines, file_hashes, "valEdit")?;

    let mut mismatches: Vec<Mismatch> = Vec::new();
    let mut boundary_dups: Vec<BoundaryDup> = Vec::new();

    let mut hash_index: FxHashMap<&str, ArenaVec<'_, usize>> = FxHashMap::default();
    for (index, hash) in file_hashes.iter().enumerate() {
        hash_index
            .entry(hash.as_str())
            .or_insert_with(|| ArenaVec::new_in(bump))
            .push(index + 1);
    }

    let start_resolved = resolve_or_record(&edit.hash_bounds[0], &hash_index, &mut mismatches);
    let end_resolved = resolve_or_record(&edit.hash_bounds[1], &hash_index, &mut mismatches);

    let (start, end) = match (start_resolved, end_resolved) {
        (Some(start), Some(end)) => (start, end),
        (start_resolved, end_resolved) => {
            // 一侧解析成功时，把它作为另一侧「过期锚点」的上下文提示。
            let (missing_hash, context) = if start_resolved.is_none() && end_resolved.is_some() {
                (edit.hash_bounds[0].hash.as_str(), end_resolved)
            } else if start_resolved.is_some() && end_resolved.is_none() {
                (edit.hash_bounds[1].hash.as_str(), start_resolved)
            } else {
                ("" as &str, None)
            };
            if let Some(context) = context
                && let Some(Mismatch::NotFound { context: slot, .. }) = mismatches
                    .iter_mut()
                    .rev()
                    .find(|mismatch| {
                        matches!(mismatch, Mismatch::NotFound { hash, .. } if hash == missing_hash)
                    })
            {
                *slot = Some(context);
            }
            return Ok(ValOutcome {
                resolved: None,
                mismatches,
                boundary_dups,
            });
        },
    };

    if start.line > end.line {
        return Err(EditError::new(
            ErrorCode::BadOp,
            format!(
                "Range start line {} must be <= end line {} (anchors {} and {}).",
                start.line, end.line, edit.hash_bounds[0].hash, edit.hash_bounds[1].hash
            ),
        ));
    }

    let end_line = end.line;
    let range_lines: Vec<&str> = file_lines[start.line - 1..end_line].to_vec();
    let canon_lines: Vec<Cow<'_, str>> = file_lines.iter().map(|line| canon(line)).collect();
    boundary_dups.extend(trailing_dups(&edit.content_lines, file_lines, end_line));
    boundary_dups.extend(leading_dups(&edit.content_lines, file_lines, start.line));
    boundary_dups.extend(first_new_after_dups(
        &edit.content_lines,
        &range_lines,
        &canon_lines,
        end_line,
    ));
    boundary_dups.extend(last_new_before_dups(
        &edit.content_lines,
        &range_lines,
        &canon_lines,
        start.line,
    ));

    Ok(ValOutcome {
        resolved: Some(ResolvedEdit {
            content_lines: edit.content_lines.clone(),
            hash_bounds: [start, end],
        }),
        mismatches,
        boundary_dups,
    })
}

/// served 守卫：范围内的每一行都必须展示给过模型。
pub fn assert_range_served(
    resolved: &ResolvedEdit,
    file_lines: &[&str],
    file_hashes: &[String],
    served: &BTreeSet<String>,
    file_path: Option<&str>,
) -> Result<(), EditError> {
    assert_aligned(file_lines, file_hashes, "assertRangeServed")?;

    let start_line = resolved.hash_bounds[0].line;
    let end_line = resolved.hash_bounds[1].line;
    let mismatch_lines: Vec<usize> = (start_line..=end_line)
        .filter(|line| !served.contains(file_hashes[line - 1].as_str()))
        .collect();
    if mismatch_lines.is_empty() {
        return Ok(());
    }

    let range_length = end_line - start_line + 1;
    let shown_length = range_length.min(MAX_RANGE_STALE_LINES);
    let mut rows = Vec::with_capacity(shown_length);
    let mut shown_hashes = Vec::with_capacity(shown_length);
    for line in start_line..start_line + shown_length {
        let hash = &file_hashes[line - 1];
        shown_hashes.push(hash.clone());
        rows.push(format!("{hash}{HASH_SEP}{}", file_lines[line - 1]));
    }

    let location = file_path
        .map(|path| format!(" in {path}"))
        .unwrap_or_default();
    let first = mismatch_lines[0];
    let mismatch_text = if mismatch_lines.len() == 1 {
        format!(
            "Line {first} of the replaced range (lines {start_line}-{end_line}){location} does not \
             match"
        )
    } else {
        format!(
            "{} of {range_length} line(s) in the replaced range (lines {start_line}-{end_line})\
             {location} do not match",
            mismatch_lines.len()
        )
    };
    let cap_hint = if range_length > shown_length {
        format!(
            "\n\n[The range has {range_length} lines; showing the first {shown_length}. Call \
             hashline_read with offset={} to see the rest.]",
            start_line + shown_length
        )
    } else {
        String::new()
    };

    let body = format!(
        "{mismatch_text} what was previously shown: the file changed on disk after the anchors \
         were read, or the line(s) were never shown. Nothing was modified. Current range with \
         fresh anchors:\n\n{}{cap_hint}",
        rows.join("\n")
    );
    Err(EditError::new(ErrorCode::RangeStale, body).with_feedback(shown_hashes))
}

/// 锚点校验失败的反馈。
///
/// `body` **不含**开头的 `[E_*]` 标记，由 [`EditError`] 统一拼上。原版把标记写死在
/// 正文里，这里拆开是为了让分类与正文永远一致；正文其余部分逐字照抄。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MismatchFeedback {
    pub code: ErrorCode,
    pub body: String,
    /// 随反馈一并展示的当前锚点，调用方把它们记进 served 集合。
    pub hashes: Vec<String>,
}

/// 渲染锚点校验失败反馈，同时返回随反馈展示的锚点。
pub fn fmt_mismatch_with_hashes(
    mismatches: &[Mismatch],
    file_lines: &[&str],
    file_hashes: &[String],
    file_path: Option<&str>,
) -> Result<MismatchFeedback, EditError> {
    assert_aligned(file_lines, file_hashes, "fmtMismatch")?;

    let mut out: Vec<String> = Vec::new();
    let mut hashes: Vec<String> = Vec::new();
    let not_found_count = mismatches
        .iter()
        .filter(|mismatch| matches!(mismatch, Mismatch::NotFound { .. }))
        .count();
    let ambiguous_count = mismatches
        .iter()
        .filter(|mismatch| matches!(mismatch, Mismatch::Ambiguous { .. }))
        .count();
    let location = file_path
        .map(|path| format!(" in {path}"))
        .unwrap_or_default();

    if not_found_count > 0 {
        let ref_list = mismatches
            .iter()
            .filter_map(|mismatch| match mismatch {
                Mismatch::NotFound { hash, .. } => Some(format!("\"{hash}\"")),
                Mismatch::Ambiguous { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(", ");
        let plural = if not_found_count > 1 { "s" } else { "" };
        out.push(format!(
            "{not_found_count} stale anchor{plural}{location}: {ref_list}. The \
             file content has changed since those anchors were read. Call hashline_read to get \
             fresh anchors, then copy the 3-char HASH of the start and end of the range you are \
             replacing into remove_from and remove_to of your next replace call."
        ));

        for mismatch in mismatches {
            let Mismatch::NotFound {
                context: Some(anchor),
                ..
            } = mismatch
            else {
                continue;
            };
            let from = anchor.line.saturating_sub(1).max(1);
            let to = (anchor.line + 1).min(file_lines.len());
            let mut rows = Vec::new();
            for line in from..=to {
                hashes.push(file_hashes[line - 1].clone());
                rows.push(format!(
                    "    {line}: {}{HASH_SEP}{}",
                    file_hashes[line - 1],
                    clip_line(file_lines[line - 1])
                ));
            }
            out.push(String::new());
            out.push(format!(
                "  Current context around resolved anchor \"{}\" (line {}):\n{}",
                anchor.hash,
                anchor.line,
                rows.join("\n")
            ));
        }
    }

    if ambiguous_count > 0 {
        // 没有过期块时，本块就是开头的块：标记由 [`EditError`] 统一拼上，正文里不再重复。
        // 作为第二个块时标记必须留在正文里，否则模型看不到「这一段是另一个问题」。
        let leading = out.is_empty();
        if !leading {
            out.push(String::new());
        }
        let plural = if ambiguous_count > 1 { "s" } else { "" };
        let marker = if leading { "" } else { "[E_AMBIGUOUS_ANCHOR] " };
        out.push(format!(
            "{marker}{ambiguous_count} ambiguous anchor{plural}{location}. Call \
             hashline_read to get fresh anchors, then copy the 3-char HASH of the start and end of \
             the range you are replacing into remove_from and remove_to of your next replace call."
        ));

        for mismatch in mismatches {
            let Mismatch::Ambiguous { hash, candidates } = mismatch else {
                continue;
            };
            let sample: Vec<usize> = candidates.iter().copied().take(5).collect();
            let more = if candidates.len() > sample.len() {
                format!(", ... (+{} more)", candidates.len() - sample.len())
            } else {
                String::new()
            };
            for line in &sample {
                hashes.push(file_hashes[line - 1].clone());
            }
            let lines = sample
                .iter()
                .map(|line| {
                    format!(
                        "    {line}: {}{HASH_SEP}{}",
                        file_hashes[line - 1],
                        clip_line(file_lines[line - 1])
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            out.push(format!(
                "  Hash \"{hash}\" matches lines {}{more}.\n{lines}",
                sample
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    Ok(MismatchFeedback {
        code: if not_found_count > 0 {
            ErrorCode::StaleAnchor
        } else {
            ErrorCode::AmbiguousAnchor
        },
        body: out.join("\n"),
        hashes,
    })
}

/// 在哈希索引里定位一个锚点，失败时把原因记进 `mismatches`。
fn resolve_or_record(
    reference: &HashRef,
    hash_index: &FxHashMap<&str, ArenaVec<'_, usize>>,
    mismatches: &mut Vec<Mismatch>,
) -> Option<ResolvedAnchor> {
    match hash_index.get(reference.hash.as_str()) {
        Some(candidates) if candidates.len() == 1 => Some(ResolvedAnchor {
            line: candidates[0],
            hash: reference.hash.clone(),
        }),
        Some(candidates) => {
            mismatches.push(Mismatch::Ambiguous {
                hash: reference.hash.clone(),
                candidates: candidates.as_slice().to_vec(),
            });
            None
        },
        None => {
            mismatches.push(Mismatch::NotFound {
                hash: reference.hash.clone(),
                context: None,
            });
            None
        },
    }
}

/// 哈希数组与行数组必须一一对应；不一致说明调用方写错了，不是模型的输入问题。
fn assert_aligned(
    file_lines: &[&str],
    file_hashes: &[String],
    label: &str,
) -> Result<(), EditError> {
    if file_hashes.len() != file_lines.len() {
        return Err(EditError::internal(format!(
            "{label}: fileHashes.length ({}) must match fileLines.length ({}).",
            file_hashes.len(),
            file_lines.len()
        )));
    }
    Ok(())
}

/// 替换内容末尾与范围之后的文件内容重复的部分。
fn trailing_dups(
    content_lines: &[String],
    file_lines: &[&str],
    end_line: usize,
) -> Vec<BoundaryDup> {
    let Some(start) = content_lines.iter().rposition(|line| !line.is_empty()) else {
        return Vec::new();
    };
    let max_k = (start + 1).min(file_lines.len().saturating_sub(end_line));
    let mut dups = Vec::new();
    for k in 0..max_k {
        if content_lines[start - k] != file_lines[end_line + k] {
            break;
        }
        dups.push(BoundaryDup {
            kind: "trailing",
            replacement_line_index: start - k,
        });
    }
    dups
}

/// 替换内容开头与范围之前的文件内容重复的部分。
fn leading_dups(
    content_lines: &[String],
    file_lines: &[&str],
    start_line: usize,
) -> Vec<BoundaryDup> {
    let Some(start) = content_lines.iter().position(|line| !line.is_empty()) else {
        return Vec::new();
    };
    let max_k = (content_lines.len() - start).min(start_line - 1);
    let mut dups = Vec::new();
    for k in 0..max_k {
        let Some(candidate) = start_line
            .checked_sub(2)
            .and_then(|base| base.checked_sub(k))
        else {
            break;
        };
        if content_lines[start + k] != file_lines[candidate] {
            break;
        }
        dups.push(BoundaryDup {
            kind: "leading",
            replacement_line_index: start + k,
        });
    }
    dups
}

/// 替换内容里「第一个新行」之后与范围之后的文件内容重复的一段。
fn first_new_after_dups(
    content_lines: &[String],
    range_lines: &[&str],
    canon_lines: &[Cow<'_, str>],
    end_line: usize,
) -> Vec<BoundaryDup> {
    let Some(first_new) = find_new_edge(content_lines, range_lines, false) else {
        return Vec::new();
    };
    let max_k = (content_lines.len() - first_new).min(canon_lines.len().saturating_sub(end_line));
    let mut run_len = 0usize;
    while run_len < max_k
        && canon(&content_lines[first_new + run_len]) == canon_lines[end_line + run_len]
    {
        run_len += 1;
    }
    if run_len == 0 || !section_is_unique(canon_lines, end_line, run_len) {
        return Vec::new();
    }
    (0..run_len)
        .map(|k| BoundaryDup {
            kind: "first-new-after",
            replacement_line_index: first_new + k,
        })
        .collect()
}

/// 替换内容里「最后一个新行」之前与范围之前的文件内容重复的一段。
fn last_new_before_dups(
    content_lines: &[String],
    range_lines: &[&str],
    canon_lines: &[Cow<'_, str>],
    start_line: usize,
) -> Vec<BoundaryDup> {
    let Some(last_new) = find_new_edge(content_lines, range_lines, true) else {
        return Vec::new();
    };
    let max_k = (last_new + 1).min(start_line - 1);
    let mut run_len = 0usize;
    while run_len < max_k {
        let Some(candidate) = start_line
            .checked_sub(2)
            .and_then(|base| base.checked_sub(run_len))
        else {
            break;
        };
        if canon(&content_lines[last_new - run_len]) != canon_lines[candidate] {
            break;
        }
        run_len += 1;
    }
    if run_len == 0 {
        return Vec::new();
    }
    let section_start = start_line - 1 - run_len;
    if !section_is_unique(canon_lines, section_start, run_len) {
        return Vec::new();
    }
    (0..run_len)
        .map(|k| BoundaryDup {
            kind: "last-new-before",
            replacement_line_index: last_new - k,
        })
        .collect()
}

/// 从替换内容里找出第一个（或从末尾数第一个）不属于被替换范围的行。
///
/// 用多重集扣减而不是逐个查找，是为了让「同内容行重复出现」也能正确配对。
fn find_new_edge(
    content_lines: &[String],
    range_lines: &[&str],
    from_end: bool,
) -> Option<usize> {
    let mut multiset: FxHashMap<Cow<'_, str>, usize> = FxHashMap::default();
    for line in range_lines {
        *multiset.entry(canon(line)).or_insert(0) += 1;
    }

    let indexes: Box<dyn Iterator<Item = usize>> = if from_end {
        Box::new((0..content_lines.len()).rev())
    } else {
        Box::new(0..content_lines.len())
    };
    for index in indexes {
        let line = content_lines[index].as_str();
        if line.is_empty() {
            continue;
        }
        let key = canon(line);
        match multiset.get_mut(key.as_ref()) {
            Some(count) if *count > 0 => *count -= 1,
            _ => return Some(index),
        }
    }
    None
}

/// `canon_lines[start..start+length]` 在整份文件里是否只出现一次。
fn section_is_unique(canon_lines: &[Cow<'_, str>], start: usize, length: usize) -> bool {
    let mut count = 0usize;
    for index in 0..=canon_lines.len().saturating_sub(length) {
        if index + length > canon_lines.len() {
            break;
        }
        let identical = (0..length).all(|k| canon_lines[index + k] == canon_lines[start + k]);
        if !identical {
            continue;
        }
        count += 1;
        if count > 1 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashline::{hash::line_hashes_pure, lines::split_lines};

    const SAMPLE: &str = "function hello() {\n  console.log(\"world\");\n}\n\n// end\n";

    fn sample_lines() -> Vec<&'static str> {
        split_lines(SAMPLE)
    }

    fn edit(from: &str, to: &str, lines: &[&str]) -> EditRequest {
        EditRequest {
            content_lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            hash_bounds: [
                HashRef { hash: from.to_owned() },
                HashRef { hash: to.to_owned() },
            ],
        }
    }

    #[test]
    fn val_edit_resolves_unique_anchors() {
        let lines = sample_lines();
        let hashes = line_hashes_pure(SAMPLE).expect("分配失败");
        let request = edit(&hashes[1], &hashes[1], &["x"]);
        let outcome = val_edit(&request, &lines, &hashes).expect("校验失败");
        let resolved = outcome.resolved.expect("应当解析成功");
        assert_eq!(resolved.hash_bounds[0].line, 2);
        assert_eq!(resolved.hash_bounds[1].line, 2);
        assert!(outcome.mismatches.is_empty());
    }

    #[test]
    fn val_edit_reports_stale_and_ambiguous_anchors() {
        let lines = sample_lines();
        let hashes = line_hashes_pure(SAMPLE).expect("分配失败");

        let stale = edit("ZZZ", "ZZZ", &["x"]);
        let outcome = val_edit(&stale, &lines, &hashes).expect("校验失败");
        assert!(outcome.resolved.is_none());
        assert_eq!(outcome.mismatches.len(), 2);
        assert!(matches!(outcome.mismatches[0], Mismatch::NotFound { .. }));

        // 伪造重复哈希，触发歧义分支
        let duplicated = ["aB3".to_owned(), "aB3".to_owned(), "cD4".to_owned()];
        let ambiguous = edit("aB3", "aB3", &["x"]);
        let outcome = val_edit(&ambiguous, &["a", "b", "c"], &duplicated).expect("校验失败");
        assert_eq!(outcome.mismatches.len(), 2);
        assert!(matches!(
            &outcome.mismatches[0],
            Mismatch::Ambiguous { candidates, .. } if candidates == &vec![1, 2]
        ));
    }

    #[test]
    fn val_edit_attaches_context_when_only_one_side_resolves() {
        let lines = sample_lines();
        let hashes = line_hashes_pure(SAMPLE).expect("分配失败");
        let request = edit("ZZZ", &hashes[2], &["x"]);
        let outcome = val_edit(&request, &lines, &hashes).expect("校验失败");
        let Mismatch::NotFound { context, .. } = &outcome.mismatches[0] else {
            panic!("应当报过期锚点");
        };
        assert_eq!(context.as_ref().expect("应当带上上下文").line, 3);
    }

    #[test]
    fn val_edit_rejects_reversed_ranges() {
        let lines = sample_lines();
        let hashes = line_hashes_pure(SAMPLE).expect("分配失败");
        let request = edit(&hashes[3], &hashes[0], &["x"]);
        let error = val_edit(&request, &lines, &hashes).expect_err("应当报错");
        assert_eq!(error.code(), ErrorCode::BadOp);
    }

    #[test]
    fn fmt_mismatch_lists_stale_anchors_with_context_rows() {
        let lines = sample_lines();
        let hashes = line_hashes_pure(SAMPLE).expect("分配失败");
        let request = edit("ZZZ", &hashes[2], &["x"]);
        let outcome = val_edit(&request, &lines, &hashes).expect("校验失败");
        let feedback =
            fmt_mismatch_with_hashes(&outcome.mismatches, &lines, &hashes, Some("sample.js"))
                .expect("渲染失败");

        assert_eq!(feedback.code, ErrorCode::StaleAnchor);
        assert!(feedback.body.starts_with("1 stale anchor in sample.js: \"ZZZ\"."));
        assert!(feedback.body.contains("Current context around resolved anchor"));
        // 上下文行把周围的锚点一并展示，模型可以直接复用
        assert!(feedback.hashes.contains(&hashes[1]));
        assert!(feedback.hashes.contains(&hashes[2]));
    }

    #[test]
    fn fmt_mismatch_reports_ambiguous_candidates_with_cap() {
        let lines: Vec<&str> = vec!["a", "b", "c", "d", "e", "f", "g"];
        let hashes: Vec<String> = (0..7).map(|_| "aB3".to_owned()).collect();
        let mismatches = vec![Mismatch::Ambiguous {
            hash: "aB3".into(),
            candidates: (1..=7).collect(),
        }];
        let feedback = fmt_mismatch_with_hashes(&mismatches, &lines, &hashes, None)
            .expect("渲染失败");
        assert_eq!(feedback.code, ErrorCode::AmbiguousAnchor);
        assert!(feedback.body.starts_with("1 ambiguous anchor."));
        assert!(feedback.body.contains("matches lines 1, 2, 3, 4, 5, ... (+2 more)"));
        assert_eq!(feedback.hashes.len(), 5);
    }

    /// 两类锚点同时出错时，分类取第一个（过期），歧义块的标记留在正文里。
    #[test]
    fn fmt_mismatch_keeps_the_second_marker_inline() {
        let lines: Vec<&str> = vec!["a", "b"];
        let hashes: Vec<String> = vec!["aB3".to_owned(), "aB3".to_owned()];
        let mismatches = vec![
            Mismatch::NotFound {
                hash: "ZZZ".into(),
                context: None,
            },
            Mismatch::Ambiguous {
                hash: "aB3".into(),
                candidates: vec![1, 2],
            },
        ];
        let feedback =
            fmt_mismatch_with_hashes(&mismatches, &lines, &hashes, None).expect("渲染失败");
        assert_eq!(feedback.code, ErrorCode::StaleAnchor);
        assert!(feedback.body.contains("\n\n[E_AMBIGUOUS_ANCHOR] 1 ambiguous anchor."));
    }

    #[test]
    fn assert_range_served_passes_when_every_line_was_shown() {
        let lines = sample_lines();
        let hashes = line_hashes_pure(SAMPLE).expect("分配失败");
        let resolved = ResolvedEdit {
            content_lines: vec!["x".into()],
            hash_bounds: [
                ResolvedAnchor { line: 2, hash: hashes[1].clone() },
                ResolvedAnchor { line: 3, hash: hashes[2].clone() },
            ],
        };
        let served: BTreeSet<String> = [hashes[1].clone(), hashes[2].clone()].into_iter().collect();
        assert!(assert_range_served(&resolved, &lines, &hashes, &served, None).is_ok());
    }

    #[test]
    fn assert_range_served_rejects_unseen_lines_with_fresh_anchors() {
        let lines = sample_lines();
        let hashes = line_hashes_pure(SAMPLE).expect("分配失败");
        let resolved = ResolvedEdit {
            content_lines: vec!["x".into()],
            hash_bounds: [
                ResolvedAnchor { line: 1, hash: hashes[0].clone() },
                ResolvedAnchor { line: 3, hash: hashes[2].clone() },
            ],
        };
        let served: BTreeSet<String> = [hashes[0].clone()].into_iter().collect();
        let error = assert_range_served(&resolved, &lines, &hashes, &served, Some("sample.js"))
            .expect_err("应当报范围过期");

        assert_eq!(error.code(), ErrorCode::RangeStale);
        assert!(error.render().starts_with("[E_RANGE_STALE] 2 of 3 line(s)"));
        assert!(error.render().contains("Nothing was modified"));
        assert_eq!(error.feedback_hashes().len(), 3);
    }

    #[test]
    fn assert_range_served_caps_the_shown_rows() {
        let lines: Vec<&str> = vec!["x"; 150];
        let hashes: Vec<String> = (0..150).map(|index| format!("{index:03}")).collect();
        let resolved = ResolvedEdit {
            content_lines: vec!["x".into()],
            hash_bounds: [
                ResolvedAnchor { line: 1, hash: hashes[0].clone() },
                ResolvedAnchor { line: 150, hash: hashes[149].clone() },
            ],
        };
        let error = assert_range_served(&resolved, &lines, &hashes, &BTreeSet::new(), None)
            .expect_err("应当报范围过期");
        assert!(error.render().contains("The range has 150 lines; showing the first 100"));
        assert!(error.render().contains("offset=101"));
        assert_eq!(error.feedback_hashes().len(), 100);
    }

    /// 边界重复的判定方向是原版的既有行为，不是笔误：
    ///
    /// - `trailing` 拿**替换内容从后往前**的行去对**范围之后从前往后**的行；
    /// - `leading` 拿替换内容从前往后的行去对范围之前从后往前的行。
    ///
    /// 因此「替换内容 = 后续行的正序重复」由 `first_new_after` 捕获，而 `trailing`
    /// 捕获的是倒序重复。下面的期望值取自原版 `probe.js` 的实测输出。
    #[test]
    fn trailing_and_leading_dups_detect_restated_boundaries() {
        let file_lines = vec!["a", "b", "c", "d"];

        // 范围是第 2 行，替换内容倒序重复了第 3、4 行
        let content_lines = vec!["d".to_owned(), "c".to_owned()];
        let dups = trailing_dups(&content_lines, &file_lines, 2);
        assert_eq!(dups.len(), 2);
        assert_eq!(dups[0].replacement_line_index, 1);
        assert_eq!(dups[1].replacement_line_index, 0);

        // 范围是第 3 行，替换内容开头重复了第 2 行
        let content_lines = vec!["b".to_owned(), "C".to_owned()];
        let dups = leading_dups(&content_lines, &file_lines, 3);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].replacement_line_index, 0);

        // 无重复时一条也不报
        let content_lines = vec!["B".to_owned()];
        assert!(trailing_dups(&content_lines, &file_lines, 2).is_empty());
        assert!(leading_dups(&content_lines, &file_lines, 3).is_empty());
    }

    #[test]
    fn section_is_unique_counts_occurrences() {
        let lines: Vec<Cow<'_, str>> = ["a", "b", "c", "a", "b"]
            .iter()
            .map(|line| Cow::Borrowed(*line))
            .collect();
        assert!(!section_is_unique(&lines, 0, 2)); // "a b" 出现两次
        assert!(section_is_unique(&lines, 1, 2)); // "b c" 只出现一次
        assert!(section_is_unique(&lines, 0, 5)); // 整段唯一
    }
}
