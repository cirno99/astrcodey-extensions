//! `hashline_read`：把文件读成 `HASH│content` 行。
//!
//! 读取同时承担一件事：把「模型见过这些锚点」记进 served 集合。`replace` 的
//! served 守卫完全依赖这一步，所以读取路径上的每次展示都必须记账——包括分页
//! 展示的每一页；而因为输出超限被截掉、或因为单行过长被整行隐去的那些行没有
//! 展示过，就不记账。

use std::path::Path;

use serde::Deserialize;

use crate::{
    fsops,
    hashline::{
        Anchor, HASH_SEP,
        error::{EditError, ErrorCode},
        hash::HASH_SPACE,
        lines::split_lines,
    },
    state::SessionState,
    tools::ToolOutcome,
};

/// 单次读取的默认行数上限。
const READ_LIMIT: usize = 2000;
/// 单行内容的展示上限：超过就整行不展示（锚点要求整行内容，截断的行无法安全编辑）。
const MAX_READ_LINE_BYTES: usize = 200 * 1024;
/// 单次输出的字节上限。
const MAX_OUTPUT_BYTES: usize = 200 * 1024;
/// `raw` 模式下单行的字符上限。
const RAW_LINE_CHARS: usize = 2000;
/// 锚点容量，也就是行数上限。
const MAX_HASH_LINES: usize = HASH_SPACE;

#[derive(Debug, Deserialize)]
pub struct ReadArgs {
    pub path: String,
    #[serde(default)]
    pub offset: Option<f64>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub raw: Option<bool>,
}

pub fn execute(
    args: &ReadArgs,
    working_dir: &Path,
    state: &mut SessionState,
) -> Result<ToolOutcome, EditError> {
    let target = fsops::resolve_path(working_dir, &args.path);
    let display_path = args.path.clone();
    let file = fsops::read_raw_text(&target, &display_path)?;

    let all_lines = split_lines(&file.normalized);
    let total = all_lines.len();
    if total > MAX_HASH_LINES {
        return Err(too_many_lines(&display_path));
    }

    let offset = positive_int(args.offset, "offset")?.unwrap_or(1);
    let limit = positive_int(args.limit, "limit")?
        .unwrap_or(READ_LIMIT)
        .min(READ_LIMIT);

    if args.raw == Some(true) {
        return Ok(ToolOutcome::ok(render_raw(
            &display_path,
            &all_lines,
            total,
            offset,
            limit,
        )?));
    }

    let hashes = state.hashes_for(&display_path, &file.normalized)?;

    if file.normalized.is_empty() {
        let empty_hash = hashes.first().copied().unwrap_or(Anchor::new("AuN"));
        state.record_served(&display_path, std::slice::from_ref(&empty_hash));
        return Ok(ToolOutcome::ok(format!(
            "<path>{display_path}</path>\n<type>file</type>\n<content>\n{empty_hash}{HASH_SEP}\n\
             [File is empty. Use replace to insert content.]\n</content>"
        )));
    }

    if offset > total {
        return Err(offset_past_end(offset, total));
    }

    let end_index = (offset - 1 + limit).min(total);
    let mut rows: Vec<String> = Vec::new();
    let mut served: Vec<Anchor> = Vec::new();
    let mut budget = RowBudget::default();
    let mut capped = false;

    for index in offset - 1..end_index {
        let line = all_lines[index];
        let hash = &hashes[index];
        let row = format!("{hash}{HASH_SEP}{line}");
        if row.len() > MAX_READ_LINE_BYTES {
            // 锚点要求整行内容，所以超长行只能整行不给，并指一条绕行路径。
            let notice = format!(
                "[Line {} is {} bytes, exceeds {MAX_READ_LINE_BYTES}; content not shown because \
                 hashline anchors require full lines. Inspect with bash: sed -n '{}p' <path> | \
                 head -c {MAX_READ_LINE_BYTES}]",
                index + 1,
                row.len(),
                index + 1
            );
            if !budget.push(&mut rows, notice) {
                capped = true;
                break;
            }
            continue;
        }
        if !budget.push(&mut rows, row) {
            capped = true;
            break;
        }
        served.push(hash.clone());
    }

    let end_line = offset + rows.len() - 1;
    let footer = if capped {
        format!(
            "[Showing lines {offset}-{end_line} of {total} ({MAX_OUTPUT_BYTES} byte limit). Use \
             offset={} to continue.]",
            end_line + 1
        )
    } else if end_line < total {
        format!(
            "[Showing lines {offset}-{end_line} of {total}. Use offset={} to continue.]",
            end_line + 1
        )
    } else {
        format!("[End of file - total {total} lines]")
    };

    state.record_served(&display_path, &served);

    let utf8_note = if file.had_utf8_errors {
        "\n\n[Non-UTF-8 bytes shown as U+FFFD; editing rewrites the file as UTF-8.]"
    } else {
        ""
    };
    let body = if rows.is_empty() {
        footer
    } else {
        format!("{}\n\n{footer}", rows.join("\n"))
    };
    Ok(ToolOutcome::ok(format!(
        "<path>{display_path}</path>\n<type>file</type>\n<content>\n{body}\n</content>{utf8_note}"
    )))
}

