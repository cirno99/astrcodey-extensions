//! 构建/编译输出过滤。
//!
//! 上游 `techniques/build.ts` 的移植：命中构建命令时，只保留错误块与警告，其余行丢弃。
//!
//! 错误块的分组表（每个错误一个行下标小表）是调用内部的临时量，整段借用
//! `common::arena` 的线程本地竞技场：行本身直接借用输出切片，不复制成 `String`。

use std::sync::LazyLock;

use astrcode_ext_common::arena::with_scratch;
use bumpalo::Bump;
use bumpalo::collections::Vec as ArenaVec;
use regex::Regex;

use super::command::matches_command_patterns;

/// 视为构建命令的第一段命令。
static BUILD_COMMAND_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^cargo\s+(build|check)\b",
        r"^bun\s+build\b",
        r"^npm\s+run\s+build\b",
        r"^yarn\s+build\b",
        r"^pnpm\s+build\b",
        r"^(?:npx\s+)?tsc\b",
        r"^make\b",
        r"^cmake\b",
        r"^gradle\b",
        r"^mvn\b",
        r"^go\s+(build|install)\b",
        r"^python\s+setup\.py\s+build\b",
        r"^pip\s+install\b",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("build command pattern is valid"))
    .collect()
});

/// 进度噪音行，直接丢弃。
static SKIP_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^\s*Compiling\s+",
        r"^\s*Checking\s+",
        r"^\s*Downloading\s+",
        r"^\s*Downloaded\s+",
        r"^\s*Fetching\s+",
        r"^\s*Fetched\s+",
        r"^\s*Updating\s+",
        r"^\s*Updated\s+",
        r"^\s*Building\s+",
        r"^\s*Generated\s+",
        r"^\s*Creating\s+",
        r"^\s*Running\s+",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("skip pattern is valid"))
    .collect()
});

static ERROR_START_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"^error\[", r"^error:", r"^\[ERROR\]", r"^FAIL"]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("error pattern is valid"))
        .collect()
});

static WARNING_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"^warning:", r"^\[WARNING\]", r"^warn:"]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("warning pattern is valid"))
        .collect()
});

/// 编译单元计数行，例如 `   Compiling foo v0.1.0`。
static COMPILE_UNIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(Compiling|Checking|Building)\s+").expect("compile unit pattern is valid")
});

/// 错误块的续行：缩进行或以 `-->` 开头。
static ERROR_CONTINUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s|^-->").expect("continuation pattern is valid"));

fn any_match(patterns: &[Regex], line: &str) -> bool {
    patterns.iter().any(|pattern| pattern.is_match(line))
}

/// 是否是构建命令。
pub fn is_build_command(command: Option<&str>) -> bool {
    matches_command_patterns(command, &BUILD_COMMAND_PATTERNS)
}

/// 过滤构建输出。不是构建命令时返回 `None`，表示「本技术不适用」。
pub fn filter_build_output(output: &str, command: Option<&str>) -> Option<String> {
    if !is_build_command(command) {
        return None;
    }
    with_scratch(|bump| filter_build_output_in(bump, output))
}

