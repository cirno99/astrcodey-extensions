//! shell 命令的纯文本处理：前置环境变量、`RTK_DB_PATH` 注入、改写后的安全修正。
//!
//! 上游 `shell-env-prefix.ts`、`rtk-command-environment.ts`、`rewrite-pipeline-safety.ts`
//! 与 `windows-command-helpers.ts` 的移植。
//!
//! # 平台闸门
//!
//! `apply_windows_bash_compatibility_fixes` 与 `apply_rewritten_command_shell_safety_fixups`
//! 在上游是 win32 专属的。这里保留完整逻辑并显式接收 `platform`，因此非 Windows 上
//! 是直通，同时逻辑本身仍可在任意平台被单元测试覆盖。
//!
//! # 与上游的差异
//!
//! `RTK_DB_PATH` 指向的临时目录用本插件自己的名字（`astrcode-rtk-optimizer`）而不是
//! 上游的 `pi-rtk-optimizer`，避免同一台机器上两个插件共用同一个历史库。

use std::{path::PathBuf, sync::LazyLock};

use regex::Regex;

/// 改写后的命令里注入的历史库环境变量名。
const RTK_DB_PATH_ENV_NAME: &str = "RTK_DB_PATH";
/// 临时目录下属于本插件的子目录名。
const RTK_TEMP_DIR_NAME: &str = "astrcode-rtk-optimizer";

/// POSIX 单引号字面量：`'` 内的 `'\''` 表示一个字面单引号。
const SINGLE_QUOTED_SHELL_VALUE: &str = r"'(?:'\\''|[^'])*'";
/// 环境变量值：双引号、单引号或裸词。
const SHELL_ENV_VALUE: &str = r#"(?:"[^"]*"|'(?:'\\''|[^'])*'|[^\s;]+)"#;

/// 前置环境变量赋值：`FOO=bar BAZ='q u x' <命令>`。
static LEADING_ENV_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"^((?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|{SINGLE_QUOTED_SHELL_VALUE}|[^\s]+)\s+)*)"#
    ))
    .expect("leading env assignment pattern is valid")
});

/// 命令里已经带了 `RTK_DB_PATH=...` 赋值。
///
/// 上游用 `(?=\s|$)` 前瞻，`regex` crate 不支持前瞻；这里把终结符改成消费式
/// `(?:\s|$)`——本函数只做布尔判断，消费掉终结符没有副作用。
static RTK_DB_PATH_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:^|\s)RTK_DB_PATH={SHELL_ENV_VALUE}(?:\s|$)"
    ))
    .expect("rtk db path assignment pattern is valid")
});

/// 命令以 `export RTK_DB_PATH=...` 开头。
static RTK_DB_PATH_EXPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^export\s+RTK_DB_PATH={SHELL_ENV_VALUE}(?:\s*(?:;|$))"
    ))
    .expect("rtk db path export pattern is valid")
});

/// 改写结果里可能出现的 `export RTK_DB_PATH=...; ` 前导。
static LEADING_RTK_DB_PATH_PRELUDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(\s*export\s+RTK_DB_PATH={SHELL_ENV_VALUE}\s*;\s*)([\s\S]*)$"
    ))
    .expect("rtk db path prelude pattern is valid")
});

/// 生产者的 `2>&1` 收尾。
static STDERR_MERGE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*?)(?:\s+)?2>\s*&1\s*$").expect("stderr merge is valid"));

/// `rtk ` 开头。
static RTK_COMMAND_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^rtk\s+").expect("rtk prefix is valid"));

/// 前置环境变量赋值的拆分结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadingEnvAssignment {
    pub env_prefix: String,
    pub command: String,
}

/// 把 `FOO=1 BAR=2 <命令>` 拆成前缀与命令两部分。
pub fn split_leading_env_assignments(input: &str) -> LeadingEnvAssignment {
    let env_prefix = LEADING_ENV_ASSIGNMENT
        .captures(input)
        .and_then(|captures| captures.get(1))
        .map_or_else(String::new, |value| value.as_str().to_owned());
    LeadingEnvAssignment {
        command: input[env_prefix.len()..].to_owned(),
        env_prefix,
    }
}

