//! `rtk` 可执行文件的定位与 `rtk rewrite` 调用。
//!
//! 上游 `rtk-executable-resolver.ts` 与 `rtk-rewrite-provider.ts` 的移植。
//!
//! 改写决策的唯一来源是外部 `rtk` 二进制：本插件不维护任何改写规则表，`rtk` 支持哪些
//! 命令、如何解析 shell、何时拒绝，全部由它自己回答（`rtk rewrite` 的退出码即契约）。
//!
//! 调用走宿主受限子进程（`HostClient::process().spawn`，需要 `process_spawn` 能力）：
//! cwd 落在会话工作目录内，stdout/stderr 与总时长都由宿主约束。

use std::sync::LazyLock;

use astrcode_extension_worker::worker_prelude::*;
use regex::Regex;

/// `rtk rewrite` 的单次预算。
const REWRITE_TIMEOUT_MS: u64 = 3_000;
/// `rtk --version` 的探测预算。
const VERSION_TIMEOUT_MS: u64 = 5_000;
/// `which` / `where` 的探测预算。
const RESOLVER_TIMEOUT_MS: u64 = 1_000;

static WRAPPING_QUOTES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:"(.*)"|'(.*)')$"#).expect("wrapping quote pattern is valid")
});

/// 已解析的 rtk 可执行文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtkExecutable {
    /// 实际用于 spawn 的命令（解析成功时是绝对路径）。
    pub command: String,
    /// 解析出的绝对路径；解析失败时为 `None`。
    pub resolved_path: Option<String>,
    /// 使用的解析器：`which` 或 `where`。
    pub resolver: &'static str,
    /// 解析过程中的非致命问题。
    pub warning: Option<String>,
}

impl RtkExecutable {
    /// 未解析时的兜底：直接按 `rtk` 调用，交给 PATH 决定。
    fn fallback(resolver: &'static str, warning: String) -> Self {
        Self {
            command: "rtk".to_owned(),
            resolved_path: None,
            resolver,
            warning: Some(warning),
        }
    }
}

/// 一次 `rtk rewrite` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteProviderResult {
    pub changed: bool,
    pub original_command: String,
    pub rewritten_command: String,
    pub exit_code: i64,
    pub error: Option<String>,
}

impl RewriteProviderResult {
    fn unchanged(command: &str, exit_code: i64, error: Option<String>) -> Self {
        Self {
            changed: false,
            original_command: command.to_owned(),
            rewritten_command: command.to_owned(),
            exit_code,
            error,
        }
    }
}

/// 本平台的解析器命令。
pub fn resolver_command(platform: &str) -> (&'static str, Vec<String>) {
    if platform == "win32" {
        ("where", vec!["rtk".to_owned()])
    } else {
        ("which", vec!["rtk".to_owned()])
    }
}

/// 从解析器输出里取第一个非空行，并剥掉可能包裹的引号。
pub fn parse_rtk_executable_path(stdout: &str) -> Option<String> {
    stdout
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(strip_wrapping_quotes)
        .find(|candidate| !candidate.is_empty())
}

