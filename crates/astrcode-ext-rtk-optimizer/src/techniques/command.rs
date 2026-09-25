//! 从命令字符串里识别工具类别（构建 / 测试 / git / linter）。
//!
//! 上游 `techniques/command-detection.ts` 的移植。识别只看**第一段命令**：先取第一个
//! 非空行，剥掉前置环境变量赋值，再在第一个链式操作符处截断并小写化。

use std::sync::LazyLock;

use regex::Regex;

/// 链式操作符；识别只看它们之前的那一段。
const CHAIN_OPERATORS: [&str; 4] = ["&&", "||", ";", "|"];

/// 前置环境变量赋值：`FOO=bar BAZ="q u x" <命令>`。
static ENV_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|[^\s]+)\s+)*"#)
        .expect("env prefix pattern is valid")
});

/// 取第一个链式操作符之前的部分；没有操作符时返回整串。
fn slice_first_segment(command: &str) -> &str {
    let mut cut: Option<usize> = None;
    for operator in CHAIN_OPERATORS {
        if let Some(index) = command.find(operator) {
            cut = Some(cut.map_or(index, |current| current.min(index)));
        }
    }
    cut.map_or(command, |index| &command[..index])
}

/// 把任意命令归一化成「可用于模式匹配的第一段命令」。
///
/// 空命令、只有环境变量赋值的命令、只有链式操作符的命令都返回 `None`。
pub fn normalize_command_for_detection(command: Option<&str>) -> Option<String> {
    let command = command?;
    let first_line = command
        .split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty())?;

    let without_env = ENV_PREFIX.replace(first_line, "");
    let without_env = without_env.trim();
    if without_env.is_empty() {
        return None;
    }

    let segment = slice_first_segment(without_env).trim().to_lowercase();
    (!segment.is_empty()).then_some(segment)
}

/// 归一化后的命令是否命中任一模式。
pub fn matches_command_patterns(command: Option<&str>, patterns: &[Regex]) -> bool {
    let Some(normalized) = normalize_command_for_detection(command) else {
        return false;
    };
    patterns.iter().any(|pattern| pattern.is_match(&normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns() -> Vec<Regex> {
        vec![
            Regex::new(r"^cargo\s+(build|check)\b").unwrap(),
            Regex::new(r"^git\s+status\b").unwrap(),
        ]
    }

    #[test]
    fn normalizes_the_first_non_empty_line() {
        assert_eq!(
            normalize_command_for_detection(Some("\n\n  Cargo Build  \nmore")),
            Some("cargo build".to_owned())
        );
    }

    #[test]
    fn strips_leading_environment_assignments() {
        assert_eq!(
            normalize_command_for_detection(Some(r#"FOO=1 BAR="a b" cargo build"#)),
            Some("cargo build".to_owned())
        );
        assert_eq!(
            normalize_command_for_detection(Some("FOO='a b' git status")),
            Some("git status".to_owned())
        );
    }

    #[test]
    fn cuts_at_the_first_chain_operator() {
        assert_eq!(
            normalize_command_for_detection(Some("cargo build && git status")),
            Some("cargo build".to_owned())
        );
        assert_eq!(
            normalize_command_for_detection(Some("cargo build; git status")),
            Some("cargo build".to_owned())
        );
        assert_eq!(
            normalize_command_for_detection(Some("cargo build || true")),
            Some("cargo build".to_owned())
        );
        assert_eq!(
            normalize_command_for_detection(Some("cargo build | tee log")),
            Some("cargo build".to_owned())
        );
    }

    /// `rtk rewrite` 会产出 `export RTK_DB_PATH=...; <命令>` 形态；识别要能处理它。
    /// `export` 本身不是赋值形态，所以第一段停在分号处——与上游一致。
    #[test]
    fn a_leading_export_is_its_own_first_segment() {
        assert_eq!(
            normalize_command_for_detection(Some(
                "export RTK_DB_PATH='/tmp/x/history.db'; git status"
            )),
            Some("export rtk_db_path='/tmp/x/history.db'".to_owned())
        );
    }

    /// 归一化前先按行 `trim`。环境变量前缀模式要求赋值后面跟着空白，因此最后一段赋值
    /// 永远会被当成命令本身——包括「只有赋值」的命令。这是上游 `replace` 的实际行为。
    #[test]
    fn empty_and_assignment_only_commands_are_none() {
        assert_eq!(normalize_command_for_detection(None), None);
        assert_eq!(normalize_command_for_detection(Some("")), None);
        assert_eq!(normalize_command_for_detection(Some("   \n  ")), None);
        assert_eq!(normalize_command_for_detection(Some("&&")), None);

        // 只有赋值时，最后一段赋值被当作命令本身。
        assert_eq!(
            normalize_command_for_detection(Some("FOO=1")),
            Some("foo=1".to_owned())
        );
        assert_eq!(
            normalize_command_for_detection(Some("FOO=1 BAR=2 ")),
            Some("bar=2".to_owned())
        );
    }

    #[test]
    fn matches_against_normalized_segment() {
        let patterns = patterns();
        assert!(matches_command_patterns(Some("cargo check"), &patterns));
        assert!(matches_command_patterns(
            Some("FOO=1 git status --short"),
            &patterns
        ));
        assert!(!matches_command_patterns(Some("cargo test"), &patterns));
        assert!(!matches_command_patterns(None, &patterns));
    }

    #[test]
    fn matching_is_anchored_and_case_insensitive_by_normalization() {
        let patterns = patterns();
        assert!(matches_command_patterns(Some("CARGO BUILD"), &patterns));
        assert!(!matches_command_patterns(Some("sudo cargo build"), &patterns));
    }
}