/// 取第一个非空的环境变量值。
fn first_non_empty_env(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

fn resolve_temporary_directory() -> String {
    if cfg!(windows) {
        if let Some(value) = first_non_empty_env(&["TEMP", "TMP"]) {
            return value;
        }
        if let Some(value) = first_non_empty_env(&["LOCALAPPDATA"]) {
            return format!("{value}/Temp");
        }
        if let Some(value) = first_non_empty_env(&["USERPROFILE"]) {
            return format!("{value}/AppData/Local/Temp");
        }
        if let Some(value) = first_non_empty_env(&["SystemRoot", "WINDIR"]) {
            return format!("{value}/Temp");
        }
        return "C:/Windows/Temp".to_owned();
    }

    first_non_empty_env(&["TMPDIR", "TMP"]).unwrap_or_else(|| "/tmp".to_owned())
}

/// 本插件的临时历史库路径。
///
/// 用 `Path::join` 拼（与上游 `path.join` 一致），因此 Windows 上得到的是反斜杠路径；
/// 它随后会经 [`quote_for_shell_env`] 换成正斜杠。
fn temporary_rtk_history_db_path() -> String {
    PathBuf::from(resolve_temporary_directory())
        .join(RTK_TEMP_DIR_NAME)
        .join("history.db")
        .to_string_lossy()
        .into_owned()
}

/// 单引号包裹一个值，内部单引号按 `'\''` 转义。
fn quote_for_shell_env(value: &str) -> String {
    let normalized = if cfg!(windows) {
        value.replace('\\', "/")
    } else {
        value.to_owned()
    };
    format!("'{}'", normalized.replace('\'', r"'\''"))
}

fn has_leading_rtk_db_path_assignment(command: &str) -> bool {
    let trimmed = command.trim_start();
    RTK_DB_PATH_ASSIGNMENT.is_match(&split_leading_env_assignments(trimmed).env_prefix)
        || RTK_DB_PATH_EXPORT.is_match(trimmed)
}

fn has_inherited_rtk_db_path() -> bool {
    std::env::var(RTK_DB_PATH_ENV_NAME).is_ok_and(|value| !value.trim().is_empty())
}

/// 给改写后的命令注入 `RTK_DB_PATH`，把 rtk 的历史库限定在临时目录。
///
/// 命令自己已经设置了、或环境里已经继承时原样返回。
pub fn apply_rtk_command_environment(command: &str) -> String {
    if command.trim().is_empty() {
        return command.to_owned();
    }
    if has_leading_rtk_db_path_assignment(command) || has_inherited_rtk_db_path() {
        return command.to_owned();
    }
    format!(
        "export {RTK_DB_PATH_ENV_NAME}={}; {command}",
        quote_for_shell_env(&temporary_rtk_history_db_path())
    )
}

/// 引号 / 转义状态机。
#[derive(Debug, Clone, Copy, Default)]
struct QuoteEscapeState {
    quote: Option<char>,
    escaped: bool,
}

/// 推进一步状态机。返回 `true` 表示该字符被状态机消费，调用方应跳过。
fn advance_quote_escape_state(
    state: &mut QuoteEscapeState,
    character: char,
    quote_chars: &[char],
) -> bool {
    if state.escaped {
        state.escaped = false;
        return true;
    }

    if let Some(active) = state.quote {
        if character == '\\' && active != '\'' {
            state.escaped = true;
            return true;
        }
        if character == active {
            state.quote = None;
        }
        return true;
    }

    if character == '\\' {
        state.escaped = true;
        return true;
    }

    if quote_chars.contains(&character) {
        state.quote = Some(character);
        return true;
    }

    false
}

/// 拆分出来的顶层管道结构。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPipeline {
    segments: Vec<String>,
    separators: Vec<String>,
    suffix: String,
}

