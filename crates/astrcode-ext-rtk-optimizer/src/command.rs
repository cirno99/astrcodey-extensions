//! Worker 装配、`/rtk` 命令面与 `prompt_build` 提示注入。
//!
//! 上游的 `/rtk` 是「无参打开 TUI 设置弹窗 + 一组子命令」。S5R 没有 TUI 渲染 API，
//! 因此弹窗那一半换成 `/rtk set <key> <value>`：键名沿用上游的 setting id，
//! 取值沿用上游弹窗里给出的候选集合。
//!
//! 命令输出是给人看的，用中文；模型可见的压缩结果保持英文（那是被逐条对齐的移植产物，
//! 措辞不能改）。

use std::sync::Arc;

use astrcode_extension_sdk::s5r::hooks::PromptBuildHookInput;
use astrcode_extension_worker::worker_prelude::*;
use serde_json::json;

use crate::{
    COMMAND_NAME, EXTENSION_ID,
    config::{
        self, Config, EnsureOutcome, Mode, SMART_TRUNCATE_MAX_LINES_RANGE,
        SourceFilterLevel, TRUNCATE_MAX_CHARS_RANGE,
    },
    hook::{self, SharedState},
    metrics, rewrite,
};

/// `/rtk` 的子命令及其说明。
const SUBCOMMANDS: [(&str, &str); 8] = [
    ("show", "显示当前配置与运行期状态"),
    ("set", "在线修改一项配置：/rtk set <key> <value>"),
    ("path", "显示配置文件路径"),
    ("verify", "重新探测 rtk 可执行文件"),
    ("stats", "查看输出压缩节省统计"),
    ("clear-stats", "清空输出压缩节省统计"),
    ("reset", "恢复默认配置"),
    ("help", "显示用法"),
];

/// `/rtk set` 可修改的设置项，键名与上游 TUI 弹窗的 setting id 一致。
const SETTING_IDS: [&str; 20] = [
    "enabled",
    "mode",
    "showRewriteNotifications",
    "guardWhenRtkMissing",
    "outputCompactionEnabled",
    "outputStripAnsi",
    "outputReadCompactionEnabled",
    "outputTruncateEnabled",
    "outputTruncateMaxChars",
    "outputSourceFilteringEnabled",
    "outputPreserveExactSkillReads",
    "outputSourceFiltering",
    "outputSmartTruncate",
    "outputSmartTruncateMaxLines",
    "outputAggregateTestOutput",
    "outputFilterBuildOutput",
    "outputCompactGitOutput",
    "outputAggregateLinterOutput",
    "outputGroupSearchOutput",
    "outputTrackSavings",
];

