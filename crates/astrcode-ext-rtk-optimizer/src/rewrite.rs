//! 改写决策与 rtk 运行期状态。
//!
//! 上游 `command-rewriter.ts` 与 `runtime-guard.ts` 的移植，外加 `index.ts` 里那套
//! 「30 秒状态缓存 + rtk 缺失时旁路」的运行时守卫。
//!
//! # 平台边界：通知只能事后查询
//!
//! 上游用 TUI 通知展示「RTK rewrite: old -> new」与建议改写。S5R 的钩子返回值里没有
//! 任何面向用户的文本通道（`Replace` 只换工具入参，`ModifyResult` 只换工具结果正文），
//! 因此本插件把最近一次改写/建议记进运行期状态，由 `/rtk show` 呈现。
//! `showRewriteNotifications` 开关随之变成「是否记录这条信息」。

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::{config::Config, rtk, rtk::RtkExecutable};

/// rtk 可用性探测结果的有效期；过期后在下一次改写前重新探测。
const STATUS_TTL: Duration = Duration::from_secs(30);

/// 改写决策的成因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteReason {
    /// rtk 给出了不同的命令。
    Rewritten,
    /// 命令为空。
    Empty,
    /// 命令本身已经是 `rtk ...`。
    AlreadyRtk,
    /// rtk 认为这条命令没有等价形式。
    NoMatch,
}

impl RewriteReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rewritten => "rewritten",
            Self::Empty => "empty",
            Self::AlreadyRtk => "already_rtk",
            Self::NoMatch => "no_match",
        }
    }
}

/// 一次改写决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteDecision {
    pub changed: bool,
    pub original_command: String,
    pub rewritten_command: String,
    pub reason: RewriteReason,
    /// rtk 给出的非致命问题（例如主动拒绝改写的原因）。
    pub warning: Option<String>,
}

impl RewriteDecision {
    fn unchanged(command: &str, reason: RewriteReason, warning: Option<String>) -> Self {
        Self {
            changed: false,
            original_command: command.to_owned(),
            rewritten_command: command.to_owned(),
            reason,
            warning,
        }
    }
}

/// 计算一条命令的改写决策。
///
/// `executable` 为 `None` 时按裸 `rtk` 调用（首次探测尚未完成时的兜底）。
pub async fn compute_rewrite_decision(
    command: &str,
    executable: Option<&RtkExecutable>,
) -> RewriteDecision {
    if command.trim().is_empty() {
        return RewriteDecision::unchanged(command, RewriteReason::Empty, None);
    }

    if rtk::is_already_rtk(command) {
        return RewriteDecision::unchanged(command, RewriteReason::AlreadyRtk, None);
    }

    let fallback;
    let executable = match executable {
        Some(executable) => executable,
        None => {
            fallback = RtkExecutable {
                command: "rtk".to_owned(),
                resolved_path: None,
                resolver: if std::env::consts::OS == "win32" {
                    "where"
                } else {
                    "which"
                },
                warning: None,
            };
            &fallback
        },
    };

    let result = rtk::resolve_rtk_rewrite(command, executable).await;
    if result.changed && !result.rewritten_command.is_empty() {
        return RewriteDecision {
            changed: true,
            original_command: result.original_command,
            rewritten_command: result.rewritten_command,
            reason: RewriteReason::Rewritten,
            warning: None,
        };
    }

    RewriteDecision::unchanged(command, RewriteReason::NoMatch, result.error)
}

/// rtk 可用性快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub rtk_available: bool,
    pub last_checked_at: Option<Instant>,
    pub last_error: Option<String>,
    pub executable: Option<RtkExecutable>,
    /// 最近一次改写（或建议）的展示行；`/rtk show` 读取它。
    pub last_notice: Option<String>,
}

/// 运行期状态容器。
#[derive(Debug, Default)]
pub struct RtkRuntime {
    status: Mutex<RuntimeStatus>,
}

