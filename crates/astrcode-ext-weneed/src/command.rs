//! Worker 装配、`/weneed` 命令面。
//!
//! 四个钩子的实现都在 [`crate::hook`]；这里只负责把宿主事件接到它们上面，并提供命令面。
//! 命令输出是给人看的，用中文；模型可见的工件（规范正文、贴尾提醒、拦截原因里的替代用法）
//! 保持英文或原样，见仓库根 `AGENTS.md`。

use std::sync::Arc;

use astrcode_extension_worker::worker_prelude::*;

use astrcode_ext_common::config::EnsureOutcome;

use crate::{
    COMMAND_NAME, EXTENSION_ID,
    config::{self, Config, ReminderMode},
    hook, model,
    state::SharedState,
    toggle::SessionSwitch,
};

/// `/weneed` 的子命令及其说明。
const SUBCOMMANDS: [(&str, &str); 10] = [
    ("status", "显示模型判定、会话开关、全局配置与运行期统计"),
    ("on", "开启当前会话（覆盖此前写入的关闭）"),
    ("off", "关闭当前会话"),
    ("global", "全局总开关：/weneed global on|off"),
    (
        "reminder",
        "贴尾风格提醒节奏：/weneed reminder off|on-drift|always",
    ),
    ("drift", "是否读推理通道做漂移判定：/weneed drift on|off"),
    ("guard", "工具守卫：/weneed guard on|off"),
    ("block", "被拦工具名单：/weneed block [add|remove] <tool>"),
    ("reset", "恢复默认配置（全局配置与会话开关都重置）"),
    ("help", "显示本说明"),
];

/// 命令输出的统一前缀，让 `/weneed` 的回复在会话里可辨认。
const PREFIX: &str = "we need 模式：";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    // `provider_contribution` / `after_provider_response` 走 `provider_request`；
    // `pre_tool_use` 走 `tool_intercept`。
    worker.capability(ExtensionCapability::ProviderRequest);
    worker.capability(ExtensionCapability::ToolIntercept);

    let store = config::store();
    if let EnsureOutcome::Failed(error) = store.ensure_exists::<Config>() {
        eprintln!("{EXTENSION_ID}: {error}");
    }
    let (state, warning) = SharedState::load(store);
    if let Some(warning) = warning {
        eprintln!("{EXTENSION_ID}: {warning}");
    }

    let prompt_state = Arc::clone(&state);
    worker.on_prompt_build(prompt_build_handler(
        move |input: PromptBuildHookInput, _ctx| {
            let state = Arc::clone(&prompt_state);
            async move { hook::contribute(&state, &input).await }
        },
    ))?;

    let reminder_state = Arc::clone(&state);
    worker.on_provider_contribution(provider_contribution_handler(move |input, _ctx| {
        let state = Arc::clone(&reminder_state);
        async move { hook::plan_reminder(&state, &input).await }
    }))?;

    let observer_state = Arc::clone(&state);
    worker.on_after_provider_response(provider_handler(move |input, _ctx| {
        let state = Arc::clone(&observer_state);
        async move { hook::observe_reasoning(&state, &input).await }
    }))?;

    let guard_state = Arc::clone(&state);
    worker.on_pre_tool_use(pre_tool_use_handler(move |input, _ctx| {
        let state = Arc::clone(&guard_state);
        async move { hook::guard_tool_call(&state, &input).await }
    }))?;

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

    let command_state = Arc::clone(&state);
    worker.command(
        command(COMMAND_NAME)
            .description(
                "「we need」思维链规范：/weneed 开启本会话，/weneed status 查看状态，\
                 /weneed help 看全部子命令。仅在 DeepSeek 模型上生效。",
            )
            .argument_completions(true)
            .build(),
        command_handler(move |ctx| {
            let state = Arc::clone(&command_state);
            async move { dispatch(&state, &ctx).await }
        }),
    )?;

    worker.run_stdio().await
}

async fn dispatch(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    match ctx.invocation() {
        WorkerCommandInvocation::Complete { cursor } => {
            Ok(completions(argument_prefix(ctx.argument(), cursor)))
        }
        WorkerCommandInvocation::Execute => execute(state, ctx).await,
    }
}

async fn execute(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    let (head, tail) = split_once_whitespace(ctx.argument().trim());

    match head.to_ascii_lowercase().as_str() {
        // 无参等价于 `/weneed on`：这是本命令最早的行为，保持兼容。
        "" | "on" => set_session(state, ctx, SessionSwitch::On).await,
        "off" => set_session(state, ctx, SessionSwitch::Off).await,
        "global" => apply_toggle(state, ctx, &tail, Setting::Global).await,
        "drift" => apply_toggle(state, ctx, &tail, Setting::Drift).await,
        "guard" => apply_toggle(state, ctx, &tail, Setting::Guard).await,
        "reminder" => set_reminder(state, ctx, &tail).await,
        "block" => edit_blocklist(state, ctx, &tail).await,
        "status" => Ok(status(state, ctx).await),
        "reset" => reset(state, ctx).await,
        "help" => Ok(display(&render_help(), false)),
        other => Ok(display(
            &format!("{PREFIX}未知参数 `{other}`。\n\n{}", render_help()),
            true,
        )),
    }
}