/// `raw: true`：普通带行号输出，不带锚点，也不记 served（不是给编辑用的）。
fn render_raw(
    display_path: &str,
    all_lines: &[&str],
    total: usize,
    offset: usize,
    limit: usize,
) -> Result<String, EditError> {
    if offset > total {
        return Err(offset_past_end(offset, total));
    }
    let end_index = (offset - 1 + limit).min(total);
    let mut rows: Vec<String> = Vec::new();
    let mut budget = RowBudget::default();
    let mut capped = false;

    for (index, line) in all_lines
        .iter()
        .enumerate()
        .take(end_index)
        .skip(offset - 1)
    {
        let line = if line.chars().count() > RAW_LINE_CHARS {
            format!(
                "{}... (line truncated to {RAW_LINE_CHARS} chars)",
                line.chars().take(RAW_LINE_CHARS).collect::<String>()
            )
        } else {
            (*line).to_owned()
        };
        if !budget.push(&mut rows, format!("{}: {line}", index + 1)) {
            capped = true;
            break;
        }
    }

    let end_line = offset + rows.len() - 1;
    let footer = if capped {
        format!(
            "(Output capped. Showing lines {offset}-{end_line}. Use offset={} to continue.)",
            end_line + 1
        )
    } else if end_line < total {
        format!(
            "(Showing lines {offset}-{end_line} of {total}. Use offset={} to continue.)",
            end_line + 1
        )
    } else {
        format!("(End of file - total {total} lines)")
    };
    let body = if rows.is_empty() {
        footer
    } else {
        format!("{}\n\n{footer}", rows.join("\n"))
    };
    Ok(format!(
        "<path>{display_path}</path>\n<type>file</type>\n<content>\n{body}\n</content>"
    ))
}

/// 逐行累加的字节预算。与原版一致：只统计行本身，不计行间换行符。
#[derive(Debug, Default)]
struct RowBudget {
    bytes: usize,
}

impl RowBudget {
    /// 放得下就收下并返回 `true`；放不下返回 `false`（调用方据此收尾）。
    fn push(&mut self, rows: &mut Vec<String>, row: String) -> bool {
        if self.bytes + row.len() <= MAX_OUTPUT_BYTES {
            self.bytes += row.len();
            rows.push(row);
            true
        } else {
            false
        }
    }
}

/// 校验 `offset` / `limit` 是正整数。
fn positive_int(value: Option<f64>, name: &str) -> Result<Option<usize>, EditError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !value.is_finite() || value.fract() != 0.0 || value < 1.0 {
        return Err(EditError::new(
            ErrorCode::BadShape,
            format!("Field \"{name}\" must be a positive integer."),
        ));
    }
    Ok(Some(value as usize))
}

fn offset_past_end(offset: usize, total: usize) -> EditError {
    EditError::plain(format!(
        "Offset {offset} is beyond end of file ({total} lines total). Use offset=1 to read from the \
         start, or offset={total} to read the last line."
    ))
}