/// 解析「顶层只有一个生产者 + 管道」的简单结构；其余形态返回 `None`。
fn parse_simple_top_level_pipeline(command: &str) -> Option<ParsedPipeline> {
    let characters: Vec<char> = command.chars().collect();
    let mut segments: Vec<String> = Vec::new();
    let mut separators: Vec<String> = Vec::new();
    let mut state = QuoteEscapeState::default();
    let mut segment_start = 0usize;
    let mut suffix = String::new();
    let mut index = 0usize;

    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();
        let previous = index.checked_sub(1).map(|previous| characters[previous]);

        if advance_quote_escape_state(&mut state, character, &['"', '\'', '`']) {
            index += 1;
            continue;
        }

        let is_double = |value: Option<char>, expected: char| value == Some(expected);

        if (character == '|' && is_double(next, '|'))
            || (character == '&' && is_double(next, '&'))
            || character == ';'
        {
            if separators.is_empty() {
                return None;
            }
            segments.push(characters[segment_start..index].iter().collect());
            suffix = characters[index..].iter().collect();
            break;
        }

        if character == '|' && !is_double(previous, '>') {
            let separator_length = if is_double(next, '&') { 2 } else { 1 };
            segments.push(characters[segment_start..index].iter().collect());
            separators.push(
                characters[index..index + separator_length]
                    .iter()
                    .collect(),
            );
            segment_start = index + separator_length;
            index += separator_length;
            continue;
        }

        if character == '&'
            && !is_double(next, '>')
            && !is_double(previous, '>')
            && !is_double(previous, '<')
        {
            return None;
        }

        index += 1;
    }

    if separators.is_empty() {
        return None;
    }

    if suffix.is_empty() {
        segments.push(characters[segment_start..].iter().collect());
    }

    Some(ParsedPipeline {
        segments,
        separators,
        suffix,
    })
}

/// 一个待缓冲的生产者命令。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProducerRewritePlan {
    command: String,
    capture_stderr: bool,
}

fn extract_producer_rewrite_plan(
    segment: &str,
    first_separator: &str,
) -> Option<ProducerRewritePlan> {
    let trimmed = segment.trim();
    let LeadingEnvAssignment { env_prefix, command } = split_leading_env_assignments(trimmed);
    if !RTK_COMMAND_PREFIX.is_match(&command) {
        return None;
    }

    if let Some(captures) = STDERR_MERGE_SUFFIX.captures(&command) {
        let command = captures
            .get(1)
            .map_or("", |value| value.as_str())
            .trim_end();
        return (!command.is_empty()).then(|| ProducerRewritePlan {
            command: format!("{env_prefix}{command}").trim().to_owned(),
            capture_stderr: true,
        });
    }

    Some(ProducerRewritePlan {
        command: format!("{env_prefix}{command}").trim().to_owned(),
        capture_stderr: first_separator == "|&",
    })
}