/// 有损 `read` 压缩开启时注入的排查提示。
///
/// 上游让用户「在 Pi TUI 里跑 `/rtk` 关掉 Read compaction」；AstrCode 没有弹窗，
/// 对应的操作是 `/rtk set outputReadCompactionEnabled off`。
const SOURCE_FILTER_TROUBLESHOOTING_NOTE: &str = "RTK note: read compaction with source \
    filtering is active, so `read` output may have whole lines removed. If an edit repeatedly \
    fails because oldText does not match, run `/rtk set outputReadCompactionEnabled off`, \
    re-read the file, apply the edit, then re-enable it with `/rtk set \
    outputReadCompactionEnabled on`.";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    // `tool_intercept` 覆盖 blocking `post_tool_use` 与 `tool_input_transform`；
    // `process_spawn` 用于调用外部 `rtk rewrite`。
    worker.capability(ExtensionCapability::ToolIntercept);
    worker.capability(ExtensionCapability::ProcessSpawn);

    let store = config::store();
    if let EnsureOutcome::Failed(error) = store.ensure_exists::<Config>() {
        eprintln!("{EXTENSION_ID}: {error}");
    }

    let (state, warning) = SharedState::load(store);
    if let Some(warning) = warning {
        // stdout 专用于 S5R 帧；配置问题只能写 stderr。
        eprintln!("{EXTENSION_ID}: {warning}");
    }

    let transform_state = Arc::clone(&state);
    worker.on_tool_input_transform(tool_input_transform_handler(move |input, _ctx| {
        let state = Arc::clone(&transform_state);
        async move { hook::tool_input_transform(input, state).await }
    }))?;

    let post_state = Arc::clone(&state);
    worker.hook(
        LifecycleEvent::PostToolUse,
        HookMode::Blocking,
        post_tool_use_handler(move |input, _ctx| {
            let state = Arc::clone(&post_state);
            async move { hook::post_tool_use(input, state).await }
        }),
    )?;

    let prompt_state = Arc::clone(&state);
    worker.on_prompt_build(prompt_build_handler(
        move |input: PromptBuildHookInput, _ctx| {
            let state = Arc::clone(&prompt_state);
            async move { contribute(&state, &input).await }
        },
    ))?;

    let command_state = Arc::clone(&state);
    worker.command(
        command(COMMAND_NAME)
            .description(
                "RTK 优化器：/rtk 查看配置，/rtk set <key> <value> 修改，\
                 /rtk stats 看压缩统计，/rtk help 看全部子命令。",
            )
            .argument_completions(true)
            .build(),
        command_handler(move |ctx| {
            let state = Arc::clone(&command_state);
            async move { dispatch(&state, &ctx).await }
        }),
    )?;

    // 会话开始时重新读取配置，让外部手工编辑的 config.json 生效。
    let lifecycle_state = Arc::clone(&state);
    worker.hook(
        LifecycleEvent::SessionStart,
        HookMode::NonBlocking,
        Arc::new(move |_event, _ctx| {
            let state = Arc::clone(&lifecycle_state);
            Box::pin(async move {
                if let Some(warning) = state.reload() {
                    eprintln!("{EXTENSION_ID}: {warning}");
                }
                Ok(HandlerResult::ok())
            })
        }),
    )?;

    worker.run_stdio().await
}

/// `prompt_build` 贡献：有损 `read` 压缩开启时附上排查提示。
async fn contribute(
    state: &Arc<SharedState>,
    _input: &PromptBuildHookInput,
) -> Result<PromptContributions, ErrorPayload> {
    let config = state.config();
    if !should_inject_troubleshooting_note(&config) {
        return Ok(PromptContributions::default());
    }
    Ok(PromptContributions {
        additional_instructions: vec![SOURCE_FILTER_TROUBLESHOOTING_NOTE.to_owned()],
        ..Default::default()
    })
}

/// 是否该注入排查提示：`read` 压缩、源码过滤与某种截断同时开启。
fn should_inject_troubleshooting_note(config: &Config) -> bool {
    let compaction = &config.output_compaction;
    config.enabled
        && compaction.enabled
        && compaction.read_compaction.enabled
        && compaction.source_code_filtering_enabled
        && compaction.source_code_filtering != SourceFilterLevel::None
        && (compaction.smart_truncate.enabled || compaction.truncate.enabled)
}

async fn dispatch(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    match ctx.invocation() {
        WorkerCommandInvocation::Complete { cursor } => {
            Ok(completions(argument_prefix(ctx.argument(), cursor)))
        },
        WorkerCommandInvocation::Execute => Ok(run_subcommand(state, ctx.argument()).await),
    }
}

/// 执行一条 `/rtk` 子命令。
async fn run_subcommand(state: &Arc<SharedState>, argument: &str) -> HandlerResult {
    let (head, tail) = split_once_whitespace(argument.trim());

    match head.as_str() {
        "" | "show" => {
            state.reload();
            display(&render_summary(state), false)
        },
        "set" => run_set(state, &tail),
        "path" => display(
            &format!("RTK 优化器配置文件：{}", state.store_path().display()),
            false,
        ),
        "verify" => {
            let status = state.runtime().refresh().await;
            display(&render_verify(&state.config(), &status), !status.rtk_available)
        },
        "stats" => display(&metrics::summary(), false),
        "clear-stats" => {
            metrics::clear();
            display("RTK 压缩统计已清空。", false)
        },
        "reset" => display(&reset_settings(state), false),
        "help" => display(&render_help(), false),
        other => display(&format!("未知子命令 `{other}`。\n\n{}", render_help()), true),
    }
}