fn too_many_lines(display_path: &str) -> EditError {
    EditError::new(
        ErrorCode::FileTooLarge,
        format!(
            "{display_path} has more than {MAX_HASH_LINES} lines, exceeding the hashline anchor \
             limit. Use write or a non-line-based approach."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state(name: &str) -> (std::path::PathBuf, SessionState) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-hashline-read-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let state = SessionState::load(dir.join("state.json"));
        (dir, state)
    }

    fn args(path: &str) -> ReadArgs {
        ReadArgs {
            path: path.to_owned(),
            offset: None,
            limit: None,
            raw: None,
        }
    }

    #[test]
    fn reads_anchored_rows_and_records_served() {
        let (dir, mut state) = temp_state("rows");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").expect("写入失败");
        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("读取失败");

        assert!(!outcome.is_error);
        assert!(
            outcome
                .text
                .starts_with("<path>a.txt</path>\n<type>file</type>\n<content>\n")
        );
        assert!(outcome.text.contains("[End of file - total 2 lines]"));
        assert_eq!(state.served_set("a.txt").map(|set| set.len()), Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_yields_a_single_anchor_row() {
        let (dir, mut state) = temp_state("empty");
        std::fs::write(dir.join("a.txt"), "").expect("写入失败");
        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("读取失败");

        assert!(
            outcome
                .text
                .contains("[File is empty. Use replace to insert content.]")
        );
        assert_eq!(state.served_set("a.txt").map(|set| set.len()), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pagination_only_marks_the_returned_page_as_served() {
        let (dir, mut state) = temp_state("paging");
        let content: String = (0..10).map(|index| format!("line{index}\n")).collect();
        std::fs::write(dir.join("a.txt"), &content).expect("写入失败");

        let mut page = args("a.txt");
        page.limit = Some(3.0);
        let outcome = execute(&page, &dir, &mut state).expect("读取失败");
        assert!(
            outcome
                .text
                .contains("[Showing lines 1-3 of 10. Use offset=4 to continue.]")
        );
        assert_eq!(state.served_set("a.txt").map(|set| set.len()), Some(3));

        page.offset = Some(4.0);
        execute(&page, &dir, &mut state).expect("读取失败");
        assert_eq!(state.served_set("a.txt").map(|set| set.len()), Some(6));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn raw_mode_returns_numbered_lines_without_anchors_or_served() {
        let (dir, mut state) = temp_state("raw");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").expect("写入失败");
        let mut request = args("a.txt");
        request.raw = Some(true);
        let outcome = execute(&request, &dir, &mut state).expect("读取失败");

        assert!(outcome.text.contains("1: one\n2: two"));
        assert!(!outcome.text.contains(HASH_SEP));
        assert!(state.served_set("a.txt").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn raw_mode_still_rejects_a_bad_offset() {
        let (dir, mut state) = temp_state("raw-offset");
        std::fs::write(dir.join("a.txt"), "one\n").expect("写入失败");
        let mut request = args("a.txt");
        request.raw = Some(true);
        request.offset = Some(0.0);
        let error = execute(&request, &dir, &mut state).expect_err("应当报参数错误");
        assert_eq!(error.code(), ErrorCode::BadShape);
        assert_eq!(
            error.render(),
            "[E_BAD_SHAPE] Field \"offset\" must be a positive integer."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offset_past_the_end_reports_the_total() {
        let (dir, mut state) = temp_state("past-end");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").expect("写入失败");
        let mut request = args("a.txt");
        request.offset = Some(9.0);
        let error = execute(&request, &dir, &mut state).expect_err("应当报越界");
        assert!(
            error
                .render()
                .contains("beyond end of file (2 lines total)")
        );
        assert!(error.render().contains("offset=2 to read the last line"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_non_integer_limit_is_rejected() {
        let (dir, mut state) = temp_state("bad-limit");
        std::fs::write(dir.join("a.txt"), "one\n").expect("写入失败");
        let mut request = args("a.txt");
        request.limit = Some(1.5);
        let error = execute(&request, &dir, &mut state).expect_err("应当报参数错误");
        assert!(
            error
                .render()
                .contains("Field \"limit\" must be a positive integer.")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_is_capped_at_the_byte_budget() {
        let (dir, mut state) = temp_state("cap");
        // 每行约 200 字节，1200 行超过 200KB 上限
        let line = "x".repeat(199);
        let content: String = (0..1200).map(|_| format!("{line}\n")).collect();
        std::fs::write(dir.join("a.txt"), &content).expect("写入失败");
        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("读取失败");

        assert!(outcome.text.contains("byte limit). Use offset="));
        assert!(outcome.text.len() < MAX_OUTPUT_BYTES + 4096);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_over_long_line_is_withheld_with_a_workaround() {
        let (dir, mut state) = temp_state("long-line");
        let long = "y".repeat(MAX_READ_LINE_BYTES + 10);
        std::fs::write(dir.join("a.txt"), format!("{long}\nshort\n")).expect("写入失败");
        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("读取失败");

        assert!(outcome.text.contains("exceeds"));
        assert!(
            outcome
                .text
                .contains("hashline anchors require full lines")
        );
        // 超长行没有展示，因此不进 served
        assert_eq!(state.served_set("a.txt").map(|set| set.len()), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
