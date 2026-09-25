//! Worker 装配、`before_provider_request` 观测钩子与 `/cache-doctor` 命令族。
//!
//! # 这个钩子只观测，不改写
//!
//! `before_provider_request` 是 provider 请求路径上的同步钩子，本插件永远返回
//! [`ProviderResult::Allow`]，从不返回 `ReplaceMessages` / `AppendMessages`：
//! astrcode 的 system prompt 由宿主按固定 section 顺序组装，稳定段（Identity /
//! System / Task Guidelines / Communication）本来就在最前，重排是空操作。
//! 这里做的唯一一件事是记录「上一次请求的逐消息指纹」，用来在历史被追溯改写时
//! 指出断点位置。
//!
//! # 关闭观测就是真的关闭
//!
//! `watch = off` 时钩子直接返回 `Allow`，不做任何指纹计算，因此关闭后热路径零开销。

use std::sync::Arc;

use astrcode_extension_sdk::s5r::hooks::ProviderHookInput;
use astrcode_extension_worker::worker_prelude::*;

use crate::{
    config::ConfigStore,
    report,
    usage::{self, UsageScan},
    watch::WatchRegistry,
};

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-cache-doctor";

/// 命令名，宿主侧解析为 `/cache-doctor`。
pub const COMMAND_NAME: &str = "cache-doctor";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let config = Arc::new(ConfigStore::for_extension(EXTENSION_ID));
    let watch = Arc::new(WatchRegistry::new());
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    // `provider_request`：注册 before/after provider hook 的前置能力。
    worker.capability(ExtensionCapability::ProviderRequest);
    // `session_history`：覆盖 `astrcode.session.read_events`，用于读回缓存用量。
    worker.capability(ExtensionCapability::SessionHistory);

    worker.command(
        command(COMMAND_NAME)
            .description(
                "观测 provider 请求的 prompt 前缀，定位缓存断点：\
                 /cache-doctor status|doctor|stats|reset|enable|disable|config|help",
            )
            .build(),
        command_handler({
            let config = Arc::clone(&config);
            let watch = Arc::clone(&watch);
            move |ctx| {
                let config = Arc::clone(&config);
                let watch = Arc::clone(&watch);
                async move { dispatch(&config, &watch, &ctx).await }
            }
        }),
    )?;

    // 钩子注册借用 worker；单独放进作用域，注册完就释放，后续才能继续装配与运行。
    {
        let config = Arc::clone(&config);
        let watch = Arc::clone(&watch);
        worker.hook(
            LifecycleEvent::BeforeProviderRequest,
            HookMode::Blocking,
            provider_handler(move |input: ProviderHookInput, _ctx| {
                let config = Arc::clone(&config);
                let watch = Arc::clone(&watch);
                async move { Ok(observe(&watch, &config, &input)) }
            }),
        )?;
    }

    worker.run_stdio().await
}

/// 钩子的纯逻辑部分：观测一次请求，然后无条件放行。
///
/// 观测失败（锁中毒）不影响返回值——provider 请求路径上的钩子绝不能因为诊断而失败。
pub fn observe(
    watch: &WatchRegistry,
    config: &ConfigStore,
    input: &ProviderHookInput,
) -> ProviderResult {
    let settings = config.get();
    if settings.watch {
        watch.observe(&input.session_id, &input.messages, settings.history);
    }
    ProviderResult::Allow
}

/// 命令族。补全调用返回空补全而不是报错（未声明 `argument_completions`）。
async fn dispatch(
    config: &ConfigStore,
    watch: &WatchRegistry,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    match ctx.invocation() {
        WorkerCommandInvocation::Complete { .. } => Ok(command_result("", false, None)),
        WorkerCommandInvocation::Execute => execute(config, watch, ctx).await,
    }
}