/// 处理 `/rtk set ...`。
fn run_set(state: &Arc<SharedState>, rest: &str) -> HandlerResult {
    let (key, value) = split_once_whitespace(rest);

    if key.is_empty() {
        return display(&render_setting_list(state), false);
    }
    if value.is_empty() {
        return match setting_value(&state.config(), &key) {
            Some(current) => display(&format!("{key} = {current}"), false),
            None => display(&unknown_setting_message(&key), true),
        };
    }

    match apply_setting(&state.config(), &key, &value) {
        Ok(next) => match state.set_config(next) {
            Ok(()) => {
                let current = setting_value(&state.config(), &key).unwrap_or_default();
                display(&format!("{key} = {current}"), false)
            },
            Err(error) => display(&error, true),
        },
        Err(error) => display(&error, true),
    }
}

/// 恢复默认配置；落盘失败时如实报告。
fn reset_settings(state: &Arc<SharedState>) -> String {
    match state.set_config(Config::default()) {
        Ok(()) => "RTK 优化器配置已恢复默认值。".to_owned(),
        Err(error) => error,
    }
}

/// 构造 `ExtensionCommandResult::Display` 的线缆形状。
fn display(content: &str, is_error: bool) -> HandlerResult {
    HandlerResult::effect(
        HandlerEffect::Ok,
        json!({ "kind": "display", "content": content, "is_error": is_error }),
    )
}

fn completions(prefix: String) -> HandlerResult {
    let mut items: Vec<serde_json::Value> = Vec::new();

    if let Some(rest) = prefix.strip_prefix("set ") {
        let rest = rest.trim();
        for id in SETTING_IDS {
            if rest.is_empty() || id.starts_with(rest) {
                items.push(json!({
                    "label": id,
                    "insert_text": id,
                    "detail": "配置项",
                }));
            }
        }
    } else {
        for (name, description) in SUBCOMMANDS {
            if prefix.is_empty() || name.starts_with(&prefix) {
                items.push(json!({
                    "label": name,
                    "insert_text": name,
                    "detail": description,
                }));
            }
        }
    }

    HandlerResult::effect(HandlerEffect::Ok, json!({ "items": items, "truncated": false }))
}

/// 取光标之前的参数文本，并转成小写做前缀匹配。
fn argument_prefix(argument: &str, cursor: usize) -> String {
    let end = cursor.min(argument.len());
    let end = (0..=end)
        .rev()
        .find(|index| argument.is_char_boundary(*index))
        .unwrap_or(0);
    argument[..end].trim_start().to_lowercase()
}

/// 按第一个空白切成两段；没有空白时第二段为空串。
fn split_once_whitespace(value: &str) -> (String, String) {
    match value.find(char::is_whitespace) {
        Some(index) => (
            value[..index].to_owned(),
            value[index..].trim().to_owned(),
        ),
        None => (value.to_owned(), String::new()),
    }
}

fn render_help() -> String {
    let mut lines = vec!["RTK 优化器用法：".to_owned()];
    for (name, description) in SUBCOMMANDS {
        lines.push(format!("  /rtk {name:<12} {description}"));
    }
    lines.push(String::new());
    lines.push("改写决策来自外部 `rtk rewrite`，本插件不维护改写规则表。".to_owned());
    lines.join("\n")
}

fn render_summary(state: &Arc<SharedState>) -> String {
    let config = state.config();
    let status = state.runtime().status();
    let compaction = &config.output_compaction;

    let mut lines = vec![
        "RTK 优化器".to_owned(),
        format!("  配置文件：{}", state.store_path().display()),
        format!(
            "  enabled={}  mode={}  rewriteNotice={}  guardWhenRtkMissing={}",
            config.enabled,
            config.mode.as_str(),
            config.show_rewrite_notifications,
            config.guard_when_rtk_missing
        ),
        format!(
            "  compaction={}  stripAnsi={}  readCompaction={}",
            compaction.enabled, compaction.strip_ansi, compaction.read_compaction.enabled
        ),
        format!(
            "  sourceFilterEnabled={}  preserveSkillReads={}  sourceFilter={}",
            compaction.source_code_filtering_enabled,
            compaction.preserve_exact_skill_reads,
            compaction.source_code_filtering.as_str()
        ),
        format!(
            "  truncate={} ({})  smartTruncate={} ({})",
            compaction.truncate.enabled,
            compaction.truncate.max_chars,
            compaction.smart_truncate.enabled,
            compaction.smart_truncate.max_lines
        ),
        format!(
            "  test={}  build={}  git={}  linter={}  search={}  trackSavings={}",
            compaction.aggregate_test_output,
            compaction.filter_build_output,
            compaction.compact_git_output,
            compaction.aggregate_linter_output,
            compaction.group_search_output,
            compaction.track_savings
        ),
        format!("  {}", render_runtime_status(&status)),
    ];

    if let Some(notice) = &status.last_notice {
        lines.push(format!("  最近一次改写：{notice}"));
    }

    lines.join("\n")
}

