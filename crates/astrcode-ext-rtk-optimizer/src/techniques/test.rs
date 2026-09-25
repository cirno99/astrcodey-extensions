//! 测试运行输出聚合。
//!
//! 上游 `techniques/test-output.ts` 的移植：抽取通过/失败/跳过计数，失败时只保留
//! 每个失败块的前几行。
//!
//! # 与上游的一处修正
//!
//! 上游第一条计数模式是 `/test result:\s*(\w+)\.\s*(\d+)\s*passed;\s*(\d+)\s*failed;/`，
//! 组 1 是状态词（`ok` / `FAILED`），组 2、3 才是通过数与失败数；但上游统一按
//! 「组 1 = passed、组 2 = failed、组 3 = skipped」取值，于是
//! `test result: FAILED. 2 passed; 1 failed;` 会被报成「0 passed / 2 failed / 1 skipped」。
//! 上游测试没有覆盖这条模式，因此这个错位一直没被发现。本 crate 按模式各自声明组映射，
//! 把它修正为「2 passed / 1 failed / 0 skipped」。

use std::sync::LazyLock;

use regex::Regex;

use super::command::matches_command_patterns;

static TEST_COMMAND_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^npm\s+test\b",
        r"^pnpm\s+test\b",
        r"^yarn\s+test\b",
        r"^bun\s+test\b",
        r"^cargo\s+test\b",
        r"^go\s+test\b",
        r"^pytest\b",
        r"^python\s+-m\s+pytest\b",
        r"^(?:pnpm\s+)?(?:npx\s+)?vitest\b",
        r"^(?:npx\s+)?jest\b",
        r"^mocha\b",
        r"^ava\b",
        r"^tap\b",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("test command pattern is valid"))
    .collect()
});

/// 一条计数模式：正则，以及 passed / failed / skipped 各自取自哪个捕获组。
///
/// `None` 表示该模式不提供这一项，按 0 处理。
struct StatsPattern {
    regex: Regex,
    passed_group: usize,
    failed_group: Option<usize>,
    skipped_group: Option<usize>,
}

static TEST_RESULT_PATTERNS: LazyLock<Vec<StatsPattern>> = LazyLock::new(|| {
    [
        // `test result: FAILED. 2 passed; 1 failed;` —— 组 1 是状态词，不是计数。
        StatsPattern {
            regex: Regex::new(
                r"test result:\s*[A-Za-z0-9_]+\.\s*([0-9]+)\s*passed;\s*([0-9]+)\s*failed;",
            )
            .expect("test result pattern is valid"),
            passed_group: 1,
            failed_group: Some(2),
            skipped_group: None,
        },
        StatsPattern {
            regex: Regex::new(
                r"([0-9]+)\s*passed(?:,\s*([0-9]+)\s*failed)?(?:,\s*([0-9]+)\s*skipped)?",
            )
            .expect("test result pattern is valid"),
            passed_group: 1,
            failed_group: Some(2),
            skipped_group: Some(3),
        },
        StatsPattern {
            regex: Regex::new(
                r"([0-9]+)\s*pass(?:,\s*([0-9]+)\s*fail)?(?:,\s*([0-9]+)\s*skip)?",
            )
            .expect("test result pattern is valid"),
            passed_group: 1,
            failed_group: Some(2),
            skipped_group: Some(3),
        },
        StatsPattern {
            regex: Regex::new(
                r"tests?:\s*([0-9]+)\s*passed(?:,\s*([0-9]+)\s*failed)?(?:,\s*([0-9]+)\s*skipped)?",
            )
            .expect("test result pattern is valid"),
            passed_group: 1,
            failed_group: Some(2),
            skipped_group: Some(3),
        },
    ]
    .into_iter()
    .collect()
});

static FAILURE_START_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^FAIL\s+",
        r"^FAILED\s+",
        r"^\s*●\s+",
        r"^\s*✕\s+",
        r"test\s+[A-Za-z0-9_]+\s+\.\.\.\s*FAILED",
        r"thread\s+'[A-Za-z0-9_]+'\s+panicked",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("failure pattern is valid"))
    .collect()
});

static FALLBACK_PASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\b(?:ok|PASS)\b|[✓✔])").expect("fallback pass is valid"));
static FALLBACK_FAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\b(?:FAIL|fail)\b|[✗✕])").expect("fallback fail is valid"));

/// 失败块续行：缩进行或以 `-` 开头。
static FAILURE_CONTINUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s|^-").expect("continuation pattern is valid"));

/// 从输出里抽取计数；没有可识别计数时返回 `None`。
fn extract_test_stats(output: &str) -> Option<(usize, usize, usize)> {
    for pattern in TEST_RESULT_PATTERNS.iter() {
        let Some(captures) = pattern.regex.captures(output) else {
            continue;
        };
        let group = |index: Option<usize>| -> usize {
            index
                .and_then(|index| captures.get(index))
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(0)
        };
        return Some((
            group(Some(pattern.passed_group)),
            group(pattern.failed_group),
            group(pattern.skipped_group),
        ));
    }
    None
}

fn is_failure_start(line: &str) -> bool {
    FAILURE_START_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(line))
}

/// 是否是测试命令。
pub fn is_test_command(command: Option<&str>) -> bool {
    matches_command_patterns(command, &TEST_COMMAND_PATTERNS)
}

