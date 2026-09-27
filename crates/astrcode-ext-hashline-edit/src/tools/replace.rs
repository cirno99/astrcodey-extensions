//! `replace`：用两个裸锚点圈定行范围并替换。
//!
//! 执行顺序与原版逐条对齐，顺序本身就是契约：
//!
//! 1. 解析参数（含三项自动纠正），先于读文件——参数写错时不必碰磁盘。
//! 2. 读文件、算/取锚点、取 served 集合。
//! 3. `apply_edit`：锚点校验 → served 守卫 → 换算字节区间 → 拼新内容。
//! 4. **先持久化 undo 记录，再写文件**；写失败就回滚 undo 记录。这样崩溃窗口里
//!    最多丢掉一次撤销点，不会出现「文件已改但没有还原点」。
//! 5. 稳定映射出新锚点、生成 diff、把 diff 里展示过的锚点记进 served。

use std::{path::Path, sync::Arc};

use rustc_hash::FxHashSet;
use serde::Deserialize;

use crate::{
    fsops,
    hashline::{
        apply::apply_edit,
        diff::gen_diff,
        error::{EditError, ErrorCode},
        hash::{Anchor, HASH_SPACE, map_stable_hashes},
        lines::{clip_line, restore_endings, split_lines},
        request::{EditRequest, RawEdit, res_edit},
    },
    state::{SessionState, UndoRecord},
    tools::{DIFF_CONTEXT_LINES, ToolOutcome, render_diff},
};

/// 锚点容量，也就是行数上限。
const MAX_HASH_LINES: usize = HASH_SPACE;

#[derive(Debug, Deserialize)]
pub struct ReplaceArgs {
    pub path: String,
    pub remove_from: String,
    pub remove_to: String,
    /// 故意用 `Value`：模型传数组时要回一句可操作的 `E_BAD_SHAPE` 指引。
    pub replacement_text: serde_json::Value,
}

pub fn execute(
    args: &ReplaceArgs,
    working_dir: &Path,
    state: &mut SessionState,
) -> Result<ToolOutcome, EditError> {
    let mut warnings: Vec<String> = Vec::new();

    let raw_edit = RawEdit {
        remove_from: args.remove_from.clone(),
        remove_to: args.remove_to.clone(),
        replacement_text: args.replacement_text.clone(),
    };
    let edit = res_edit(&raw_edit, &mut warnings)?;

    let target = fsops::resolve_path(working_dir, &args.path);
    let display_path = args.path.clone();
    let file = fsops::read_raw_text(&target, &display_path)?;
    let original = file.normalized.clone();
    if split_lines(&original).len() > MAX_HASH_LINES {
        return Err(too_many_lines(&display_path));
    }

    let key = display_path.clone();
    let original_hashes = state.hashes_for(&key, &original)?;
    let served = state.served_set(&key).cloned();

    let applied = match apply_edit(
        &original,
        &edit,
        Some(&original_hashes),
        Some(&display_path),
        served.as_ref(),
    ) {
        Ok(applied) => applied,
        Err(error) => {
            // 错误正文里已经展示了当前锚点，补记进 served：模型下一轮可以直接用。
            if !error.feedback_hashes().is_empty() {
                state.record_served(&key, error.feedback_hashes());
            }
            let _ = state.save();
            return Err(error);
        },
    };
    warnings.extend(applied.warnings.iter().cloned());

    let result = applied.content;
    if result == original {
        let _ = state.save();
        let text = match &applied.noop {
            Some(noop) => format!(
                "No changes made to {}\nClassification: noop\nReplacement for {} is identical to \
                 current content:\n  {}: {}",
                args.path,
                noop.loc,
                noop.loc,
                clip_line(&noop.current_content)
            ),
            None => format!(
                "No changes made to {}\nClassification: noop\n\nThe edit produced identical \
                 content.",
                args.path
            ),
        };
        return Ok(ToolOutcome::ok(text));
    }

    let (total_added, total_removed) = count_line_changes(
        &edit,
        &original_hashes,
        false,
        applied.auto_fixes.len(),
    );

    let removed_hashes = collect_removed_hashes(&edit, &original_hashes);
    let previous_undo = state.set_undo(
        &key,
        UndoRecord {
            content: original.clone(),
            bom: file.bom.to_owned(),
            ending: file.ending.as_str().to_owned(),
            hashes: Arc::clone(&original_hashes),
            result_content: result.clone(),
        },
    );
    if state.save().is_err() {
        warnings.push(
            "[E_UNDO_UNAVAILABLE] Could not persist undo history; this edit will NOT be undoable \
             after a restart."
                .to_owned(),
        );
    }

    let payload = format!("{}{}", file.bom, restore_endings(&result, file.ending));
    if let Err(error) = fsops::write_text(&target, &payload) {
        match previous_undo {
            Some(previous) => {
                state.set_undo(&key, previous);
            },
            None => {
                state.clear_undo(&key);
            },
        }
        let _ = state.save();
        return Err(EditError::plain(format!("replace failed: {error}")));
    }

    let result_hashes =
        Arc::new(map_stable_hashes(&original, &original_hashes, &result, &removed_hashes)?);
    state.put_snapshot(&key, &result, Arc::clone(&result_hashes));
    let diff = gen_diff(
        &original,
        &result,
        DIFF_CONTEXT_LINES,
        Some(&result_hashes),
        Some(&original_hashes),
    )
    .0;
    state.record_served_from_diff(&key, &diff);
    let _ = state.save();

    let line_summary = if total_added > 0 || total_removed > 0 {
        format!(" Added {total_added} line(s), removed {total_removed} line(s).")
    } else {
        String::new()
    };
    Ok(ToolOutcome::ok(format!(
        "Successfully replaced in {}.{line_summary}{}{}",
        args.path,
        render_warnings(&warnings),
        render_diff(&diff)
    )))
}