fn render_runtime_status(status: &rewrite::RuntimeStatus) -> String {
    let runtime = if status.rtk_available {
        "rtk=available".to_owned()
    } else {
        match &status.last_error {
            Some(error) => format!("rtk=missing ({error})"),
            None => "rtk=missing".to_owned(),
        }
    };

    match &status.executable {
        Some(executable) => match &executable.resolved_path {
            Some(path) => format!("{runtime}, rtkPath={path}"),
            None => match &executable.warning {
                Some(warning) => format!("{runtime}, rtkPath=unresolved ({warning})"),
                None => runtime,
            },
        },
        None => format!("{runtime}, rtkPath=未探测"),
    }
}

fn render_verify(config: &Config, status: &rewrite::RuntimeStatus) -> String {
    if status.rtk_available {
        let detail = status
            .executable
            .as_ref()
            .and_then(|executable| executable.resolved_path.clone())
            .map_or_else(String::new, |path| format!("，路径 {path}"));
        return format!("RTK 二进制可用{detail}。");
    }

    let reason = status
        .last_error
        .as_deref()
        .map_or_else(String::new, |error| format!("：{error}"));
    format!(
        "RTK 二进制不可用{reason}。\n\
         guardWhenRtkMissing={}，因此命令改写会被旁路，原始命令照常执行。",
        config.guard_when_rtk_missing
    )
}

fn render_setting_list(state: &Arc<SharedState>) -> String {
    let config = state.config();
    let mut lines = vec!["可修改的配置项（`/rtk set <key> <value>`）：".to_owned()];
    for id in SETTING_IDS {
        let value = setting_value(&config, id).unwrap_or_default();
        lines.push(format!("  {id} = {value}"));
    }
    lines.push(String::new());
    lines.push(
        "布尔项取值 on/off；mode 取 rewrite/suggest；sourceFilter 取 none/minimal/aggressive。"
            .to_owned(),
    );
    lines.join("\n")
}

fn unknown_setting_message(key: &str) -> String {
    format!("未知配置项 `{key}`。用 `/rtk set` 列出全部可修改项。")
}

/// 读取某个设置项的当前值。
fn setting_value(config: &Config, id: &str) -> Option<String> {
    let compaction = &config.output_compaction;
    let on_off = |value: bool| {
        if value {
            "on".to_owned()
        } else {
            "off".to_owned()
        }
    };

    Some(match id {
        "enabled" => on_off(config.enabled),
        "mode" => config.mode.as_str().to_owned(),
        "showRewriteNotifications" => on_off(config.show_rewrite_notifications),
        "guardWhenRtkMissing" => on_off(config.guard_when_rtk_missing),
        "outputCompactionEnabled" => on_off(compaction.enabled),
        "outputStripAnsi" => on_off(compaction.strip_ansi),
        "outputReadCompactionEnabled" => on_off(compaction.read_compaction.enabled),
        "outputTruncateEnabled" => on_off(compaction.truncate.enabled),
        "outputTruncateMaxChars" => compaction.truncate.max_chars.to_string(),
        "outputSourceFilteringEnabled" => on_off(compaction.source_code_filtering_enabled),
        "outputPreserveExactSkillReads" => on_off(compaction.preserve_exact_skill_reads),
        "outputSourceFiltering" => compaction.source_code_filtering.as_str().to_owned(),
        "outputSmartTruncate" => on_off(compaction.smart_truncate.enabled),
        "outputSmartTruncateMaxLines" => compaction.smart_truncate.max_lines.to_string(),
        "outputAggregateTestOutput" => on_off(compaction.aggregate_test_output),
        "outputFilterBuildOutput" => on_off(compaction.filter_build_output),
        "outputCompactGitOutput" => on_off(compaction.compact_git_output),
        "outputAggregateLinterOutput" => on_off(compaction.aggregate_linter_output),
        "outputGroupSearchOutput" => on_off(compaction.group_search_output),
        "outputTrackSavings" => on_off(compaction.track_savings),
        _ => return None,
    })
}