/// `/weneed on|off`：写本会话的开关。
async fn set_session(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    switch: SessionSwitch,
) -> Result<HandlerResult, ErrorPayload> {
    if let Err(error) = state.set_session_switch(ctx.session_id(), switch).await {
        return Ok(display(&format!("{PREFIX}会话开关写入失败：{error}"), true));
    }
    let action = match switch {
        SessionSwitch::On => "已开启本会话。",
        SessionSwitch::Off => "已关闭本会话。",
        SessionSwitch::Unset => "已清除本会话开关，跟随全局配置。",
    };
    let note = effective_summary(state, ctx).await;
    Ok(display(&format!("{PREFIX}{action}\n{note}"), false))
}

/// 可被 `on|off` 修改的布尔配置项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting {
    Global,
    Drift,
    Guard,
}

impl Setting {
    const fn key(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Drift => "drift",
            Self::Guard => "guard",
        }
    }
}

/// `/weneed global|drift|guard on|off`：改一项全局布尔配置并落盘。
async fn apply_toggle(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    value: &str,
    setting: Setting,
) -> Result<HandlerResult, ErrorPayload> {
    let Some(enabled) = parse_on_off(value) else {
        return Ok(display(
            &format!(
                "{PREFIX}`{}` 只接受 on / off。用法：`/weneed {} on|off`。",
                setting.key(),
                setting.key()
            ),
            true,
        ));
    };

    let mut config = state.config();
    match setting {
        Setting::Global => config.enabled = enabled,
        Setting::Drift => config.drift_check = enabled,
        Setting::Guard => config.guard.enabled = enabled,
    }
    match state.set_config(config) {
        Err(error) => Ok(display(&format!("{PREFIX}配置保存失败：{error}"), true)),
        Ok(()) => {
            let value = if enabled { "on" } else { "off" };
            let note = effective_summary(state, ctx).await;
            Ok(display(
                &format!("{PREFIX}{} 已设为 {value}。\n{note}", setting.key()),
                false,
            ))
        }
    }
}

/// `/weneed reminder off|on-drift|always`。
async fn set_reminder(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    value: &str,
) -> Result<HandlerResult, ErrorPayload> {
    let reminder = match value.to_ascii_lowercase().as_str() {
        "off" => ReminderMode::Off,
        "on-drift" => ReminderMode::OnDrift,
        "always" => ReminderMode::Always,
        _ => {
            return Ok(display(
                &format!(
                    "{PREFIX}`reminder` 只接受 off / on-drift / always。\
                     用法：`/weneed reminder on-drift`。"
                ),
                true,
            ));
        }
    };

    let mut config = state.config();
    config.reminder = reminder;
    match state.set_config(config) {
        Err(error) => Ok(display(&format!("{PREFIX}配置保存失败：{error}"), true)),
        Ok(()) => {
            let note = effective_summary(state, ctx).await;
            Ok(display(
                &format!("{PREFIX}贴尾提醒已设为 {}。\n{note}", reminder.as_str()),
                false,
            ))
        }
    }
}

/// `/weneed block`：列出、增删被拦工具。
///
/// 工具名**大小写敏感**，因此这里不做小写化——工具名逐字相等才命中。
async fn edit_blocklist(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    rest: &str,
) -> Result<HandlerResult, ErrorPayload> {
    let (action, tool) = split_once_whitespace(rest);
    if action.is_empty() {
        let config = state.config();
        let list = render_blocklist(&config.guard.blocked_tools);
        return Ok(display(
            &format!(
                "{PREFIX}被拦工具：{list}\n\
                 增删用法：`/weneed block add <tool>`、`/weneed block remove <tool>`。\n\
                 工具名大小写敏感；增删后守卫开关不变（当前{}）。",
                on_off(config.guard.enabled)
            ),
            false,
        ));
    }
    if tool.is_empty() {
        return Ok(display(
            &format!(
                "{PREFIX}`block {action}` 缺少工具名。用法：`/weneed block {action} <tool>`。"
            ),
            true,
        ));
    }

    let mut config = state.config();
    let blocked = &mut config.guard.blocked_tools;
    match action.as_str() {
        "add" => {
            if blocked.iter().any(|name| name == &tool) {
                return Ok(display(
                    &format!("{PREFIX}`{tool}` 已经在被拦名单里。"),
                    false,
                ));
            }
            blocked.push(tool.clone());
        }
        "remove" => {
            let before = blocked.len();
            blocked.retain(|name| name != &tool);
            if blocked.len() == before {
                return Ok(display(
                    &format!("{PREFIX}`{tool}` 不在被拦名单里。"),
                    false,
                ));
            }
        }
        other => {
            return Ok(display(
                &format!("{PREFIX}未知操作 `{other}`。用法：`/weneed block add|remove <tool>`。"),
                true,
            ));
        }
    }

    let list = render_blocklist(&config.guard.blocked_tools);
    match state.set_config(config) {
        Err(error) => Ok(display(&format!("{PREFIX}配置保存失败：{error}"), true)),
        Ok(()) => {
            let note = effective_summary(state, ctx).await;
            Ok(display(&format!("{PREFIX}被拦工具：{list}\n{note}"), false))
        }
    }
}

