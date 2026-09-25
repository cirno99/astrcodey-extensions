//! git 输出压缩。
//!
//! 上游 `techniques/git.ts` 的移植：`git diff` 压成「文件 + 变更行数 + 少量上下文」，
//! `git status` 压成分类计数，`git log` 截行并收窄超长行。

use std::sync::LazyLock;

use regex::Regex;

use super::command::{matches_command_patterns, normalize_command_for_detection};

static GIT_COMMAND_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"^git\s+(diff|status|log|show|stash)\b"]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("git command pattern is valid"))
        .collect()
});

static RAW_GIT_DIFF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^diff --git ").expect("raw diff pattern is valid"));
static RAW_GIT_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(?:## |(?:M|A|D|R|C|U|\?| )\S)").expect("raw status pattern is valid")
});
static DIFF_FILE_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"diff --git a/(.+) b/(.+)").expect("diff header is valid"));
static HUNK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@@ .+ @@").expect("hunk header is valid"));
static STATUS_BRANCH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"## (.+)").expect("status branch is valid"));

/// 是否是 git 命令。
pub fn is_git_command(command: Option<&str>) -> bool {
    matches_command_patterns(command, &GIT_COMMAND_PATTERNS)
}

/// 压缩 diff：每个文件只留前若干行变更，并汇总增删行数。
pub fn compact_diff(output: &str, max_lines: usize) -> String {
    const MAX_HUNK_LINES: usize = 10;

    let mut result: Vec<String> = Vec::new();
    let mut current_file = String::new();
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut in_hunk = false;
    let mut hunk_lines = 0usize;

    for line in output.split('\n') {
        if result.len() >= max_lines {
            result.push("\n... (more changes truncated)".to_owned());
            break;
        }

        if line.starts_with("diff --git") {
            if !current_file.is_empty() && (added > 0 || removed > 0) {
                result.push(format!("  +{added} -{removed}"));
            }
            current_file = DIFF_FILE_HEADER
                .captures(line)
                .and_then(|captures| captures.get(2))
                .map_or_else(|| "unknown".to_owned(), |value| value.as_str().to_owned());
            result.push(format!("\n> {current_file}"));
            added = 0;
            removed = 0;
            in_hunk = false;
            continue;
        }

        if line.starts_with("@@") {
            in_hunk = true;
            hunk_lines = 0;
            let hunk_info = HUNK_HEADER
                .find(line)
                .map_or_else(|| "@@".to_owned(), |value| value.as_str().to_owned());
            result.push(format!("  {hunk_info}"));
            continue;
        }

        if !in_hunk {
            continue;
        }

        if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
            if hunk_lines < MAX_HUNK_LINES {
                result.push(format!("  {line}"));
                hunk_lines += 1;
            }
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
            if hunk_lines < MAX_HUNK_LINES {
                result.push(format!("  {line}"));
                hunk_lines += 1;
            }
        } else if hunk_lines < MAX_HUNK_LINES && !line.starts_with('\\') && hunk_lines > 0 {
            result.push(format!("  {line}"));
            hunk_lines += 1;
        }

        if hunk_lines == MAX_HUNK_LINES {
            result.push("  ... (truncated)".to_owned());
            hunk_lines += 1;
        }
    }

    if !current_file.is_empty() && (added > 0 || removed > 0) {
        result.push(format!("  +{added} -{removed}"));
    }

    result.join("\n")
}

#[derive(Default)]
struct StatusStats {
    staged: usize,
    modified: usize,
    untracked: usize,
    conflicts: usize,
    staged_files: Vec<String>,
    modified_files: Vec<String>,
    untracked_files: Vec<String>,
}

/// 压缩 `git status --short`：按分类汇总，每类只列前几个文件。
pub fn compact_status(output: &str) -> String {
    let lines: Vec<&str> = output.split('\n').collect();

    if lines.is_empty() || (lines.len() == 1 && lines[0].trim().is_empty()) {
        return "Clean working tree".to_owned();
    }

    let mut stats = StatusStats::default();
    let mut branch_name = String::new();

    for line in &lines {
        if line.starts_with("##") {
            if let Some(captures) = STATUS_BRANCH.captures(line)
                && let Some(value) = captures.get(1)
            {
                let value = value.as_str();
                branch_name = value.split("...").next().unwrap_or(value).to_owned();
            }
            continue;
        }

        if line.chars().count() < 3 {
            continue;
        }

        let characters: Vec<char> = line.chars().collect();
        let index_status = characters[0];
        let worktree_status = characters[1];
        let filename: String = characters[3..].iter().collect();

        if matches!(index_status, 'M' | 'A' | 'D' | 'R' | 'C') {
            stats.staged += 1;
            stats.staged_files.push(filename.clone());
        }

        if index_status == 'U' {
            stats.conflicts += 1;
        }

        if matches!(worktree_status, 'M' | 'D') {
            stats.modified += 1;
            stats.modified_files.push(filename.clone());
        }

        if line.starts_with("??") {
            stats.untracked += 1;
            stats.untracked_files.push(filename);
        }
    }

    let mut result = format!("Branch: {branch_name}\n");

    if stats.staged > 0 {
        result.push_str(&format!("Staged: {} files\n", stats.staged));
        for file in stats.staged_files.iter().take(5) {
            result.push_str(&format!("  {file}\n"));
        }
        if stats.staged > 5 {
            result.push_str(&format!("  ... +{} more\n", stats.staged - 5));
        }
    }

    if stats.modified > 0 {
        result.push_str(&format!("Modified: {} files\n", stats.modified));
        for file in stats.modified_files.iter().take(5) {
            result.push_str(&format!("  {file}\n"));
        }
        if stats.modified > 5 {
            result.push_str(&format!("  ... +{} more\n", stats.modified - 5));
        }
    }

    if stats.untracked > 0 {
        result.push_str(&format!("Untracked: {} files\n", stats.untracked));
        for file in stats.untracked_files.iter().take(3) {
            result.push_str(&format!("  {file}\n"));
        }
        if stats.untracked > 3 {
            result.push_str(&format!("  ... +{} more\n", stats.untracked - 3));
        }
    }

    if stats.conflicts > 0 {
        result.push_str(&format!("Conflicts: {} files\n", stats.conflicts));
    }

    result.trim().to_owned()
}

