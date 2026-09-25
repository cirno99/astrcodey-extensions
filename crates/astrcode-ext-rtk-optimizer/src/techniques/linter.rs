//! 静态检查（linter）输出聚合。
//!
//! 上游 `techniques/linter.ts` 的移植：解析 `文件:行:列: 内容` 与 Rust 的
//! `error|warning: 内容 at 文件:行:列` 两种形态，按规则与文件做 Top-N 汇总。

use std::sync::LazyLock;

use regex::Regex;

use super::{
    command::{matches_command_patterns, normalize_command_for_detection},
    path::compact_path,
};

static LINTER_COMMAND_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^(?:pnpm\s+)?(?:npx\s+)?eslint\b",
        r"^(?:npx\s+)?prettier\b",
        r"^ruff\b",
        r"^pylint\b",
        r"^mypy\b",
        r"^flake8\b",
        r"^black\b",
        r"^cargo\s+clippy\b",
        r"^golangci-lint\b",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("linter command pattern is valid"))
    .collect()
});

static FILE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+):([0-9]+):([0-9]+):\s*(.+)$").expect("file line pattern is valid")
});
static RUST_DIAGNOSTIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(error|warning):\s*(.+?)\s+at\s+(.+):([0-9]+):([0-9]+)$")
        .expect("rust diagnostic pattern is valid")
});
/// 规则名：行尾的最后一个 `[...]`。
static RULE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(.+?)\]$").expect("rule suffix is valid"));
static WARNING_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)warning").expect("warning word is valid"));

static ESLINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)eslint\b").expect("eslint pattern is valid"));

/// 命令片段 → 展示名。按顺序取第一个命中的。
static LINTER_NAMES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"^ruff\b", "Ruff"),
        (r"^pylint\b", "Pylint"),
        (r"^mypy\b", "MyPy"),
        (r"^flake8\b", "Flake8"),
        (r"clippy\b", "Clippy"),
        (r"^golangci-lint\b", "GolangCI-Lint"),
        (r"prettier\b", "Prettier"),
    ]
    .iter()
    .map(|(pattern, name)| {
        (
            Regex::new(pattern).expect("linter name pattern is valid"),
            *name,
        )
    })
    .collect()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
struct Issue {
    severity: Severity,
    rule: String,
    file: String,
}

/// 是否是 linter 命令。
pub fn is_linter_command(command: Option<&str>) -> bool {
    matches_command_patterns(command, &LINTER_COMMAND_PATTERNS)
}

fn parse_line(line: &str) -> Option<Issue> {
    if let Some(captures) = FILE_LINE.captures(line) {
        let file = captures
            .get(1)
            .map_or_else(|| "unknown".to_owned(), |value| value.as_str().to_owned());
        let content = captures.get(4).map_or(line, |value| value.as_str());
        let severity = if WARNING_WORD.is_match(content) {
            Severity::Warning
        } else {
            Severity::Error
        };
        let rule = RULE_SUFFIX
            .captures(content)
            .and_then(|captures| captures.get(1))
            .map_or_else(|| "unknown".to_owned(), |value| value.as_str().to_owned());
        return Some(Issue {
            severity,
            rule,
            file,
        });
    }

    if let Some(captures) = RUST_DIAGNOSTIC.captures(line) {
        let severity = match captures.get(1).map(|value| value.as_str()) {
            Some("warning") => Severity::Warning,
            _ => Severity::Error,
        };
        let file = captures
            .get(3)
            .map_or_else(|| "unknown".to_owned(), |value| value.as_str().to_owned());
        return Some(Issue {
            severity,
            rule: "unknown".to_owned(),
            file,
        });
    }

    None
}

fn detect_linter_type(command: Option<&str>) -> &'static str {
    let Some(normalized) = normalize_command_for_detection(command) else {
        return "Linter";
    };
    if ESLINT.is_match(&normalized) {
        return "ESLint";
    }
    for (pattern, name) in LINTER_NAMES.iter() {
        if pattern.is_match(&normalized) {
            return name;
        }
    }
    "Linter"
}

/// 统计计数器：保留首次出现顺序，便于与上游的稳定排序对齐。
fn bump(counter: &mut Vec<(String, usize)>, key: &str) {
    match counter.iter_mut().find(|(name, _)| name == key) {
        Some((_, count)) => *count += 1,
        None => counter.push((key.to_owned(), 1)),
    }
}

/// 按计数降序取前 `limit` 项；计数相同时保持首次出现顺序（与 JS 的稳定排序一致）。
fn top_n(counter: &[(String, usize)], limit: usize) -> Vec<(String, usize)> {
    let mut sorted = counter.to_vec();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    sorted.truncate(limit);
    sorted
}