/// 聚合测试输出。不是测试命令时返回 `None`。
pub fn aggregate_test_output(output: &str, command: Option<&str>) -> Option<String> {
    if !is_test_command(command) {
        return None;
    }

    let lines: Vec<&str> = output.split('\n').collect();
    let (mut passed, mut failed, skipped) = extract_test_stats(output).unwrap_or((0, 0, 0));

    if passed == 0 && failed == 0 {
        for line in &lines {
            if FALLBACK_PASS.is_match(line) {
                passed += 1;
            }
            if FALLBACK_FAIL.is_match(line) {
                failed += 1;
            }
        }
    }

    let mut failures: Vec<String> = Vec::new();

    if failed > 0 {
        let mut in_failure = false;
        let mut current: Vec<String> = Vec::new();
        let mut blank_count = 0usize;

        for line in &lines {
            if is_failure_start(line) {
                if in_failure && !current.is_empty() {
                    failures.push(current.join("\n"));
                }
                in_failure = true;
                current = vec![(*line).to_owned()];
                blank_count = 0;
                continue;
            }

            if !in_failure {
                continue;
            }

            if line.trim().is_empty() {
                blank_count += 1;
                if blank_count >= 2 && current.len() > 3 {
                    failures.push(std::mem::take(&mut current).join("\n"));
                    in_failure = false;
                } else {
                    current.push((*line).to_owned());
                }
                continue;
            }

            if FAILURE_CONTINUATION.is_match(line) {
                current.push((*line).to_owned());
                blank_count = 0;
                continue;
            }

            failures.push(std::mem::take(&mut current).join("\n"));
            in_failure = false;
        }

        if in_failure && !current.is_empty() {
            failures.push(current.join("\n"));
        }
    }

    let mut result: Vec<String> = vec!["Test Results:".to_owned()];
    result.push(format!("   PASS: {passed} passed"));
    if failed > 0 {
        result.push(format!("   FAIL: {failed} failed"));
    }
    if skipped > 0 {
        result.push(format!("   SKIP: {skipped} skipped"));
    }

    if failed > 0 && !failures.is_empty() {
        result.push("\n   Failures:".to_owned());
        for failure in failures.iter().take(5) {
            let failure_lines: Vec<&str> = failure.split('\n').collect();
            let first_line = failure_lines.first().copied().unwrap_or("");
            result.push(format!("   - {}", clip(first_line, 70)));
            for detail in failure_lines.iter().skip(1).take(3) {
                if !detail.trim().is_empty() {
                    result.push(format!("     {}", clip(detail, 65)));
                }
            }
            if failure_lines.len() > 4 {
                result.push(format!("     ... ({} more lines)", failure_lines.len() - 4));
            }
        }
        if failures.len() > 5 {
            result.push(format!("   ... and {} more failures", failures.len() - 5));
        }
    }

    Some(result.join("\n"))
}

/// 截到 `limit` 个字符；超长时以 `...` 收尾。
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() > limit {
        let head: String = text.chars().take(limit).collect();
        format!("{head}...")
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_test_commands_are_handled() {
        assert!(is_test_command(Some("cargo test")));
        assert!(is_test_command(Some("npm test")));
        assert!(is_test_command(Some("python -m pytest -q")));
        assert!(!is_test_command(Some("cargo build")));
        assert!(!is_test_command(None));
        assert_eq!(aggregate_test_output("x", Some("ls")), None);
    }

    /// 这条模式在上游存在分组错位，本 crate 已修正；断言钉住修正后的口径。
    #[test]
    fn parses_the_cargo_summary_line_with_the_corrected_group_mapping() {
        let output = "\
running 3 tests
test a ... ok
test b ... ok
test c ... FAILED

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
";
        let aggregated = aggregate_test_output(output, Some("cargo test")).unwrap();
        assert!(aggregated.contains("PASS: 2 passed"), "{aggregated}");
        assert!(aggregated.contains("FAIL: 1 failed"), "{aggregated}");
        assert!(!aggregated.contains("SKIP:"), "{aggregated}");
        assert!(aggregated.contains("Failures:"));
        assert!(aggregated.contains("- test c ... FAILED"));
    }

    #[test]
    fn parses_the_jest_summary_line() {
        let output = "Tests:       5 passed, 2 failed, 1 skipped\n";
        let aggregated = aggregate_test_output(output, Some("npx jest")).unwrap();
        assert!(aggregated.contains("PASS: 5 passed"));
        assert!(aggregated.contains("FAIL: 2 failed"));
        assert!(aggregated.contains("SKIP: 1 skipped"));
    }

    #[test]
    fn parses_the_bare_passed_line() {
        let output = "12 passed, 3 failed, 1 skipped\n";
        let aggregated = aggregate_test_output(output, Some("npx jest")).unwrap();
        assert!(aggregated.contains("PASS: 12 passed"));
        assert!(aggregated.contains("FAIL: 3 failed"));
        assert!(aggregated.contains("SKIP: 1 skipped"));
    }

    #[test]
    fn falls_back_to_counting_pass_and_fail_markers() {
        let output = "✓ one\n✓ two\n✗ three\n";
        let aggregated = aggregate_test_output(output, Some("bun test")).unwrap();
        assert!(aggregated.contains("PASS: 2 passed"));
        assert!(aggregated.contains("FAIL: 1 failed"));
    }

    #[test]
    fn a_clean_run_has_no_failure_section() {
        let output = "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured\n";
        let aggregated = aggregate_test_output(output, Some("cargo test")).unwrap();
        assert_eq!(aggregated, "Test Results:\n   PASS: 4 passed");
    }

    #[test]
    fn failure_details_are_clipped() {
        let long_line = "x".repeat(200);
        let output = format!("FAIL {long_line}\n\n\n");
        let aggregated = aggregate_test_output(&output, Some("cargo test")).unwrap();
        assert!(aggregated.contains("..."));
        assert!(!aggregated.contains(&long_line));
    }
}