/// `/weneed reset`：全局配置恢复默认，并清除本会话开关。
async fn reset(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    if let Err(error) = state.set_config(Config::default()) {
        return Ok(display(&format!("{PREFIX}配置重置失败：{error}"), true));
    }
    let switch_note = match state
        .set_session_switch(ctx.session_id(), SessionSwitch::Unset)
        .await
    {
        Ok(()) => "本会话开关已清除".to_owned(),
        Err(error) => format!("但会话开关清除失败：{error}"),
    };
    let note = effective_summary(state, ctx).await;
    Ok(display(
        &format!("{PREFIX}全局配置已恢复默认值，{switch_note}。\n{note}"),
        false,
    ))
}

/// `/weneed status`。
async fn status(state: &Arc<SharedState>, ctx: &WorkerCommandContext) -> HandlerResult {
    // 先重载，让用户刚手改过的 config.json 立刻反映到状态里。
    let warning = state.reload();
    let config = state.config();
    let stats = state.stats();
    let switch = state.session_switch(ctx.session_id()).await;

    let mut lines = vec![
        format!("{PREFIX}当前模型 {}", ctx.model().model),
        format!("  本会话开关：{}", switch_label(switch)),
        format!("  全局总开关：{}", on_off(config.enabled)),
        format!("  贴尾提醒：{}", config.reminder.as_str()),
        format!("  漂移判定：{}", on_off(config.drift_check)),
        format!(
            "  工具守卫：{}（被拦：{}）",
            on_off(config.guard.enabled),
            render_blocklist(&config.guard.blocked_tools)
        ),
        format!(
            "  运行期统计：注入 {} 轮，贴尾提醒 {} 次，观测漂移 {} 次，拦截 {} 次",
            stats.injections, stats.reminders, stats.drifts, stats.blocks
        ),
        format!("  配置文件：{}", state.store_path().display()),
        String::new(),
        effective_summary(state, ctx).await,
    ];
    if let Some(warning) = warning {
        lines.push(format!("  ⚠️ {warning}"));
    }
    display(&lines.join("\n"), false)
}

/// 当前是否真的会注入，以及原因。
async fn effective_summary(state: &Arc<SharedState>, ctx: &WorkerCommandContext) -> String {
    let model_id = ctx.model().model.as_str();
    if !model::is_deepseek(model_id) {
        return format!("当前模型 {model_id} 不是 DeepSeek：规范不会注入。");
    }
    if !state.config().enabled {
        return "全局总开关已关闭（`/weneed global on`）：规范不会注入。".to_owned();
    }
    match state.session_switch(ctx.session_id()).await {
        SessionSwitch::Off => "本会话开关已关闭（`/weneed on`）：规范不会注入。".to_owned(),
        SessionSwitch::Unset | SessionSwitch::On => {
            format!("当前模型 {model_id}，规范每轮注入 system prompt 的静态前缀区。")
        }
    }
}

fn render_blocklist(blocked: &[String]) -> String {
    if blocked.is_empty() {
        "（无）".to_owned()
    } else {
        blocked.join(", ")
    }
}

fn switch_label(switch: SessionSwitch) -> String {
    match switch {
        SessionSwitch::Unset => "未设置（跟随全局）".to_owned(),
        SessionSwitch::On => "已开启".to_owned(),
        SessionSwitch::Off => "已关闭".to_owned(),
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "开" } else { "关" }
}