/// 把 `rtk ... | consumer` 改写成「先落临时文件再喂给消费者」的缓冲形式。
///
/// Windows 的 bash 兼容层无法可靠地流式处理 rtk 的管道输出，因此上游在 win32 上把
/// 管道改成临时文件中转，并把生产者的退出码传播到整个复合命令。
fn build_buffered_pipeline_command(producer: &ProducerRewritePlan, remainder: &str) -> String {
    let temp_file_variable = "__astrcode_rtk_pipe_tmp";
    let status_variable = "__astrcode_rtk_pipe_status";
    let producer_redirect = if producer.capture_stderr {
        format!(r#"> "${temp_file_variable}" 2>&1"#)
    } else {
        format!(r#"> "${temp_file_variable}""#)
    };
    let cleanup_trap = format!(r#"rm -f "${temp_file_variable}""#);

    [
        "{".to_owned(),
        format!("{temp_file_variable}=\"$(mktemp)\" || exit $?;"),
        format!("{status_variable}=0;"),
        format!("trap '{cleanup_trap}' EXIT HUP INT TERM;"),
        format!("{} {producer_redirect};", producer.command),
        format!("{status_variable}=$?;"),
        format!(
            "if [ ${status_variable} -eq 0 ]; then ({remainder}) < \"${temp_file_variable}\"; \
             {status_variable}=$?; fi;"
        ),
        format!("exit ${status_variable};"),
        "}".to_owned(),
    ]
    .join(" ")
}

/// 改写后的命令在 win32 上的 shell 安全修正。
pub fn apply_rewritten_command_shell_safety_fixups(command: &str, platform: &str) -> String {
    if platform != "win32" {
        return command.to_owned();
    }

    let (environment_prelude, target) = match LEADING_RTK_DB_PATH_PRELUDE.captures(command) {
        Some(captures) => (
            captures.get(1).map_or("", |value| value.as_str()).to_owned(),
            captures.get(2).map_or("", |value| value.as_str()).to_owned(),
        ),
        None => (String::new(), command.to_owned()),
    };

    let Some(pipeline) = parse_simple_top_level_pipeline(&target) else {
        return command.to_owned();
    };

    let Some(producer) = extract_producer_rewrite_plan(
        pipeline.segments.first().map_or("", String::as_str),
        pipeline.separators.first().map_or("", String::as_str),
    ) else {
        return command.to_owned();
    };

    let remainder = pipeline
        .segments
        .iter()
        .skip(1)
        .enumerate()
        .map(|(index, segment)| {
            format!(
                "{}{segment}",
                if index == 0 {
                    ""
                } else {
                    pipeline.separators.get(index).map_or("", String::as_str)
                }
            )
        })
        .collect::<String>()
        .trim()
        .to_owned();
    if remainder.is_empty() {
        return command.to_owned();
    }

    let suffix = if pipeline.suffix.is_empty() {
        String::new()
    } else {
        format!(" {}", pipeline.suffix.trim_start())
    };
    format!(
        "{environment_prelude}{}{suffix}",
        build_buffered_pipeline_command(&producer, &remainder)
    )
}

/// win32 上把 `cd /d <路径>` 规范化成 bash 可用的 `cd "<路径>"`。
fn rewrite_leading_cd_slash_d(command: &str) -> Option<String> {
    let characters: Vec<char> = command.chars().collect();
    let mut index = 0usize;

    while index < characters.len() && characters[index].is_whitespace() {
        index += 1;
    }
    for expected in ['c', 'd'] {
        if !characters
            .get(index)
            .is_some_and(|character| character.eq_ignore_ascii_case(&expected))
        {
            return None;
        }
        index += 1;
    }
    if !characters.get(index).is_some_and(|character| character.is_whitespace()) {
        return None;
    }
    while index < characters.len() && characters[index].is_whitespace() {
        index += 1;
    }
    if characters.get(index) != Some(&'/') || !characters.get(index + 1).is_some_and(|c| c.eq_ignore_ascii_case(&'d'))
    {
        return None;
    }
    index += 2;
    if !characters.get(index).is_some_and(|character| character.is_whitespace()) {
        return None;
    }
    while index < characters.len() && characters[index].is_whitespace() {
        index += 1;
    }

    let path_start = index;
    let mut state = QuoteEscapeState::default();
    let mut operator = String::new();
    let mut raw_path = String::new();
    let mut tail = String::new();

    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();

        if advance_quote_escape_state(&mut state, character, &['"', '\'']) {
            index += 1;
            continue;
        }

        let (found, length) = if character == '&' && next == Some('&') {
            ("&&", 2)
        } else if character == '|' && next == Some('|') {
            ("||", 2)
        } else if character == '|' || character == ';' {
            ("", 1)
        } else {
            ("", 0)
        };

        if length > 0 {
            operator = if found.is_empty() {
                character.to_string()
            } else {
                found.to_owned()
            };
            raw_path = characters[path_start..index].iter().collect();
            tail = characters[index + length..].iter().collect();
            break;
        }

        index += 1;
    }

    if operator.is_empty() {
        raw_path = characters[path_start..].iter().collect();
    }

    let normalized = normalize_windows_path_for_bash(&raw_path);
    let quoted = quote_for_bash(&normalized);
    Some(if operator.is_empty() {
        format!("cd {quoted}")
    } else {
        format!("cd {quoted} {operator} {}", tail.trim_start())
    })
}

fn normalize_windows_path_for_bash(raw_path: &str) -> String {
    let trimmed = raw_path.trim();
    let unquoted = if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    unquoted.replace('\\', "/")
}

fn quote_for_bash(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

/// win32 上给 Python 命令补 `PYTHONIOENCODING=utf-8`。
fn ensure_python_utf8(command: &str) -> Option<String> {
    static PYTHONIOENCODING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bPYTHONIOENCODING\s*=").expect("pythonioencoding pattern is valid")
    });
    static PYTHON_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(^|[;&|]\s*|&&\s*|\|\|\s*)python(?:3(?:\.[0-9]+)?)?\b")
            .expect("python command pattern is valid")
    });

    if PYTHONIOENCODING.is_match(command) {
        return None;
    }
    if !PYTHON_COMMAND.is_match(command) {
        return None;
    }
    Some(format!("PYTHONIOENCODING=utf-8 {command}"))
}