/// 被替换范围内的全部锚点。用于稳定映射时判断哪些锚点「腾出来了」。
fn collect_removed_hashes(edit: &EditRequest, original_hashes: &[Anchor]) -> FxHashSet<Anchor> {
    let mut removed = FxHashSet::default();
    let start = index_of(original_hashes, &edit.hash_bounds[0].hash);
    let end = index_of(original_hashes, &edit.hash_bounds[1].hash);
    if let (Some(start), Some(end)) = (start, end) {
        let (first, last) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        for hash in original_hashes.iter().take(last + 1).skip(first) {
            removed.insert(hash.clone());
        }
    }
    removed
}

/// 增删行数统计。自动纠正删掉的行不计入新增。
fn count_line_changes(
    edit: &EditRequest,
    original_hashes: &[Anchor],
    is_noop: bool,
    auto_fix_count: usize,
) -> (usize, usize) {
    if is_noop {
        return (0, 0);
    }
    let total_removed = match (
        index_of(original_hashes, &edit.hash_bounds[0].hash),
        index_of(original_hashes, &edit.hash_bounds[1].hash),
    ) {
        (Some(start), Some(end)) => start.abs_diff(end) + 1,
        _ => 0,
    };
    let total_added = edit.content_lines.len().saturating_sub(auto_fix_count);
    (total_added, total_removed)
}

fn index_of(hashes: &[Anchor], hash: &str) -> Option<usize> {
    hashes.iter().position(|candidate| candidate.as_str() == hash)
}