fn parse_on_off(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

fn render_help() -> String {
    let mut lines = vec!["「we need」思维链规范用法：".to_owned()];
    for (name, description) in SUBCOMMANDS {
        lines.push(format!("  /weneed {name:<9} {description}"));
    }
    lines.push(String::new());
    lines.push(
        "触发条件是模型：只有激活模型是 DeepSeek 时才注入，非 DeepSeek 会话不会强行开启。"
            .to_owned(),
    );
    lines.join("\n")
}

/// 构造 `ExtensionCommandResult::Display` 的线缆形状。
///
/// 宿主在 `session_command_service` 里把它反序列化成 `ExtensionCommandResult`：
/// `Display { content, is_error, status_update }`，带 `kind = "display"` 标签。
fn display(content: &str, is_error: bool) -> HandlerResult {
    HandlerResult::effect(
        HandlerEffect::Ok,
        serde_json::json!({
            "kind": "display",
            "content": content,
            "is_error": is_error,
        }),
    )
}

/// 构造参数补全结果。
fn completions(prefix: String) -> HandlerResult {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let (head, tail) = split_once_whitespace(&prefix);

    if head.is_empty() {
        for (name, description) in SUBCOMMANDS {
            items.push(completion(name, name, description));
        }
    } else {
        let candidates: &[(&str, &str)] = match head.as_str() {
            "global" | "drift" | "guard" => &[("on", "开启"), ("off", "关闭")],
            "reminder" => &[
                ("off", "不贴提醒"),
                ("on-drift", "仅在漂移时贴"),
                ("always", "每请求都贴"),
            ],
            "block" => &[("add", "加入被拦名单"), ("remove", "移出被拦名单")],
            _ => &[],
        };
        for (name, description) in candidates {
            if tail.is_empty() || name.starts_with(&tail) {
                items.push(completion(name, name, description));
            }
        }
    }

    HandlerResult::effect(
        HandlerEffect::Ok,
        serde_json::json!({ "items": items, "truncated": false }),
    )
}

fn completion(label: &str, insert_text: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "label": label,
        "insert_text": insert_text,
        "detail": detail,
    })
}

/// 取光标之前的参数文本并转成小写做前缀匹配。
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
        Some(index) => (value[..index].to_owned(), value[index..].trim().to_owned()),
        None => (value.to_owned(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_the_command_wire_shape() {
        let result = display("hello", false);
        assert_eq!(result.effect, HandlerEffect::Ok);
        assert_eq!(
            result.data,
            serde_json::json!({ "kind": "display", "content": "hello", "is_error": false })
        );
        // 本插件不写状态栏：磁盘扩展无法注册 status item，这里也不做一次性更新。
        assert!(result.data.get("status_update").is_none());
    }

    #[test]
    fn display_marks_errors() {
        assert_eq!(display("bad", true).data["is_error"], true);
    }

    #[test]
    fn parse_on_off_only_accepts_on_and_off() {
        assert_eq!(parse_on_off("on"), Some(true));
        assert_eq!(parse_on_off("OFF"), Some(false));
        assert_eq!(parse_on_off(" off "), Some(false));
        assert_eq!(parse_on_off("yes"), None);
        assert_eq!(parse_on_off(""), None);
    }

    #[test]
    fn splitting_on_the_first_space_keeps_the_rest_whole() {
        assert_eq!(
            split_once_whitespace("block add my tool"),
            ("block".to_owned(), "add my tool".to_owned())
        );
        assert_eq!(
            split_once_whitespace("status"),
            ("status".to_owned(), String::new())
        );
        assert_eq!(
            split_once_whitespace("  status  "),
            (String::new(), "status".to_owned())
        );
    }

    #[test]
    fn the_argument_prefix_stops_at_the_cursor() {
        assert_eq!(argument_prefix("reminder al", 9), "reminder ");
        assert_eq!(argument_prefix("reminder al", 11), "reminder al");
        // 光标落在多字节字符中间时不 panic，回退到最近的字符边界。
        assert_eq!(argument_prefix("提醒", 1), "");
    }

    #[test]
    fn completions_offer_subcommands_then_their_values() {
        let items = completions("".to_owned());
        let labels: Vec<&str> = items.data["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect();
        assert!(labels.contains(&"status"), "{labels:?}");
        assert!(labels.contains(&"reminder"), "{labels:?}");

        let items = completions("reminder o".to_owned());
        let labels: Vec<&str> = items.data["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["off", "on-drift"]);
    }

    #[test]
    fn an_unknown_head_offers_no_completions() {
        let items = completions("nonsense ".to_owned());
        assert!(items.data["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn an_empty_blocklist_renders_explicitly() {
        assert_eq!(render_blocklist(&[]), "（无）");
        assert_eq!(
            render_blocklist(&["edit".to_owned(), "patch".to_owned()]),
            "edit, patch"
        );
    }

    /// 帮助文本必须覆盖每一个子命令，否则补全里会出现说明为空的项。
    #[test]
    fn every_subcommand_has_a_help_line() {
        let help = render_help();
        for (name, _) in SUBCOMMANDS {
            assert!(help.contains(name), "帮助缺少 {name}");
        }
    }
}