fn filter_build_output_in(bump: &Bump, output: &str) -> Option<String> {
    let mut compiled = 0usize;
    let mut errors: ArenaVec<'_, ArenaVec<'_, &str>> = ArenaVec::new_in(bump);
    let mut warnings: ArenaVec<'_, &str> = ArenaVec::new_in(bump);

    let mut in_error_block = false;
    let mut current_error: ArenaVec<'_, &str> = ArenaVec::new_in(bump);
    let mut blank_count = 0usize;

    for line in output.split('\n') {
        if COMPILE_UNIT.is_match(line) {
            compiled += 1;
            continue;
        }

        if any_match(&SKIP_PATTERNS, line) {
            continue;
        }

        if any_match(&ERROR_START_PATTERNS, line) {
            if in_error_block && !current_error.is_empty() {
                errors.push(std::mem::replace(&mut current_error, ArenaVec::new_in(bump)));
            }
            in_error_block = true;
            current_error = {
                let mut block = ArenaVec::new_in(bump);
                block.push(line);
                block
            };
            blank_count = 0;
            continue;
        }

        if any_match(&WARNING_PATTERNS, line) {
            warnings.push(line);
            continue;
        }

        if !in_error_block {
            continue;
        }

        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count >= 2 && current_error.len() > 3 {
                errors.push(std::mem::replace(&mut current_error, ArenaVec::new_in(bump)));
                in_error_block = false;
            } else {
                current_error.push(line);
            }
            continue;
        }

        if ERROR_CONTINUATION.is_match(line) {
            current_error.push(line);
            blank_count = 0;
            continue;
        }

        errors.push(std::mem::replace(&mut current_error, ArenaVec::new_in(bump)));
        in_error_block = false;
    }

    if in_error_block && !current_error.is_empty() {
        errors.push(current_error);
    }

    if errors.is_empty() && warnings.is_empty() {
        return Some(format!("[OK] Build successful ({compiled} units compiled)"));
    }

    let mut result: Vec<String> = Vec::new();

    if !errors.is_empty() {
        result.push(format!("[ERROR] {} error(s):", errors.len()));
        for error in errors.iter().take(5) {
            result.extend(error.iter().take(10).map(|line| (*line).to_owned()));
            if error.len() > 10 {
                result.push("  ...".to_owned());
            }
        }
        if errors.len() > 5 {
            result.push(format!("... and {} more errors", errors.len() - 5));
        }
    }

    if !warnings.is_empty() {
        result.push(format!("\n[WARN] {} warning(s)", warnings.len()));
    }

    Some(result.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_build_commands_are_handled() {
        assert!(is_build_command(Some("cargo build")));
        assert!(is_build_command(Some("cargo check --all")));
        assert!(is_build_command(Some("npm run build")));
        assert!(is_build_command(Some("make")));
        assert!(is_build_command(Some("pip install foo")));
        assert!(!is_build_command(Some("cargo test")));
        assert!(!is_build_command(Some("ls")));
        assert!(!is_build_command(None));
        assert_eq!(filter_build_output("anything", Some("ls")), None);
    }

    #[test]
    fn a_clean_build_collapses_to_a_success_line() {
        let output = "   Compiling foo v0.1.0\n   Compiling bar v0.2.0\n    Finished dev [unoptimized]";
        assert_eq!(
            filter_build_output(output, Some("cargo build")).unwrap(),
            "[OK] Build successful (2 units compiled)"
        );
    }

    #[test]
    fn errors_are_collected_with_their_continuation_lines() {
        let output = "\
   Compiling foo v0.1.0
error[E0308]: mismatched types
 --> src/main.rs:3:5
  |
3 |     let x: u8 = \"a\";
  |            ^^ expected u8

error: aborting due to previous error
";
        let filtered = filter_build_output(output, Some("cargo build")).unwrap();
        assert!(filtered.starts_with("[ERROR] 2 error(s):"));
        assert!(filtered.contains("error[E0308]: mismatched types"));
        assert!(filtered.contains("--> src/main.rs:3:5"));
        assert!(!filtered.contains("Compiling foo"));
    }

    #[test]
    fn warnings_are_counted_not_listed() {
        let output = "warning: unused variable\n   Compiling x\n";
        let filtered = filter_build_output(output, Some("cargo build")).unwrap();
        assert_eq!(filtered, "\n[WARN] 1 warning(s)");
    }

    #[test]
    fn more_than_five_errors_are_summarized() {
        let mut output = String::new();
        for index in 0..7 {
            output.push_str(&format!("error: boom {index}\n"));
        }
        let filtered = filter_build_output(&output, Some("cargo build")).unwrap();
        assert!(filtered.contains("[ERROR] 7 error(s):"));
        assert!(filtered.contains("... and 2 more errors"));
    }
}