/// 压缩 `git log`：只留前 `limit` 行，超长行收窄到 80 字符。
pub fn compact_log(output: &str, limit: usize) -> String {
    let lines: Vec<&str> = output.split('\n').collect();
    let mut result: Vec<String> = Vec::new();

    for line in lines.iter().take(limit) {
        if line.chars().count() > 80 {
            let head: String = line.chars().take(77).collect();
            result.push(format!("{head}..."));
        } else {
            result.push((*line).to_owned());
        }
    }

    if lines.len() > limit {
        result.push(format!("... and {} more commits", lines.len() - limit));
    }

    result.join("\n")
}

/// 按 git 子命令分发。不是 git 命令、或输出形态不匹配时返回 `None`。
pub fn compact_git_output(output: &str, command: Option<&str>) -> Option<String> {
    if !is_git_command(command) {
        return None;
    }

    let normalized = normalize_command_for_detection(command)?;

    if normalized.starts_with("git diff") {
        return RAW_GIT_DIFF
            .is_match(output)
            .then(|| compact_diff(output, 50));
    }
    if normalized.starts_with("git status") {
        return RAW_GIT_STATUS
            .is_match(output)
            .then(|| compact_status(output));
    }
    if normalized.starts_with("git log") {
        return Some(compact_log(output, 20));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 line one
-removed line
+added line
+another added
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -10,2 +10,2 @@
-old
+new
";

    #[test]
    fn only_git_commands_are_handled() {
        assert!(is_git_command(Some("git status")));
        assert!(is_git_command(Some("git log --oneline")));
        assert!(!is_git_command(Some("git commit")));
        assert_eq!(compact_git_output("x", Some("ls")), None);
    }

    #[test]
    fn diff_is_grouped_by_file_with_change_counts() {
        let compacted = compact_git_output(DIFF, Some("git diff")).unwrap();
        assert!(compacted.contains("> src/a.rs"), "{compacted}");
        assert!(compacted.contains("  +2 -1"), "{compacted}");
        assert!(compacted.contains("> src/b.rs"), "{compacted}");
        assert!(compacted.contains("  +1 -1"), "{compacted}");
    }

    #[test]
    fn diff_without_the_raw_header_is_not_compacted() {
        assert_eq!(compact_git_output("hello", Some("git diff")), None);
    }

    #[test]
    fn hunk_bodies_are_capped() {
        let mut output = String::from("diff --git a/f b/f\n@@ -1,1 +1,1 @@\n");
        for index in 0..40 {
            output.push_str(&format!("+line {index}\n"));
        }
        let compacted = compact_git_output(&output, Some("git diff")).unwrap();
        assert!(compacted.contains("... (truncated)"));
        assert!(compacted.contains("+40 -0"));
    }

    #[test]
    fn status_is_summarized_by_category() {
        let output = "\
## main...origin/main
M  staged.rs
 M modified.rs
?? new.rs
UU conflict.rs
";
        let compacted = compact_git_output(output, Some("git status")).unwrap();
        assert!(compacted.contains("Branch: main"));
        assert!(compacted.contains("Staged: 1 files"));
        assert!(compacted.contains("Modified: 1 files"));
        assert!(compacted.contains("Untracked: 1 files"));
        assert!(compacted.contains("Conflicts: 1 files"));
    }

    /// `compact_status` 的「干净工作区」分支只在直接调用时可达：
    /// `compact_git_output` 先要求输出匹配 `RAW_GIT_STATUS_PATTERN`，空输出过不了这道门。
    /// 注意 `"\n"` 会被 `split` 成两行，因而不走干净分支。
    #[test]
    fn a_clean_status_is_a_single_line() {
        assert_eq!(compact_status(""), "Clean working tree");
        assert_eq!(compact_status("\n"), "Branch:");
        assert_eq!(compact_git_output("", Some("git status")), None);
    }

    #[test]
    fn status_without_raw_entries_is_not_compacted() {
        assert_eq!(compact_git_output("nothing here", Some("git status")), None);
    }

    #[test]
    fn log_is_limited_and_reports_the_remainder() {
        let lines: Vec<String> = (0..25).map(|index| format!("commit {index}")).collect();
        let compacted = compact_log(&lines.join("\n"), 20);

        assert_eq!(compacted.lines().count(), 21);
        assert!(compacted.starts_with("commit 0\n"));
        assert!(compacted.ends_with("... and 5 more commits"));
    }

    #[test]
    fn long_log_lines_are_clipped_to_80_characters() {
        let compacted = compact_log(&"y".repeat(200), 20);
        assert_eq!(compacted.chars().count(), 80);
        assert!(compacted.ends_with("..."));
    }
}
