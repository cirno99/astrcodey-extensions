//! `undo_last_replace`：回滚某个文件最后一次 `replace`。
//!
//! 安全性来自一次内容比对：只有当磁盘上的内容**逐字节等于**上次 `replace` 写下的
//! 结果时才允许回滚。文件被外部改过就拒绝（`[E_UNDO_STALE]`），因为回滚会覆盖那些
//! 改动。拒绝时顺手丢弃还原点——它已经对不上当前文件，留着只会反复误导。

use std::{fs, path::Path};

use serde::Deserialize;

use crate::{
    fsops,
    hashline::{
        diff::gen_diff,
        error::EditError,
        lines::{restore_endings, split_lines, strip_bom, to_lf},
    },
    state::SessionState,
    tools::{DIFF_CONTEXT_LINES, ToolOutcome, render_diff},
};

#[derive(Debug, Deserialize)]
pub struct UndoArgs {
    pub path: String,
}

pub fn execute(
    args: &UndoArgs,
    working_dir: &Path,
    state: &mut SessionState,
) -> Result<ToolOutcome, EditError> {
    let target = fsops::resolve_path(working_dir, &args.path);
    let key = args.path.clone();

    let Some(record) = state.undo_record(&key).cloned() else {
        return Ok(ToolOutcome::ok(format!(
            "No undo history for {}. There is no previous replace to revert.",
            args.path
        )));
    };

    let Ok(bytes) = fs::read(&target) else {
        state.clear_undo(&key);
        let _ = state.save();
        return Ok(ToolOutcome::error(format!(
            "[E_UNDO_STALE] Cannot undo last replace on {}: the file no longer exists. Call \
             hashline_read to inspect the current state.",
            args.path
        )));
    };
    let current_raw = String::from_utf8_lossy(&bytes).into_owned();

    let expected = format!(
        "{}{}",
        record.bom,
        restore_endings(&record.result_content, record.ending_kind())
    );
    if current_raw != expected {
        state.clear_undo(&key);
        let _ = state.save();
        return Ok(ToolOutcome::error(format!(
            "[E_UNDO_STALE] Cannot undo last replace on {}: the file was modified after the \
             replace, so undoing would overwrite those changes. Call hashline_read to inspect the \
             current state.",
            args.path
        )));
    }

    let (_, current_stripped) = strip_bom(&current_raw);
    let current_normalized = to_lf(current_stripped);
    let current_hashes = state.hashes_for(&key, &current_normalized)?;
    let undo_diff = gen_diff(
        &current_normalized,
        &record.content,
        DIFF_CONTEXT_LINES,
        Some(&record.hashes),
        Some(&current_hashes),
    )
    .0;

    let payload = format!(
        "{}{}",
        record.bom,
        restore_endings(&record.content, record.ending_kind())
    );
    if let Err(error) = fsops::write_text(&target, &payload) {
        return Err(EditError::plain(format!("replace failed: {error}")));
    }

    state.clear_undo(&key);
    state.put_snapshot(&key, &record.content, record.hashes.clone());
    state.record_served_from_diff(&key, &undo_diff);
    let _ = state.save();

    let line_delta = split_lines(&record.result_content).len() as isize
        - split_lines(&record.content).len() as isize;
    let mut parts = vec![format!("Undone last replace on {}.", args.path)];
    if line_delta != 0 {
        parts.push(format!(
            "Removed {} line(s) that were added and restored {} line(s) that were removed.",
            line_delta.max(0),
            (-line_delta).max(0)
        ));
    }
    parts.push(
        "File reverted to previous state. Call `hashline_read` to get fresh anchors for follow-up \
         edits."
            .to_owned(),
    );
    Ok(ToolOutcome::ok(format!(
        "{}{}",
        parts.join("\n"),
        render_diff(&undo_diff)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashline::hash::line_hashes_pure;
    use crate::state::UndoRecord;

    fn temp_state(name: &str) -> (std::path::PathBuf, SessionState) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-hashline-undo-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let state = SessionState::load(dir.join("state.json"));
        (dir, state)
    }

    fn args(path: &str) -> UndoArgs {
        UndoArgs {
            path: path.to_owned(),
        }
    }

    /// 造一个「刚被 replace 过」的文件与还原点。
    fn after_replace(dir: &Path, state: &mut SessionState, path: &str) -> Vec<String> {
        let before = "one\ntwo\n";
        let after = "one\nTWO\n";
        std::fs::write(dir.join(path), after).expect("写入失败");
        let hashes = line_hashes_pure(before).expect("分配失败");
        state.set_undo(
            path,
            UndoRecord {
                content: before.to_owned(),
                bom: String::new(),
                ending: "\n".to_owned(),
                hashes: hashes.clone(),
                result_content: after.to_owned(),
            },
        );
        hashes
    }

    #[test]
    fn without_history_it_says_so_without_failing() {
        let (dir, mut state) = temp_state("no-history");
        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("执行失败");
        assert!(!outcome.is_error);
        assert!(outcome.text.contains("No undo history for a.txt."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reverts_the_file_and_restores_the_previous_anchors() {
        let (dir, mut state) = temp_state("revert");
        let hashes = after_replace(&dir, &mut state, "a.txt");

        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("回滚失败");

        assert!(!outcome.is_error);
        assert!(outcome.text.starts_with("Undone last replace on a.txt."));
        assert!(outcome.text.contains("Call `hashline_read` to get fresh anchors"));
        assert!(outcome.text.contains("Diff (HASH│anchored"));
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            "one\ntwo\n"
        );
        // 还原点用完即弃，锚点快照回到编辑前
        assert!(state.undo_record("a.txt").is_none());
        assert_eq!(state.hashes_for("a.txt", "one\ntwo\n").expect("计算失败"), hashes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_line_deltas_when_the_edit_grew_the_file() {
        let (dir, mut state) = temp_state("delta");
        let before = "one\ntwo\n";
        std::fs::write(dir.join("a.txt"), "one\nTWO\nEXTRA\n").expect("写入失败");
        let hashes = line_hashes_pure(before).expect("分配失败");
        state.set_undo(
            "a.txt",
            UndoRecord {
                content: before.to_owned(),
                bom: String::new(),
                ending: "\n".to_owned(),
                hashes,
                result_content: "one\nTWO\nEXTRA\n".to_owned(),
            },
        );

        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("回滚失败");
        assert!(
            outcome
                .text
                .contains("Removed 1 line(s) that were added and restored 0 line(s)")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_undo_when_the_file_changed_afterwards() {
        let (dir, mut state) = temp_state("modified");
        after_replace(&dir, &mut state, "a.txt");
        std::fs::write(dir.join("a.txt"), "one\nTWO\nMORE\n").expect("写入失败");

        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("执行失败");

        assert!(outcome.is_error);
        assert!(outcome.text.starts_with("[E_UNDO_STALE]"));
        assert!(outcome.text.contains("the file was modified after the replace"));
        // 拒绝的同时丢弃已对不上的还原点
        assert!(state.undo_record("a.txt").is_none());
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            "one\nTWO\nMORE\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_undo_when_the_file_is_gone() {
        let (dir, mut state) = temp_state("gone");
        after_replace(&dir, &mut state, "a.txt");
        std::fs::remove_file(dir.join("a.txt")).expect("删除失败");

        let outcome = execute(&args("a.txt"), &dir, &mut state).expect("执行失败");

        assert!(outcome.is_error);
        assert!(outcome.text.contains("the file no longer exists"));
        assert!(state.undo_record("a.txt").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restores_crlf_and_bom_on_the_way_back() {
        let (dir, mut state) = temp_state("crlf");
        std::fs::write(dir.join("a.txt"), "\u{feff}ONE\r\ntwo\r\n").expect("写入失败");
        let before = "one\ntwo\n";
        let hashes = line_hashes_pure(before).expect("分配失败");
        state.set_undo(
            "a.txt",
            UndoRecord {
                content: before.to_owned(),
                bom: "\u{feff}".to_owned(),
                ending: "\r\n".to_owned(),
                hashes,
                result_content: "ONE\ntwo\n".to_owned(),
            },
        );

        execute(&args("a.txt"), &dir, &mut state).expect("回滚失败");

        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            "\u{feff}one\r\ntwo\r\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