/// 把一项设置改成新值；越界或无法识别的取值返回可展示的错误。
fn apply_setting(config: &Config, id: &str, value: &str) -> Result<Config, String> {
    let mut next = config.clone();
    let compaction = &mut next.output_compaction;

    let on_off = |value: &str| match value {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("`{id}` 只接受 on / off，收到 `{other}`。")),
    };
    let integer = |value: &str, (min, max): (usize, usize)| {
        value
            .parse::<usize>()
            .ok()
            .filter(|parsed| (min..=max).contains(parsed))
            .ok_or_else(|| format!("`{id}` 只接受 {min}..={max} 的整数，收到 `{value}`。"))
    };

    match id {
        "enabled" => next.enabled = on_off(value)?,
        "mode" => {
            next.mode = match value {
                "rewrite" => Mode::Rewrite,
                "suggest" => Mode::Suggest,
                other => {
                    return Err(format!("`mode` 只接受 rewrite / suggest，收到 `{other}`。"));
                },
            }
        },
        "showRewriteNotifications" => next.show_rewrite_notifications = on_off(value)?,
        "guardWhenRtkMissing" => next.guard_when_rtk_missing = on_off(value)?,
        "outputCompactionEnabled" => compaction.enabled = on_off(value)?,
        "outputStripAnsi" => compaction.strip_ansi = on_off(value)?,
        "outputReadCompactionEnabled" => compaction.read_compaction.enabled = on_off(value)?,
        "outputTruncateEnabled" => compaction.truncate.enabled = on_off(value)?,
        "outputTruncateMaxChars" => {
            compaction.truncate.max_chars = integer(value, TRUNCATE_MAX_CHARS_RANGE)?;
        },
        "outputSourceFilteringEnabled" => {
            compaction.source_code_filtering_enabled = on_off(value)?;
        },
        "outputPreserveExactSkillReads" => {
            compaction.preserve_exact_skill_reads = on_off(value)?;
        },
        "outputSourceFiltering" => {
            compaction.source_code_filtering = match value {
                "none" => SourceFilterLevel::None,
                "minimal" => SourceFilterLevel::Minimal,
                "aggressive" => SourceFilterLevel::Aggressive,
                other => {
                    return Err(format!(
                        "`outputSourceFiltering` 只接受 none / minimal / aggressive，收到 \
                         `{other}`。"
                    ));
                },
            };
        },
        "outputSmartTruncate" => compaction.smart_truncate.enabled = on_off(value)?,
        "outputSmartTruncateMaxLines" => {
            compaction.smart_truncate.max_lines = integer(value, SMART_TRUNCATE_MAX_LINES_RANGE)?;
        },
        "outputAggregateTestOutput" => compaction.aggregate_test_output = on_off(value)?,
        "outputFilterBuildOutput" => compaction.filter_build_output = on_off(value)?,
        "outputCompactGitOutput" => compaction.compact_git_output = on_off(value)?,
        "outputAggregateLinterOutput" => compaction.aggregate_linter_output = on_off(value)?,
        "outputGroupSearchOutput" => compaction.group_search_output = on_off(value)?,
        "outputTrackSavings" => compaction.track_savings = on_off(value)?,
        _ => return Err(unknown_setting_message(id)),
    }

    Ok(next)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_state(name: &str) -> (Arc<SharedState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-rtk-command-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let (state, warning) = SharedState::load(config::store_at(dir.join("config.json")));
        assert!(warning.is_none());
        (state, dir)
    }

    #[test]
    fn setting_values_cover_every_settable_id() {
        let config = Config::default();
        for id in SETTING_IDS {
            assert!(
                setting_value(&config, id).is_some(),
                "setting id {id} has no renderer"
            );
        }
        assert!(setting_value(&config, "nope").is_none());
    }

    #[test]
    fn applying_a_setting_matches_the_renderer() {
        for (id, value) in [
            ("enabled", "off"),
            ("mode", "suggest"),
            ("outputStripAnsi", "off"),
            ("outputReadCompactionEnabled", "on"),
            ("outputSourceFiltering", "aggressive"),
            ("outputSmartTruncate", "on"),
            ("outputSmartTruncateMaxLines", "500"),
            ("outputTruncateMaxChars", "20000"),
            ("outputGroupSearchOutput", "off"),
        ] {
            let next = apply_setting(&Config::default(), id, value).unwrap();
            assert_eq!(setting_value(&next, id).as_deref(), Some(value), "id = {id}");
        }
    }

    #[test]
    fn rejecting_an_unknown_setting_lists_the_keys() {
        let error = apply_setting(&Config::default(), "nope", "on").unwrap_err();
        assert!(error.contains("未知配置项"), "{error}");
        assert!(error.contains("/rtk set"), "{error}");
    }

    #[test]
    fn rejecting_invalid_values_names_the_accepted_set() {
        let error = apply_setting(&Config::default(), "mode", "auto").unwrap_err();
        assert!(error.contains("rewrite / suggest"), "{error}");

        let error = apply_setting(&Config::default(), "enabled", "yes").unwrap_err();
        assert!(error.contains("on / off"), "{error}");

        let error = apply_setting(&Config::default(), "outputTruncateMaxChars", "5").unwrap_err();
        assert!(error.contains("1000..=200000"), "{error}");

        let error =
            apply_setting(&Config::default(), "outputSmartTruncateMaxLines", "9999").unwrap_err();
        assert!(error.contains("40..=4000"), "{error}");

        let error =
            apply_setting(&Config::default(), "outputSourceFiltering", "extreme").unwrap_err();
        assert!(error.contains("none / minimal / aggressive"), "{error}");
    }

    #[test]
    fn completions_offer_subcommands_then_setting_ids() {
        let result = completions(String::new());
        let items = result.data["items"].as_array().unwrap();
        assert_eq!(items.len(), SUBCOMMANDS.len());
        assert_eq!(items[0]["label"], "show");

        let result = completions("st".to_owned());
        let items = result.data["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["label"], "stats");

        let result = completions("c".to_owned());
        let items = result.data["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["label"], "clear-stats");

        let result = completions("set outputTruncate".to_owned());
        let items = result.data["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["label"], "outputTruncateEnabled");
        assert_eq!(items[1]["label"], "outputTruncateMaxChars");

        // 未知前缀不返回任何候选项。
        let result = completions("zzz".to_owned());
        assert!(result.data["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn argument_prefix_is_taken_up_to_the_cursor() {
        assert_eq!(argument_prefix("stats extra", 5), "stats");
        assert_eq!(argument_prefix("STATS", 5), "stats");
        assert_eq!(argument_prefix("stats", 100), "stats");
        assert_eq!(argument_prefix("", 0), "");
    }

    #[test]
    fn argument_prefix_respects_character_boundaries() {
        // 光标落在多字节字符中间时不能 panic，应回退到边界。
        assert_eq!(argument_prefix("设置", 1), "");
        assert_eq!(argument_prefix("设置", 3), "设");
        assert_eq!(argument_prefix("设置", 6), "设置");
    }

    #[test]
    fn splitting_on_the_first_whitespace_keeps_the_rest_intact() {
        assert_eq!(
            split_once_whitespace("set mode suggest"),
            ("set".to_owned(), "mode suggest".to_owned())
        );
        assert_eq!(
            split_once_whitespace("show"),
            ("show".to_owned(), String::new())
        );
        assert_eq!(split_once_whitespace(""), (String::new(), String::new()));
    }

    #[test]
    fn troubleshooting_note_requires_lossy_read_compaction() {
        let mut config = Config::default();
        assert!(!should_inject_troubleshooting_note(&config));

        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.source_code_filtering_enabled = true;
        config.output_compaction.source_code_filtering = SourceFilterLevel::Minimal;
        assert!(should_inject_troubleshooting_note(&config));

        config.output_compaction.truncate.enabled = false;
        config.output_compaction.smart_truncate.enabled = false;
        assert!(!should_inject_troubleshooting_note(&config));

        config.output_compaction.smart_truncate.enabled = true;
        config.enabled = false;
        assert!(!should_inject_troubleshooting_note(&config));
    }

    #[test]
    fn set_persists_the_change_and_reports_the_new_value() {
        let (state, dir) = temp_state("set-persists");

        let result = run_set(&state, "mode suggest");
        assert_eq!(result.data["content"], "mode = suggest");
        assert_eq!(result.data["is_error"], false);
        assert_eq!(state.config().mode, Mode::Suggest);

        let reloaded = config::store_at(dir.join("config.json")).load::<Config>().config;
        assert_eq!(reloaded.mode, Mode::Suggest);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn set_without_a_value_lists_the_settings() {
        let (state, dir) = temp_state("set-list");
        let result = run_set(&state, "");
        let content = result.data["content"].as_str().unwrap();
        assert!(content.contains("outputTrackSavings = on"), "{content}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn set_with_a_key_only_reports_its_value() {
        let (state, dir) = temp_state("set-read");
        let result = run_set(&state, "outputReadCompactionEnabled");
        assert_eq!(result.data["content"], "outputReadCompactionEnabled = off");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn set_reports_invalid_values_as_errors() {
        let (state, dir) = temp_state("set-invalid");
        let result = run_set(&state, "mode auto");
        assert_eq!(result.data["is_error"], true);
        assert_eq!(state.config().mode, Mode::Rewrite);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reset_restores_defaults() {
        let (state, dir) = temp_state("reset");
        run_set(&state, "enabled off");
        assert!(!state.config().enabled);

        let message = reset_settings(&state);
        assert!(message.contains("已恢复默认值"), "{message}");
        assert!(state.config().enabled);

        let reloaded = config::store_at(dir.join("config.json")).load::<Config>().config;
        assert!(reloaded.enabled);
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn show_renders_the_documented_keys() {
        let (state, dir) = temp_state("show");
        let result = run_subcommand(&state, "show").await;
        let summary = result.data["content"].as_str().unwrap();
        for expected in [
            "enabled=true",
            "mode=rewrite",
            "compaction=true",
            "readCompaction=false",
            "sourceFilter=none",
            "truncate=true (12000)",
            "rtk=missing",
        ] {
            assert!(summary.contains(expected), "missing {expected} in {summary}");
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_unknown_subcommand_reports_an_error_and_the_help() {
        let (state, dir) = temp_state("unknown-subcommand");
        let result = run_subcommand(&state, "frobnicate").await;
        assert_eq!(result.data["is_error"], true);
        let content = result.data["content"].as_str().unwrap();
        assert!(content.contains("未知子命令"), "{content}");
        assert!(content.contains("clear-stats"), "{content}");
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn path_reports_the_config_location() {
        let (state, dir) = temp_state("path");
        let result = run_subcommand(&state, "path").await;
        let content = result.data["content"].as_str().unwrap();
        assert!(content.contains("config.json"), "{content}");
        let _ = fs::remove_dir_all(dir);
    }

    /// 全局统计是进程级共享状态；用独有的工具名做标记，因此与其它并发测试互不干扰。
    #[tokio::test]
    async fn clear_stats_removes_recorded_metrics() {
        let (state, dir) = temp_state("clear-stats");
        metrics::record("marker-command-clear", 100, 50, &[]);
        assert!(metrics::summary().contains("marker-command-clear"));

        let result = run_subcommand(&state, "clear-stats").await;
        assert_eq!(result.data["is_error"], false);
        assert!(!metrics::summary().contains("marker-command-clear"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn help_lists_every_subcommand() {
        let help = render_help();
        for (name, _) in SUBCOMMANDS {
            assert!(help.contains(name), "help is missing {name}");
        }
    }

    #[test]
    fn verify_reports_a_missing_binary_with_an_explanation() {
        let status = rewrite::RuntimeStatus::default();
        let text = render_verify(&Config::default(), &status);
        assert!(text.contains("不可用"), "{text}");
        assert!(text.contains("guardWhenRtkMissing=true"), "{text}");
        assert!(text.contains("旁路"), "{text}");
    }
}