async fn execute(
    config: &ConfigStore,
    watch: &WatchRegistry,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    let model_id = ctx.model().model.as_str();
    let session_id = ctx.session_id();
    let config_path = config.path().display().to_string();

    match parse_action(ctx.argument()) {
        Action::Help => Ok(command_result(&report::help_report(), false, None)),
        Action::Status => {
            let snapshot = watch.snapshot(session_id);
            let text = report::status_report(model_id, &config.get(), snapshot.as_ref(), &config_path);
            Ok(command_result(
                &text,
                false,
                Some(report::status_text(snapshot.as_ref())),
            ))
        },
        Action::Doctor => {
            let snapshot = watch.snapshot(session_id);
            let usage = scan_usage(session_id).await;
            let text = report::doctor_report(
                model_id,
                &config.get(),
                snapshot.as_ref(),
                usage.as_ref(),
            );
            Ok(command_result(
                &text,
                false,
                Some(report::status_text(snapshot.as_ref())),
            ))
        },
        Action::Stats => {
            let snapshot = watch.snapshot(session_id);
            let usage = scan_usage(session_id).await;
            let text = report::stats_report(&config.get(), snapshot.as_ref(), usage.as_ref());
            Ok(command_result(
                &text,
                false,
                Some(report::status_text(snapshot.as_ref())),
            ))
        },
        Action::Reset => {
            watch.reset(session_id);
            Ok(command_result(
                &format!(
                    "{}已清空本会话的观测计数；上一次请求的基线保留，下一次请求仍能正常对比。",
                    report::PREFIX
                ),
                false,
                Some(report::status_text(watch.snapshot(session_id).as_ref())),
            ))
        },
        Action::SetWatch(enabled) => {
            let note = write_config(config, |current| current.watch = enabled)?;
            Ok(command_result(
                &report::config_report(&config.get(), &config_path, Some(&note)),
                false,
                None,
            ))
        },
        Action::SetHistory(history) => {
            let note = write_config(config, |current| current.history = history)?;
            Ok(command_result(
                &report::config_report(&config.get(), &config_path, Some(&note)),
                false,
                None,
            ))
        },
        Action::ShowConfig => Ok(command_result(
            &report::config_report(&config.get(), &config_path, None),
            false,
            None,
        )),
        Action::Unknown(argument) => Ok(command_result(
            &format!(
                "{}未知参数 `{argument}`。可用：`/cache-doctor help`。",
                report::PREFIX
            ),
            true,
            None,
        )),
    }
}

/// 读取缓存用量；读取失败不阻断诊断，只在输出里如实说明。
///
/// 诊断的价值主要在前缀对比上，用量读不到（例如宿主没有 `session_history` 能力）
/// 不该让整条命令失败。
async fn scan_usage(session_id: &str) -> Option<UsageScan> {
    usage::scan_session(session_id).await.ok()
}

/// 写配置并返回给用户看的提示行。
fn write_config(
    config: &ConfigStore,
    mutate: impl FnOnce(&mut crate::config::Config),
) -> Result<String, ErrorPayload> {
    let mut next = config.get();
    mutate(&mut next);
    config
        .set(next)
        .map_err(|error| {
            ErrorPayload::new(
                WireErrorCode::IoError,
                format!("写入 {} 失败：{error}", config.path().display()),
            )
        })?;
    Ok(format!("已写入 {}", config.path().display()))
}

/// `/cache-doctor` 的子命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Help,
    Status,
    Doctor,
    Stats,
    Reset,
    SetWatch(bool),
    SetHistory(usize),
    ShowConfig,
    Unknown(String),
}

/// 解析子命令。大小写不敏感，多余空白忽略。
pub fn parse_action(argument: &str) -> Action {
    let mut parts = argument.split_whitespace();
    let head = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest: Vec<String> = parts.map(|part| part.to_ascii_lowercase()).collect();

    match head.as_str() {
        "" | "help" => Action::Help,
        "status" => Action::Status,
        "doctor" => Action::Doctor,
        "stats" => Action::Stats,
        "reset" => Action::Reset,
        "enable" => Action::SetWatch(true),
        "disable" => Action::SetWatch(false),
        "config" => match rest.first().map(String::as_str) {
            None => Action::ShowConfig,
            Some("watch") => match rest.get(1).map(String::as_str) {
                Some("on") => Action::SetWatch(true),
                Some("off") => Action::SetWatch(false),
                _ => Action::Unknown(argument.trim().to_string()),
            },
            Some("history") => match rest.get(1).and_then(|value| value.parse::<usize>().ok()) {
                Some(history) => Action::SetHistory(history),
                None => Action::Unknown(argument.trim().to_string()),
            },
            Some(_) => Action::Unknown(argument.trim().to_string()),
        },
        _ => Action::Unknown(argument.trim().to_string()),
    }
}