/// win32 上的 bash 兼容修正。返回修正后的命令与实际应用的修正名。
pub fn apply_windows_bash_compatibility_fixes(command: &str, platform: &str) -> (String, Vec<String>) {
    if platform != "win32" {
        return (command.to_owned(), Vec::new());
    }

    let mut next = command.to_owned();
    let mut applied: Vec<String> = Vec::new();

    if let Some(fixed) = rewrite_leading_cd_slash_d(&next) {
        next = fixed;
        applied.push("cd-/d".to_owned());
    }

    if let Some(fixed) = ensure_python_utf8(&next) {
        next = fixed;
        applied.push("python-utf8".to_owned());
    }

    (next, applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_leading_env_assignments() {
        let split = split_leading_env_assignments(r#"FOO=1 BAR="a b" cargo build"#);
        assert_eq!(split.env_prefix, r#"FOO=1 BAR="a b" "#);
        assert_eq!(split.command, "cargo build");

        let split = split_leading_env_assignments("cargo build");
        assert_eq!(split.env_prefix, "");
        assert_eq!(split.command, "cargo build");

        let split = split_leading_env_assignments("FOO='a b' git status");
        assert_eq!(split.env_prefix, "FOO='a b' ");
        assert_eq!(split.command, "git status");
    }

    #[test]
    fn detects_an_existing_rtk_db_path_assignment() {
        assert!(has_leading_rtk_db_path_assignment(
            "RTK_DB_PATH='/tmp/x' rtk git status"
        ));
        assert!(has_leading_rtk_db_path_assignment(
            "export RTK_DB_PATH='/tmp/x'; rtk git status"
        ));
        assert!(!has_leading_rtk_db_path_assignment("rtk git status"));
        assert!(!has_leading_rtk_db_path_assignment(
            "FOO=1 rtk git status"
        ));
    }

    #[test]
    fn injects_the_temporary_history_db_path() {
        let command = apply_rtk_command_environment("rtk git status");
        assert!(command.starts_with("export RTK_DB_PATH='"));
        assert!(command.contains(RTK_TEMP_DIR_NAME));
        assert!(command.ends_with("; rtk git status"));
    }

    #[test]
    fn does_not_double_inject_the_history_db_path() {
        let already = "export RTK_DB_PATH='/tmp/x/history.db'; rtk git status";
        assert_eq!(apply_rtk_command_environment(already), already);
    }

    #[test]
    fn empty_commands_are_returned_unchanged() {
        assert_eq!(apply_rtk_command_environment("   "), "   ");
    }

    #[test]
    fn quotes_single_quotes_in_env_values() {
        assert_eq!(quote_for_shell_env("a'b"), r"'a'\''b'");
        assert_eq!(quote_for_shell_env("plain"), "'plain'");
    }

    #[test]
    fn non_windows_platforms_are_passthrough() {
        let command = "rtk git status | head";
        assert_eq!(
            apply_rewritten_command_shell_safety_fixups(command, "linux"),
            command
        );
        assert_eq!(
            apply_windows_bash_compatibility_fixes("cd /d C:\\x", "linux"),
            ("cd /d C:\\x".to_owned(), Vec::new())
        );
    }

    #[test]
    fn windows_cd_slash_d_is_rewritten_for_bash() {
        let (fixed, applied) = apply_windows_bash_compatibility_fixes("cd /d C:\\work && ls", "win32");
        assert_eq!(fixed, "cd \"C:/work\" && ls");
        assert_eq!(applied, vec!["cd-/d".to_owned()]);

        let (fixed, _) = apply_windows_bash_compatibility_fixes("cd /d \"D:\\a b\"", "win32");
        assert_eq!(fixed, "cd \"D:/a b\"");
    }

    #[test]
    fn windows_python_commands_get_utf8_encoding() {
        let (fixed, applied) = apply_windows_bash_compatibility_fixes("python3 script.py", "win32");
        assert_eq!(fixed, "PYTHONIOENCODING=utf-8 python3 script.py");
        assert_eq!(applied, vec!["python-utf8".to_owned()]);

        let (fixed, applied) =
            apply_windows_bash_compatibility_fixes("PYTHONIOENCODING=utf-8 python x.py", "win32");
        assert_eq!(fixed, "PYTHONIOENCODING=utf-8 python x.py");
        assert!(applied.is_empty());
    }

    #[test]
    fn windows_pipeline_buffering_rewrites_rtk_producers() {
        let fixed = apply_rewritten_command_shell_safety_fixups("rtk git log | head -20", "win32");
        assert!(fixed.starts_with("{ __astrcode_rtk_pipe_tmp="), "{fixed}");
        assert!(fixed.contains("rtk git log >"), "{fixed}");
        assert!(fixed.contains("< \"$__astrcode_rtk_pipe_tmp\""), "{fixed}");
    }

    #[test]
    fn windows_pipeline_buffering_keeps_the_environment_prelude() {
        let fixed = apply_rewritten_command_shell_safety_fixups(
            "export RTK_DB_PATH='/tmp/x'; rtk git log | head",
            "win32",
        );
        assert!(fixed.starts_with("export RTK_DB_PATH='/tmp/x'; {"), "{fixed}");
    }

    #[test]
    fn windows_pipeline_buffering_skips_non_rtk_producers() {
        let command = "git log | head -20";
        assert_eq!(
            apply_rewritten_command_shell_safety_fixups(command, "win32"),
            command
        );
    }

    /// `||` 属于顶层短路：上游只在「还没遇到管道」时返回 `None`，遇到管道之后会把
    /// 它当作后缀原样接在缓冲命令后面。这里钉住上游的实际行为。
    #[test]
    fn pipeline_parsing_ignores_quoted_operators() {
        assert_eq!(parse_simple_top_level_pipeline("echo 'a | b'"), None);
        assert_eq!(
            parse_simple_top_level_pipeline("echo a | cat").unwrap().separators,
            vec!["|".to_owned()]
        );
        assert_eq!(
            parse_simple_top_level_pipeline("echo a |& cat")
                .unwrap()
                .separators,
            vec!["|&".to_owned()]
        );
        assert_eq!(parse_simple_top_level_pipeline("a && b"), None);
        assert_eq!(parse_simple_top_level_pipeline("a; b"), None);

        let parsed = parse_simple_top_level_pipeline("a | b || c").unwrap();
        assert_eq!(parsed.separators, vec!["|".to_owned()]);
        assert_eq!(parsed.suffix, "|| c");
    }
}