fn render_warnings(warnings: &[String]) -> String {
    if warnings.is_empty() {
        String::new()
    } else {
        format!("\n\nWarnings:\n{}", warnings.join("\n"))
    }
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
    use crate::hashline::{HASH_SEP, hash::line_hashes_pure};
    fn temp_state(name: &str) -> (std::path::PathBuf, SessionState) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-hashline-replace-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let state = SessionState::load(dir.join("state.json"));
        (dir, state)
    }

    /// 建立「模型已经读过这个文件」的前置状态。
    fn serve(dir: &Path, state: &mut SessionState, path: &str, content: &str) -> Arc<Vec<Anchor>> {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("创建父目录失败");
        }
        std::fs::write(&target, content).expect("写入失败");
        let hashes = line_hashes_pure(content).expect("分配失败");
        state.put_snapshot(path, content, Arc::new(hashes.clone()));
        state.record_served(path, &hashes);
        Arc::new(hashes)
    }

    fn args(
        path: &str,
        from: impl AsRef<str>,
        to: impl AsRef<str>,
        text: serde_json::Value,
    ) -> ReplaceArgs {
        ReplaceArgs {
            path: path.to_owned(),
            remove_from: from.as_ref().to_owned(),
            remove_to: to.as_ref().to_owned(),
            replacement_text: text,
        }
    }

    #[test]
    fn replaces_a_served_range_and_returns_the_diff() {
        let (dir, mut state) = temp_state("basic");
        let content = "one\ntwo\nthree\n";
        let hashes = serve(&dir, &mut state, "a.txt", content);

        let outcome = execute(
            &args("a.txt", &hashes[1], &hashes[1], serde_json::json!("TWO")),
            &dir,
            &mut state,
        )
        .expect("替换失败");

        assert!(!outcome.is_error);
        assert!(outcome.text.starts_with("Successfully replaced in a.txt."));
        assert!(outcome.text.contains("Added 1 line(s), removed 1 line(s)."));
        // 被删的行用旧锚点、未被触碰的上下文行保持原锚点
        assert!(outcome.text.contains(&format!("-{}{HASH_SEP}two", hashes[1])));
        assert!(outcome.text.contains(&format!(" {}{HASH_SEP}one", hashes[0])));
        assert!(outcome.text.contains(&format!(" {}{HASH_SEP}three", hashes[2])));
        // `+` 行带的是**新**锚点：内容变了就该换地址，否则模型会拿旧地址去改新行
        assert!(!outcome.text.contains(&format!("+{}{HASH_SEP}TWO", hashes[1])));
        let new_hashes = state
            .hashes_for("a.txt", "one\nTWO\nthree\n")
            .expect("取新锚点失败");
        assert!(outcome.text.contains(&format!("+{}{HASH_SEP}TWO", new_hashes[1])));
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            "one\nTWO\nthree\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preserves_crlf_and_bom_when_writing() {
        let (dir, mut state) = temp_state("crlf");
        let content = "one\r\ntwo\r\n";
        let raw = format!("\u{feff}{content}");
        std::fs::write(dir.join("a.txt"), &raw).expect("写入失败");
        let normalized = "one\ntwo\n";
        let hashes = line_hashes_pure(normalized).expect("分配失败");
        state.put_snapshot("a.txt", normalized, Arc::new(hashes.clone()));
        state.record_served("a.txt", &hashes);

        execute(
            &args("a.txt", &hashes[0], &hashes[0], serde_json::json!("ONE")),
            &dir,
            &mut state,
        )
        .expect("替换失败");

        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            "\u{feff}ONE\r\ntwo\r\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_an_unserved_range() {
        let (dir, mut state) = temp_state("unserved");
        let content = "one\ntwo\nthree\n";
        std::fs::write(dir.join("a.txt"), content).expect("写入失败");
        let hashes = line_hashes_pure(content).expect("分配失败");
        state.put_snapshot("a.txt", content, Arc::new(hashes.clone()));
        state.record_served("a.txt", &[hashes[0].clone()]);

        let error = execute(
            &args("a.txt", &hashes[1], &hashes[1], serde_json::json!("TWO")),
            &dir,
            &mut state,
        )
        .expect_err("应当被 served 守卫拦下");

        assert_eq!(error.code(), ErrorCode::RangeStale);
        // 错误反馈里的锚点被补记进 served
        assert!(state.served_set("a.txt").expect("应当有记录").contains(&hashes[1]));
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_noop_edit_reports_that_nothing_changed() {
        let (dir, mut state) = temp_state("noop");
        let content = "one\ntwo\n";
        let hashes = serve(&dir, &mut state, "a.txt", content);

        let outcome = execute(
            &args("a.txt", &hashes[1], &hashes[1], serde_json::json!("two")),
            &dir,
            &mut state,
        )
        .expect("替换失败");

        assert!(outcome.text.contains("Classification: noop"));
        assert!(outcome.text.contains("is identical to current content"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chained_edits_work_without_re_reading() {
        let (dir, mut state) = temp_state("chained");
        let content = "line1\nline2\nline3\n";
        let hashes = serve(&dir, &mut state, "a.txt", content);

        let first = execute(
            &args("a.txt", &hashes[1], &hashes[1], serde_json::json!("line2-edited")),
            &dir,
            &mut state,
        )
        .expect("第一次替换失败");

        // diff 里 `+` 行的锚点就是新地址，模型可以直接链式使用
        let third_hash = hashes[2].clone();
        assert!(state.served_set("a.txt").expect("应当有记录").contains(&third_hash));

        execute(
            &args("a.txt", &third_hash, &third_hash, serde_json::json!("line3-edited")),
            &dir,
            &mut state,
        )
        .expect("第二次替换失败");

        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            "line1\nline2-edited\nline3-edited\n"
        );
        assert!(first.text.contains("line2-edited"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_anchor_is_rejected_and_leaves_the_file_alone() {
        let (dir, mut state) = temp_state("stale");
        let content = "one\ntwo\n";
        let hashes = serve(&dir, &mut state, "a.txt", content);

        let error = execute(
            &args("a.txt", "ZZZ", "ZZZ", serde_json::json!("x")),
            &dir,
            &mut state,
        )
        .expect_err("应当报过期锚点");

        assert_eq!(error.code(), ErrorCode::StaleAnchor);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).expect("读取失败"),
            content
        );
        // 锚点仍然有效，重试即可
        execute(
            &args("a.txt", &hashes[0], &hashes[0], serde_json::json!("ONE")),
            &dir,
            &mut state,
        )
        .expect("重试失败");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relative_path_resolves_against_the_working_dir() {
        let (dir, mut state) = temp_state("relative");
        let hashes = serve(&dir, &mut state, "nested/a.txt", "one\ntwo\n");

        let mut request = args("nested/a.txt", &hashes[0], &hashes[0], serde_json::json!("ONE"));
        request.path = "nested/a.txt".to_owned();
        execute(&request, &dir, &mut state).expect("替换失败");

        assert_eq!(
            std::fs::read_to_string(dir.join("nested/a.txt")).expect("读取失败"),
            "ONE\ntwo\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_history_is_persisted_before_the_write() {
        let (dir, mut state) = temp_state("undo-persist");
        let content = "one\ntwo\n";
        let hashes = serve(&dir, &mut state, "a.txt", content);

        execute(
            &args("a.txt", &hashes[0], &hashes[0], serde_json::json!("ONE")),
            &dir,
            &mut state,
        )
        .expect("替换失败");

        let record = state.undo_record("a.txt").expect("应当留下还原点");
        assert_eq!(record.content, content);
        assert_eq!(record.result_content, "ONE\ntwo\n");
        assert_eq!(record.hashes.as_slice(), hashes.as_slice());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_failure_rolls_the_undo_record_back() {
        let (dir, mut state) = temp_state("write-failure");
        let content = "one\ntwo\n";
        let hashes = serve(&dir, &mut state, "a.txt", content);

        // 把父目录改成只读：读仍然成功，原子写的临时文件创建会失败。
        // 以 root 运行时权限位不起作用，这种情况下跳过（否则会得到一个假阳性）。
        //
        // 还原时写回**原权限**而不是 `set_readonly(false)`：后者在 Unix 上会把目录
        // 变成全局可写（clippy 的 permissions_set_readonly_false）。
        let original = std::fs::metadata(&dir).expect("取元数据失败").permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        std::fs::set_permissions(&dir, readonly).expect("改权限失败");

        if std::fs::write(dir.join(".probe"), "x").is_ok() {
            let _ = std::fs::remove_file(dir.join(".probe"));
            let _ = std::fs::set_permissions(&dir, original);
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let error = execute(
            &args("a.txt", &hashes[0], &hashes[0], serde_json::json!("ONE")),
            &dir,
            &mut state,
        )
        .expect_err("应当报写入失败");

        assert!(
            error.render().starts_with("replace failed:"),
            "实际错误：{}",
            error.render()
        );
        // 写入失败时不能留下一个指向未发生编辑的还原点
        assert!(state.undo_record("a.txt").is_none());

        let _ = std::fs::set_permissions(&dir, original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_corrections_surface_as_warnings_in_the_result() {
        let (dir, mut state) = temp_state("autofix");
        let content = "one\ntwo\n";
        let hashes = serve(&dir, &mut state, "a.txt", content);

        let outcome = execute(
            &args(
                "a.txt",
                &format!("{}{HASH_SEP}one", hashes[0]),
                &hashes[0],
                serde_json::json!("ONE"),
            ),
            &dir,
            &mut state,
        )
        .expect("替换失败");

        assert!(outcome.text.contains("Warnings:"));
        assert!(outcome.text.contains("[E_BAD_REF] Autocorrected"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_array_replacement_text_is_rejected_with_guidance() {
        let (dir, mut state) = temp_state("array");
        let hashes = serve(&dir, &mut state, "a.txt", "one\n");

        let error = execute(
            &args("a.txt", &hashes[0], &hashes[0], serde_json::json!(["x"])),
            &dir,
            &mut state,
        )
        .expect_err("应当拒绝数组");

        assert_eq!(error.code(), ErrorCode::BadShape);
        assert!(error.render().contains("not an array"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_line_changes_subtracts_auto_removed_lines() {
        let content = "a\nb\nc\n";
        let hashes = line_hashes_pure(content).expect("分配失败");
        let edit = EditRequest {
            content_lines: vec!["x".into(), "y".into(), "z".into()],
            hash_bounds: [
                crate::hashline::HashRef { hash: hashes[0].to_string() },
                crate::hashline::HashRef { hash: hashes[0].to_string() },
            ],
        };
        assert_eq!(count_line_changes(&edit, &hashes, false, 0), (3, 1));
        assert_eq!(count_line_changes(&edit, &hashes, false, 2), (1, 1));
        assert_eq!(count_line_changes(&edit, &hashes, true, 0), (0, 0));
    }

    #[test]
    fn collect_removed_hashes_covers_the_whole_range() {
        let hashes: Vec<Anchor> = ["aB3", "cD4", "eF5", "gH6"]
            .iter()
            .map(|hash| Anchor::new(hash))
            .collect();
        let edit = EditRequest {
            content_lines: vec![],
            hash_bounds: [
                crate::hashline::HashRef { hash: "eF5".into() },
                crate::hashline::HashRef { hash: "cD4".into() },
            ],
        };
        let removed = collect_removed_hashes(&edit, &hashes);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&Anchor::new("cD4")));
        assert!(removed.contains(&Anchor::new("eF5")));
    }
}