/// 构造 `ExtensionCommandResult::Display` 的线缆形状。
///
/// 宿主在 `session_command_service` 里把它反序列化成 `ExtensionCommandResult`：
/// `Display { content, is_error, status_update }`，带 `kind = "display"` 标签。
/// `status_update` 存在时宿主立刻下发一条 `StatusItemUpdate` 通知。
fn command_result(content: &str, is_error: bool, status: Option<String>) -> HandlerResult {
    let mut data = serde_json::json!({
        "kind": "display",
        "content": content,
        "is_error": is_error,
    });
    if let Some(text) = status {
        data["status_update"] = serde_json::json!({
            "id": report::STATUS_ITEM_ID,
            "text": text,
        });
    }
    HandlerResult::effect(HandlerEffect::Ok, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_argument_shows_help() {
        assert_eq!(parse_action(""), Action::Help);
        assert_eq!(parse_action("   "), Action::Help);
        assert_eq!(parse_action("HELP"), Action::Help);
    }

    #[test]
    fn the_read_only_subcommands_parse() {
        assert_eq!(parse_action("status"), Action::Status);
        assert_eq!(parse_action("doctor"), Action::Doctor);
        assert_eq!(parse_action("stats"), Action::Stats);
        assert_eq!(parse_action("reset"), Action::Reset);
        assert_eq!(parse_action("  Status  "), Action::Status);
    }

    #[test]
    fn the_toggles_parse_in_both_spellings() {
        assert_eq!(parse_action("enable"), Action::SetWatch(true));
        assert_eq!(parse_action("disable"), Action::SetWatch(false));
        assert_eq!(parse_action("config watch on"), Action::SetWatch(true));
        assert_eq!(parse_action("config watch off"), Action::SetWatch(false));
    }

    #[test]
    fn history_takes_a_number() {
        assert_eq!(parse_action("config history 7"), Action::SetHistory(7));
        assert_eq!(
            parse_action("config history 99999"),
            Action::SetHistory(99999),
            "越界值交给 Config::normalized 夹紧"
        );
    }

    #[test]
    fn a_bare_config_shows_the_current_values() {
        assert_eq!(parse_action("config"), Action::ShowConfig);
    }

    #[test]
    fn malformed_arguments_are_reported_verbatim() {
        assert_eq!(
            parse_action("config watch maybe"),
            Action::Unknown("config watch maybe".to_string())
        );
        assert_eq!(
            parse_action("config history"),
            Action::Unknown("config history".to_string())
        );
        assert_eq!(
            parse_action("config history abc"),
            Action::Unknown("config history abc".to_string())
        );
        assert_eq!(
            parse_action("config unknown"),
            Action::Unknown("config unknown".to_string())
        );
        assert_eq!(parse_action("wat"), Action::Unknown("wat".to_string()));
    }

    #[test]
    fn the_history_bound_is_advertised_in_help() {
        assert!(
            report::help_report().contains(&crate::config::MAX_HISTORY.to_string())
        );
    }

    #[test]
    fn command_result_matches_the_display_wire_shape() {
        let result = command_result("hello", false, None);
        assert_eq!(result.effect, HandlerEffect::Ok);
        assert_eq!(
            result.data,
            serde_json::json!({ "kind": "display", "content": "hello", "is_error": false })
        );
        assert!(result.data.get("status_update").is_none());
    }

    #[test]
    fn command_result_carries_the_status_update_when_present() {
        let result = command_result("hello", true, Some("prefix 11/12".to_string()));
        assert_eq!(result.data["kind"], "display");
        assert_eq!(result.data["is_error"], true);
        assert_eq!(result.data["status_update"]["id"], report::STATUS_ITEM_ID);
        assert_eq!(result.data["status_update"]["text"], "prefix 11/12");
    }
}