fn strip_wrapping_quotes(value: &str) -> String {
    if value.len() < 2 {
        return value.to_owned();
    }
    match WRAPPING_QUOTES.captures(value) {
        Some(captures) => captures
            .get(1)
            .or_else(|| captures.get(2))
            .map_or_else(|| value.to_owned(), |group| group.as_str().to_owned()),
        None => value.to_owned(),
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn spawn_capture(command: &str, args: &[&str], timeout_ms: u64) -> Result<HostProcessOutput, String> {
    let mut request = HostProcessRequest::new(command);
    request.args = args.iter().map(|arg| (*arg).to_owned()).collect();
    request.timeout_ms = Some(timeout_ms);

    HostClient::process()
        .spawn(request)
        .await
        .map_err(|error| error.message)
}

/// 定位 rtk 可执行文件。解析失败时回落为裸 `rtk` 并带上警告，而不是直接失败。
pub async fn resolve_rtk_executable() -> RtkExecutable {
    let platform = std::env::consts::OS;
    let (resolver, args) = resolver_command(platform);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    match spawn_capture(resolver, &args, RESOLVER_TIMEOUT_MS).await {
        Ok(output) if output.success => match parse_rtk_executable_path(&output.stdout) {
            Some(path) => RtkExecutable {
                command: path.clone(),
                resolved_path: Some(path),
                resolver,
                warning: None,
            },
            None => RtkExecutable::fallback(resolver, format!("{resolver} printed no path")),
        },
        Ok(output) => {
            let detail = if output.stderr.trim().is_empty() {
                output.stdout.trim().to_owned()
            } else {
                output.stderr.trim().to_owned()
            };
            RtkExecutable::fallback(
                resolver,
                format!(
                    "{resolver} failed{}",
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", collapse_whitespace(&detail))
                    }
                ),
            )
        },
        Err(error) => RtkExecutable::fallback(
            resolver,
            format!("{resolver} failed: {}", collapse_whitespace(&error)),
        ),
    }
}

/// 探测 `rtk --version` 是否可用。返回可用性与失败原因。
pub async fn probe_rtk_available(executable: &RtkExecutable) -> (bool, Option<String>) {
    match spawn_capture(&executable.command, &["--version"], VERSION_TIMEOUT_MS).await {
        Ok(output) if output.success => (true, None),
        Ok(output) => {
            let detail = if output.stderr.trim().is_empty() {
                output.stdout.trim().to_owned()
            } else {
                output.stderr.trim().to_owned()
            };
            let detail = collapse_whitespace(&detail);
            let reason = if detail.is_empty() {
                format!("exit {}", output.status.unwrap_or(-1))
            } else {
                detail
            };
            (false, Some(reason))
        },
        Err(error) => (false, Some(collapse_whitespace(&error))),
    }
}

/// 命令本身已经是 `rtk ...`（穿透前置环境变量后判断）。
pub fn is_already_rtk(command: &str) -> bool {
    let effective = crate::shell::split_leading_env_assignments(command.trim_start());
    let effective = effective.command.trim_start();
    effective == "rtk" || effective.starts_with("rtk ")
}

/// 调用 `rtk rewrite` 并解释退出码。
///
/// 退出码语义（与 `rtk rewrite --help` 一致）：
/// - `0` / `3`：成功，stdout 是改写后的命令；
/// - `1`：命令没有 rtk 等价形式（不是错误）；
/// - `2`：rtk 主动拒绝改写（带 stderr 原因）；
/// - 其它：意外退出码。
pub async fn resolve_rtk_rewrite(command: &str, executable: &RtkExecutable) -> RewriteProviderResult {
    if command.trim().is_empty() {
        return RewriteProviderResult::unchanged(command, 1, None);
    }

    let output = match spawn_capture(&executable.command, &["rewrite", command], REWRITE_TIMEOUT_MS)
        .await
    {
        Ok(output) => output,
        Err(error) => {
            return RewriteProviderResult::unchanged(command, -1, Some(collapse_whitespace(&error)));
        },
    };

    let code = i64::from(output.status.unwrap_or(-1));

    match code {
        1 => RewriteProviderResult::unchanged(command, 1, None),
        2 => {
            let detail = collapse_whitespace(output.stderr.trim());
            RewriteProviderResult::unchanged(
                command,
                2,
                Some(if detail.is_empty() {
                    "rtk denied rewrite".to_owned()
                } else {
                    detail
                }),
            )
        },
        0 | 3 => {
            let rewritten = output.stdout.trim();
            if rewritten.is_empty() {
                return RewriteProviderResult::unchanged(
                    command,
                    code,
                    Some("rtk returned empty output".to_owned()),
                );
            }
            if rewritten == command {
                return RewriteProviderResult::unchanged(command, code, None);
            }
            RewriteProviderResult {
                changed: true,
                original_command: command.to_owned(),
                rewritten_command: rewritten.to_owned(),
                exit_code: code,
                error: None,
            }
        },
        other => RewriteProviderResult::unchanged(
            command,
            other,
            Some(format!("unexpected exit code {other}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_command_depends_on_the_platform() {
        assert_eq!(resolver_command("win32").0, "where");
        assert_eq!(resolver_command("linux").0, "which");
        assert_eq!(resolver_command("macos").0, "which");
    }

    #[test]
    fn parses_the_first_non_empty_resolver_line() {
        assert_eq!(
            parse_rtk_executable_path("\n/usr/bin/rtk\n/other/rtk\n").as_deref(),
            Some("/usr/bin/rtk")
        );
        assert_eq!(parse_rtk_executable_path("   \n  "), None);
        assert_eq!(parse_rtk_executable_path(""), None);
    }

    #[test]
    fn strips_wrapping_quotes_only() {
        assert_eq!(parse_rtk_executable_path("\"/opt/rtk\"\n").as_deref(), Some("/opt/rtk"));
        assert_eq!(parse_rtk_executable_path("'/opt/rtk'\n").as_deref(), Some("/opt/rtk"));
        assert_eq!(parse_rtk_executable_path("/opt/rtk\n").as_deref(), Some("/opt/rtk"));
    }

    #[test]
    fn already_rtk_detection_ignores_environment_prefixes() {
        assert!(is_already_rtk("rtk git status"));
        assert!(is_already_rtk("rtk"));
        assert!(is_already_rtk("FOO=1 rtk git status"));
        assert!(is_already_rtk("  RTK_DB_PATH='/x' rtk log"));
        assert!(!is_already_rtk("git status"));
        assert!(!is_already_rtk("rtkx git status"));
    }

    #[test]
    fn fallback_keeps_the_bare_command_and_records_the_warning() {
        let executable = RtkExecutable::fallback("which", "boom".to_owned());
        assert_eq!(executable.command, "rtk");
        assert_eq!(executable.resolved_path, None);
        assert_eq!(executable.warning.as_deref(), Some("boom"));
    }

    #[test]
    fn unchanged_results_keep_the_original_command() {
        let result = RewriteProviderResult::unchanged("git status", 1, None);
        assert!(!result.changed);
        assert_eq!(result.rewritten_command, "git status");
        assert_eq!(result.exit_code, 1);
    }

    #[test]
    fn whitespace_is_collapsed_in_error_details() {
        assert_eq!(collapse_whitespace("  a\n\tb  c "), "a b c");
    }
}