/// 聚合 linter 输出。不是 linter 命令时返回 `None`。
pub fn aggregate_linter_output(output: &str, command: Option<&str>) -> Option<String> {
    if !is_linter_command(command) {
        return None;
    }

    let linter_type = detect_linter_type(command);
    let issues: Vec<Issue> = output.split('\n').filter_map(parse_line).collect();

    if issues.is_empty() {
        return Some(format!("[OK] {linter_type}: No issues found"));
    }

    let errors = issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .count();
    let warnings = issues.len() - errors;

    let mut by_rule: Vec<(String, usize)> = Vec::new();
    let mut by_file: Vec<(String, Vec<&Issue>)> = Vec::new();
    for issue in &issues {
        bump(&mut by_rule, &issue.rule);
        match by_file.iter_mut().find(|(file, _)| file == &issue.file) {
            Some((_, entry)) => entry.push(issue),
            None => by_file.push((issue.file.clone(), vec![issue])),
        }
    }

    let mut result = format!(
        "{linter_type}: {errors} errors, {warnings} warnings in {} files\n",
        by_file.len()
    );
    result.push_str("═══════════════════════════════════════\n");

    result.push_str("Top rules:\n");
    for (rule, count) in top_n(&by_rule, 10) {
        result.push_str(&format!("  {rule} ({count}x)\n"));
    }

    result.push_str("\nTop files:\n");
    let mut files = by_file;
    files.sort_by_key(|entry| std::cmp::Reverse(entry.1.len()));
    files.truncate(10);

    for (file, file_issues) in files {
        result.push_str(&format!(
            "  {} ({} issues)\n",
            compact_path(&file, 40),
            file_issues.len()
        ));
        let mut file_rules: Vec<(String, usize)> = Vec::new();
        for issue in &file_issues {
            bump(&mut file_rules, &issue.rule);
        }
        for (rule, count) in top_n(&file_rules, 3) {
            result.push_str(&format!("    {rule} ({count})\n"));
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_linter_commands_are_handled() {
        assert!(is_linter_command(Some("cargo clippy")));
        assert!(is_linter_command(Some("npx eslint .")));
        assert!(is_linter_command(Some("ruff check")));
        assert!(!is_linter_command(Some("cargo build")));
        assert_eq!(aggregate_linter_output("x", Some("ls")), None);
    }

    #[test]
    fn a_clean_run_reports_the_linter_name() {
        assert_eq!(
            aggregate_linter_output("", Some("cargo clippy")).unwrap(),
            "[OK] Clippy: No issues found"
        );
        assert_eq!(
            aggregate_linter_output("nothing parseable", Some("npx eslint .")).unwrap(),
            "[OK] ESLint: No issues found"
        );
    }

    #[test]
    fn detects_the_linter_type_from_the_command() {
        assert_eq!(detect_linter_type(Some("npx eslint src")), "ESLint");
        assert_eq!(detect_linter_type(Some("ruff check")), "Ruff");
        assert_eq!(detect_linter_type(Some("cargo clippy --all")), "Clippy");
        assert_eq!(detect_linter_type(Some("golangci-lint run")), "GolangCI-Lint");
        assert_eq!(detect_linter_type(Some("prettier --check .")), "Prettier");
        assert_eq!(detect_linter_type(None), "Linter");
    }

    #[test]
    fn aggregates_eslint_style_issues_by_rule_and_file() {
        let output = "\
src/a.ts:1:1: Unexpected var, use let or const instead [no-var]
src/a.ts:2:5: Missing semicolon [semi]
src/b.ts:9:1: Unexpected any [no-explicit-any]
src/b.ts:9:2: Unexpected any [no-explicit-any]
";
        let aggregated = aggregate_linter_output(output, Some("npx eslint .")).unwrap();
        assert!(aggregated.contains("ESLint: 4 errors, 0 warnings in 2 files"));
        assert!(aggregated.contains("no-explicit-any (2x)"));
        assert!(aggregated.contains("no-var (1x)"));
        assert!(aggregated.contains("src/b.ts (2 issues)"));
    }

    #[test]
    fn classifies_warning_lines_by_keyword() {
        let output = "\
src/a.ts:1:1: this is a warning about something [w]
src/b.ts:1:1: this is an error [e]
";
        let aggregated = aggregate_linter_output(output, Some("npx eslint .")).unwrap();
        assert!(aggregated.contains("1 errors, 1 warnings"));
    }

    #[test]
    fn parses_rust_style_diagnostics() {
        let output = "\
error: unused variable `x` at src/main.rs:3:9
warning: unused import at src/lib.rs:1:5
";
        let aggregated = aggregate_linter_output(output, Some("cargo clippy")).unwrap();
        assert!(aggregated.contains("Clippy: 1 errors, 1 warnings in 2 files"));
        assert!(aggregated.contains("unknown (2x)"));
    }
}