impl RtkRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取状态快照。锁中毒说明此前有 panic 穿过临界区，此时返回默认值而不是再次 panic。
    pub fn status(&self) -> RuntimeStatus {
        self.status
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().clone(), |status| status.clone())
    }

    /// 重新解析 rtk 可执行文件并探测其可用性。
    pub async fn refresh(&self) -> RuntimeStatus {
        let executable = rtk::resolve_rtk_executable().await;
        let (available, error) = rtk::probe_rtk_available(&executable).await;

        let previous_notice = self.status().last_notice;
        let status = RuntimeStatus {
            rtk_available: available,
            last_checked_at: Some(Instant::now()),
            last_error: error,
            executable: Some(executable),
            last_notice: previous_notice,
        };
        self.store(status.clone());
        status
    }

    /// 状态过期时刷新；`guardWhenRtkMissing` 关闭时完全不探测。
    pub async fn ensure_fresh(&self, config: &Config) {
        if !config.guard_when_rtk_missing {
            return;
        }
        let stale = self
            .status()
            .last_checked_at
            .is_none_or(|checked_at| checked_at.elapsed() > STATUS_TTL);
        if stale {
            self.refresh().await;
        }
    }

    /// 记录一条展示信息（最近一次改写 / 建议）。
    pub fn record_notice(&self, notice: String) {
        let mut status = self.status();
        status.last_notice = Some(notice);
        self.store(status);
    }

    fn store(&self, status: RuntimeStatus) {
        match self.status.lock() {
            Ok(mut current) => *current = status,
            Err(poisoned) => *poisoned.into_inner() = status,
        }
    }
}

/// `guardWhenRtkMissing` 打开且 rtk 不可用时，跳过命令改写处理。
pub fn should_skip_command_handling(config: &Config, status: &RuntimeStatus) -> bool {
    config.guard_when_rtk_missing && !status.rtk_available
}

/// 把改写结果压成一行展示信息。
pub fn format_rewrite_notice(original: &str, rewritten: &str) -> String {
    format!(
        "RTK rewrite: {} -> {}",
        trim_message(original, 100),
        trim_message(rewritten, 120)
    )
}

/// 把改写失败压成一行展示信息。
pub fn format_rewrite_warning(command: &str, warning: &str) -> String {
    format!(
        "rtk rewrite skipped for '{}' ({})",
        trim_message(command, 100),
        trim_message(warning, 120)
    )
}

/// 折叠空白并截断到 `max_length` 个字符。
fn trim_message(raw: &str, max_length: usize) -> String {
    let clean = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= max_length {
        return clean;
    }
    let head: String = clean.chars().take(max_length.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_empty_command_is_not_rewritten() {
        let decision = compute_rewrite_decision("   ", None).await;
        assert!(!decision.changed);
        assert_eq!(decision.reason, RewriteReason::Empty);
    }

    /// 已经是 `rtk ...` 的命令不会再次经过 rtk，因此不产生任何宿主调用。
    #[tokio::test]
    async fn an_already_rtk_command_short_circuits() {
        let decision = compute_rewrite_decision("rtk git status", None).await;
        assert!(!decision.changed);
        assert_eq!(decision.reason, RewriteReason::AlreadyRtk);
        assert_eq!(decision.rewritten_command, "rtk git status");
    }

    #[tokio::test]
    async fn an_already_rtk_command_behind_env_prefix_short_circuits() {
        let decision = compute_rewrite_decision("FOO=1 rtk log", None).await;
        assert_eq!(decision.reason, RewriteReason::AlreadyRtk);
    }

    #[test]
    fn runtime_starts_unavailable_and_not_checked() {
        let runtime = RtkRuntime::new();
        let status = runtime.status();
        assert!(!status.rtk_available);
        assert!(status.last_checked_at.is_none());
        assert!(status.executable.is_none());
    }

    #[test]
    fn notices_round_trip_through_the_runtime() {
        let runtime = RtkRuntime::new();
        runtime.record_notice("RTK rewrite: a -> b".to_owned());
        assert_eq!(
            runtime.status().last_notice.as_deref(),
            Some("RTK rewrite: a -> b")
        );
    }

    #[test]
    fn skipping_requires_the_guard_and_a_missing_binary() {
        let config = Config::default();

        let missing = RuntimeStatus::default();
        assert!(should_skip_command_handling(&config, &missing));

        let available = RuntimeStatus {
            rtk_available: true,
            ..RuntimeStatus::default()
        };
        assert!(!should_skip_command_handling(&config, &available));

        let unguarded = Config {
            guard_when_rtk_missing: false,
            ..Config::default()
        };
        assert!(!should_skip_command_handling(&unguarded, &missing));
    }

    #[test]
    fn notices_are_collapsed_and_truncated() {
        let notice = format_rewrite_notice("  a\n b ", "x");
        assert_eq!(notice, "RTK rewrite: a b -> x");

        let long = format_rewrite_notice(&"y".repeat(500), "z");
        assert!(long.chars().count() < 130, "{long}");
    }

    #[test]
    fn warnings_name_the_skipped_command() {
        let warning = format_rewrite_warning("git status", "boom");
        assert_eq!(warning, "rtk rewrite skipped for 'git status' (boom)");
    }
}
