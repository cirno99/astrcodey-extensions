//! Worker 装配、`/sleep` 命令面。
//!
//! 钩子的实现都在 [`crate::hook`]；这里只负责把宿主事件接到它们上面，并提供命令面。
//! 命令输出是给人看的，用中文；模型可见的工件（续跑文本、拦截原因）由配置与
//! [`crate::plan`] 生成，见仓库根 `AGENTS.md`。

use std::sync::Arc;

use astrcode_extension_sdk::extension::ContinueAfterStopOptions;
use astrcode_extension_worker::worker_prelude::*;

use astrcode_ext_common::config::EnsureOutcome;

use crate::{
    COMMAND_NAME, EXTENSION_ID,
    config::{self, Config, MAX_CONTINUE_TEXT_BYTES},
    hook,
    state::{SessionSwitch, SharedState},
};

/// `/sleep` 的子命令及其说明。
const SUBCOMMANDS: [(&str, &str); 10] = [
    ("on", "开启本会话的无人值守续跑（预算归零）"),
    ("off", "关闭本会话的续跑（全局配置不受影响）"),
    ("set", "设置续跑文本并开启：/sleep set 继续按计划推进"),
    ("max", "单次人工 turn 的续跑上限：/sleep max 100"),
    ("idle", "空转熔断阈值，0 表示关闭：/sleep idle 3"),
    ("answer", "提问是否自动应答：/sleep answer on|off"),
    ("tools", "自动应答的工具名单：/sleep tools [add|remove] askUser"),
    ("status", "查看开关、预算、统计与最近一次停止原因"),
    ("reset", "恢复默认配置并清除本会话开关"),
    ("help", "显示本说明"),
];

/// 命令输出的统一前缀，让 `/sleep` 的回复在会话里可辨认。
const PREFIX: &str = "睡眠续跑：";

/// 状态栏条目 id。
///
/// 磁盘 s5r 插件**无法注册**状态栏条目（S5R 的 `InitializeManifest` 没有 `status_items`
/// 字段），只能在命令结果里携带 `status_update`。宿主不要求该 id 预先注册：前端
/// `applyDelta` 与 CLI `handle_event` 都直接按 id 写入渲染表。因此那一格**只在用户敲过
/// 至少一次 `/sleep` 之后**才出现，且不会每轮自动刷新。
const STATUS_ITEM_ID: &str = "sleep";

/// 月亮标记，与上游 pi 插件保持一致。
const MOON: &str = "🌙";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    // `continue_after_stop` 走 `turn_continuation_control`；
    // `pre_tool_use` 走 `tool_intercept`；`defer_context` 与 `session_state` 需要
    // `session_control`（宿主只在扩展声明了它时才把 `session_ops` 注入调用上下文）。
    worker.capability(ExtensionCapability::TurnContinuationControl);
    worker.capability(ExtensionCapability::SessionControl);
    worker.capability(ExtensionCapability::ToolIntercept);

    let store = config::store();
    if let EnsureOutcome::Failed(error) = store.ensure_exists::<Config>() {
        eprintln!("{EXTENSION_ID}: {error}");
    }
    let (state, warning) = SharedState::load(store);
    if let Some(warning) = warning {
        eprintln!("{EXTENSION_ID}: {warning}");
    }

    // 核心：模型自然停下后再跑一个 step。每 turn 的上限由插件自己判定（`config.max`），
    // 所以这里声明 `unlimited`——用宿主的上限会让到顶之后连 handler 都不再被调用，
    // 插件也就没机会把「为什么停了」记下来告诉用户。
    let continue_state = Arc::clone(&state);
    worker.on_continue_after_stop(
        ContinueAfterStopOptions::unlimited(),
        continue_after_stop_handler(move |input, ctx| {
            let state = Arc::clone(&continue_state);
            async move { hook::continue_run(&state, &input, &ctx).await }
        }),
    )?;

    let answer_state = Arc::clone(&state);
    worker.on_pre_tool_use(pre_tool_use_handler(move |input, _ctx| {
        let state = Arc::clone(&answer_state);
        async move { hook::answer_question(&state, &input).await }
    }))?;

    // 工具活动只是观测：不拦任何调用，也不改任何结果。
    let activity_state = Arc::clone(&state);
    worker.hook(
        LifecycleEvent::PostToolUse,
        HookMode::NonBlocking,
        post_tool_use_handler(move |input, _ctx| {
            let state = Arc::clone(&activity_state);
            async move { Ok(hook::note_tool_call(&state, &input)) }
        }),
    )?;

    // 人工接手：续跑预算归零。插件自己注入的消息走 turn 内吸收，不会派发这个事件。
    let prompt_state = Arc::clone(&state);
    worker.hook(
        LifecycleEvent::UserPromptSubmit,
        HookMode::NonBlocking,
        Arc::new(move |event, _ctx| {
            let state = Arc::clone(&prompt_state);
            Box::pin(async move {
                hook::reset_budget_on_prompt(&state, &event);
                Ok(HandlerResult::ok())
            })
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
                if let Some(warning) = hook::reload_config(&state) {
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
                "无人值守续跑：/sleep 开启本会话，模型每次停下后自动注入一条「继续」；\
                 /sleep status 看状态，/sleep help 看全部子命令。",
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
        },
        WorkerCommandInvocation::Execute => execute(state, ctx).await,
    }
}

async fn execute(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    let (head, tail) = split_once_whitespace(ctx.argument().trim());

    match head.to_ascii_lowercase().as_str() {
        // 无参等价于 `/sleep on`：与上游 pi 插件的 `/sleep-on` 一样，一个命令就能开工。
        "" | "on" => Ok(enable(state, ctx, &tail).await),
        "off" => Ok(deactivate(state, ctx).await),
        "set" => Ok(set_text(state, ctx, &tail).await),
        "max" => Ok(set_max(state, ctx, &tail).await),
        "idle" => Ok(set_idle(state, ctx, &tail).await),
        "answer" => Ok(set_answer(state, ctx, &tail).await),
        "tools" => Ok(edit_tools(state, ctx, &tail).await),
        "status" => Ok(status(state, ctx).await),
        "reset" => Ok(reset(state, ctx).await),
        "help" => Ok(display(&render_help(), false)),
        other => Ok(display(
            &format!("{PREFIX}未知参数 `{other}`。\n\n{}", render_help()),
            true,
        )),
    }
}

/// `/sleep on [续跑文本]`：开启本会话，可选地同时换掉续跑文本。
async fn enable(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    text: &str,
) -> HandlerResult {
    if !text.is_empty()
        && let Err(message) = apply_continue_text(state, text)
    {
        return display(&format!("{PREFIX}{message}"), true);
    }
    let config = state.config();
    let content = format!(
        "{PREFIX}已开启。模型每次停下后注入「{}」（单次 turn 上限 {} 次）。\n\
         {MOON} 再次输入任意消息会重置预算；`/sleep off` 停用。",
        config.continue_text, config.max
    );
    enable_with(state, ctx, &content).await
}

/// 写会话开关，把宿主的错误转成可展示的文本。
///
/// 唯一的写入点：读路径有内存缓存，写路径保持一致才不会让缓存与宿主的落盘状态分叉。
async fn write_switch(
    state: &Arc<SharedState>,
    session_id: &str,
    switch: SessionSwitch,
) -> Result<(), String> {
    state
        .set_session_switch(session_id, switch)
        .await
        .map_err(|error| error.to_string())
}

/// 开启本会话、把预算归零，然后输出 `content` 并刷新状态栏。
///
/// `/sleep on` 的语义是「从现在开始，这一轮给你 max 次」：预算必须归零，否则上一次
/// 跑剩的计数会立刻把刚开启的会话顶到上限。
async fn enable_with(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    content: &str,
) -> HandlerResult {
    if let Err(message) = write_switch(state, ctx.session_id(), SessionSwitch::On).await {
        return display(&format!("{PREFIX}会话开关写入失败：{message}"), true);
    }
    state.reset_budget(ctx.session_id());
    refresh(state, ctx, content, false).await
}

/// `/sleep off`：关掉本会话。
async fn deactivate(state: &Arc<SharedState>, ctx: &WorkerCommandContext) -> HandlerResult {
    if let Err(message) = write_switch(state, ctx.session_id(), SessionSwitch::Off).await {
        return display(&format!("{PREFIX}会话开关写入失败：{message}"), true);
    }
    let content = format!("{PREFIX}已关闭本会话的续跑。\n{}", effective(state, ctx).await);
    refresh(state, ctx, &content, false).await
}

/// `/sleep set <文本>`：换文本并开启。
async fn set_text(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    text: &str,
) -> HandlerResult {
    if text.is_empty() {
        return display(
            &format!(
                "{PREFIX}`set` 缺少文本。用法：`/sleep set 继续按计划推进`。\
                 文本上限 {MAX_CONTINUE_TEXT_BYTES} 字节。"
            ),
            true,
        );
    }
    if let Err(message) = apply_continue_text(state, text) {
        return display(&format!("{PREFIX}{message}"), true);
    }
    let config = state.config();
    let content = format!(
        "{PREFIX}已开启，续跑文本改为「{}」（单次 turn 上限 {} 次）。",
        config.continue_text, config.max
    );
    enable_with(state, ctx, &content).await
}

/// 校验并落盘续跑文本。
fn apply_continue_text(state: &Arc<SharedState>, text: &str) -> Result<(), String> {
    if text.len() > MAX_CONTINUE_TEXT_BYTES {
        return Err(format!(
            "续跑文本过长（{} 字节，上限 {MAX_CONTINUE_TEXT_BYTES}）。",
            text.len()
        ));
    }
    let mut config = state.config();
    config.continue_text = text.to_owned();
    state
        .set_config(config)
        .map_err(|error| format!("配置保存失败：{error}"))
}

/// `/sleep max <n>`：单次人工 turn 的续跑上限。
async fn set_max(state: &Arc<SharedState>, ctx: &WorkerCommandContext, value: &str) -> HandlerResult {
    let Some(max) = parse_positive(value) else {
        return display(
            &format!("{PREFIX}`max` 只接受正整数。用法：`/sleep max 200`。"),
            true,
        );
    };
    let mut config = state.config();
    config.max = max;
    if let Err(error) = state.set_config(config) {
        return display(&format!("{PREFIX}配置保存失败：{error}"), true);
    }
    let content = format!("{PREFIX}单次 turn 的续跑上限已设为 {max}。");
    refresh(state, ctx, &content, false).await
}

/// `/sleep idle <n>`：空转熔断阈值，0 表示关闭。
async fn set_idle(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    value: &str,
) -> HandlerResult {
    let Some(idle) = parse_non_negative(value) else {
        return display(
            &format!("{PREFIX}`idle` 只接受非负整数。用法：`/sleep idle 3`，0 表示关闭。"),
            true,
        );
    };
    let mut config = state.config();
    config.idle_stop = idle;
    if let Err(error) = state.set_config(config) {
        return display(&format!("{PREFIX}配置保存失败：{error}"), true);
    }
    let content = if idle == 0 {
        format!("{PREFIX}空转熔断已关闭：模型不干活时也会一直续跑，直到预算用尽。")
    } else {
        format!("{PREFIX}空转熔断阈值已设为 {idle}：连续 {idle} 次续跑都没有工具调用就停下。")
    };
    refresh(state, ctx, &content, false).await
}

/// `/sleep answer on|off`：提问是否自动应答。
async fn set_answer(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    value: &str,
) -> HandlerResult {
    let Some(enabled) = parse_on_off(value) else {
        return display(
            &format!("{PREFIX}`answer` 只接受 on / off。用法：`/sleep answer off`。"),
            true,
        );
    };
    let mut config = state.config();
    config.answer.enabled = enabled;
    let tools = config.answer.tools.clone();
    if let Err(error) = state.set_config(config) {
        return display(&format!("{PREFIX}配置保存失败：{error}"), true);
    }
    let content = format!(
        "{PREFIX}提问自动应答已{}（工具：{}）。",
        on_off(enabled),
        render_tools(&tools)
    );
    refresh(state, ctx, &content, false).await
}

/// `/sleep tools`：列出、增删被自动应答的工具。
///
/// 工具名**大小写敏感**，因此这里不做小写化——工具名逐字相等才命中。
async fn edit_tools(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    rest: &str,
) -> HandlerResult {
    let (action, tool) = split_once_whitespace(rest);
    if action.is_empty() {
        let config = state.config();
        let content = format!(
            "{PREFIX}自动应答工具：{}\n\
             增删用法：`/sleep tools add <tool>`、`/sleep tools remove <tool>`。\n\
             工具名大小写敏感；增删后应答开关不变（当前{}）。",
            render_tools(&config.answer.tools),
            on_off(config.answer.enabled)
        );
        return refresh(state, ctx, &content, false).await;
    }
    if tool.is_empty() {
        return display(
            &format!("{PREFIX}`tools {action}` 缺少工具名。用法：`/sleep tools {action} <tool>`。"),
            true,
        );
    }

    let mut config = state.config();
    let tools = &mut config.answer.tools;
    match action.as_str() {
        "add" => {
            if tools.iter().any(|name| name == &tool) {
                return display(&format!("{PREFIX}`{tool}` 已经在名单里。"), false);
            }
            tools.push(tool.clone());
        },
        "remove" => {
            let before = tools.len();
            tools.retain(|name| name != &tool);
            if tools.len() == before {
                return display(&format!("{PREFIX}`{tool}` 不在名单里。"), false);
            }
        },
        other => {
            return display(
                &format!("{PREFIX}未知操作 `{other}`。用法：`/sleep tools add|remove <tool>`。"),
                true,
            );
        },
    }

    let list = render_tools(&config.answer.tools);
    if let Err(error) = state.set_config(config) {
        return display(&format!("{PREFIX}配置保存失败：{error}"), true);
    }
    let content = format!("{PREFIX}自动应答工具：{list}");
    refresh(state, ctx, &content, false).await
}

/// `/sleep reset`：全局配置恢复默认，并清除本会话开关。
async fn reset(state: &Arc<SharedState>, ctx: &WorkerCommandContext) -> HandlerResult {
    if let Err(error) = state.set_config(Config::default()) {
        return display(&format!("{PREFIX}配置重置失败：{error}"), true);
    }
    let switch_note = match state
        .set_session_switch(ctx.session_id(), SessionSwitch::Unset)
        .await
    {
        Ok(()) => "本会话开关已清除".to_owned(),
        Err(error) => format!("但会话开关清除失败：{error}"),
    };
    state.reset_budget(ctx.session_id());
    let content = format!("{PREFIX}全局配置已恢复默认值，{switch_note}。");
    refresh(state, ctx, &content, false).await
}

/// `/sleep status`。
async fn status(state: &Arc<SharedState>, ctx: &WorkerCommandContext) -> HandlerResult {
    // 先重载，让用户刚手改过的 config.json 立刻反映到状态里。
    let warning = state.reload();
    let config = state.config();
    let stats = state.stats();
    let switch = state.session_switch(ctx.session_id()).await;
    let progress = state.progress(ctx.session_id());

    let mut lines = vec![
        format!("{PREFIX}本会话开关 {}", switch.label()),
        effective(state, ctx).await,
        format!("  预算：{}/{}", progress.continuations, config.max),
        format!("  空转链：{}（阈值 {}）", progress.idle_streak, render_idle(config.idle_stop)),
        format!(
            "  最近停止原因：{}",
            progress
                .stop_reason
                .as_ref()
                .map_or_else(|| "无".to_owned(), |reason| reason.text())
        ),
        format!(
            "  提问自动应答：{}（工具：{}）",
            on_off(config.answer.enabled),
            render_tools(&config.answer.tools)
        ),
        format!("  续跑文本：「{}」", config.continue_text),
        format!(
            "  运行期统计：续跑 {} 次，自动应答 {} 次，空转熔断 {} 次",
            stats.continuations, stats.answers, stats.idle_stops
        ),
        format!("  配置文件：{}", state.store_path().display()),
    ];
    if let Some(warning) = warning {
        lines.push(format!("  ⚠️ {warning}"));
    }
    refresh(state, ctx, &lines.join("\n"), false).await
}

/// 当前会不会续跑，以及原因。`/sleep status` 与开关类命令都带上它。
async fn effective(state: &Arc<SharedState>, ctx: &WorkerCommandContext) -> String {
    let config = state.config();
    if !config.enabled {
        return "全局总开关已关闭（`/sleep reset` 可恢复）：不会续跑。".to_owned();
    }
    match state.session_switch(ctx.session_id()).await {
        SessionSwitch::On => format!(
            "续跑已开启：模型停下后注入「{}」，单次 turn 上限 {} 次。",
            config.continue_text, config.max
        ),
        SessionSwitch::Off => "本会话已显式关闭（`/sleep on` 可重新开启）：不会续跑。".to_owned(),
        SessionSwitch::Unset => {
            "本会话未开启过（`/sleep on` 开启）：不会续跑。".to_owned()
        },
    }
}

/// 状态栏那一格的文本。
///
/// 没开启时显示 `off` 而不是留空：状态栏是用户唯一能一眼确认「它到底开着没有」的地方。
async fn status_text(state: &Arc<SharedState>, ctx: &WorkerCommandContext) -> String {
    if !state.session_enabled(ctx.session_id()).await {
        return format!("{MOON} off");
    }
    let config = state.config();
    let progress = state.progress(ctx.session_id());
    format!("{MOON} {}/{}", progress.continuations, config.max)
}

fn render_tools(tools: &[String]) -> String {
    if tools.is_empty() {
        "（无）".to_owned()
    } else {
        tools.join(", ")
    }
}

fn render_idle(idle_stop: u32) -> String {
    if idle_stop == 0 {
        "关闭".to_owned()
    } else {
        idle_stop.to_string()
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

fn parse_positive(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok().filter(|number| *number > 0)
}

fn parse_non_negative(value: &str) -> Option<u32> {
    value.trim().parse::<u32>().ok()
}

fn render_help() -> String {
    let mut lines = vec!["无人值守续跑用法：".to_owned()];
    for (name, description) in SUBCOMMANDS {
        lines.push(format!("  /sleep {name:<7} {description}"));
    }
    lines.push(String::new());
    lines.push(
        "续跑只在**本会话**生效：开启后模型每次自然停下，插件就注入一条续跑消息再推一步。"
            .to_owned(),
    );
    lines.push(format!(
        "到达上限只停止续跑、开关保持开启——再次输入任意消息即重置预算。\
         连续多次续跑都没有工具调用时会触发空转熔断，最近原因见 `/sleep status`。{MOON}"
    ));
    lines.join("\n")
}

/// 构造 `ExtensionCommandResult::Display` 的线缆形状。
///
/// 宿主在 `session_command_service` 里把它反序列化成 `ExtensionCommandResult`：
/// `Display { content, is_error, status_update }`，带 `kind = "display"` 标签。
fn display(content: &str, is_error: bool) -> HandlerResult {
    display_with_status(content, is_error, None)
}

/// 带状态栏更新的命令输出。`status_update` 存在时宿主立刻下发一条 `StatusItemUpdate`。
fn display_with_status(content: &str, is_error: bool, status: Option<String>) -> HandlerResult {
    let mut data = serde_json::json!({
        "kind": "display",
        "content": content,
        "is_error": is_error,
    });
    if let Some(text) = status {
        data["status_update"] = serde_json::json!({
            "id": STATUS_ITEM_ID,
            "text": text,
        });
    }
    HandlerResult::effect(HandlerEffect::Ok, data)
}

/// 输出内容，并把状态栏那一格刷新到当前值。
async fn refresh(
    state: &Arc<SharedState>,
    ctx: &WorkerCommandContext,
    content: &str,
    is_error: bool,
) -> HandlerResult {
    let status = status_text(state, ctx).await;
    display_with_status(content, is_error, Some(status))
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
            "answer" => &[("on", "自动应答提问"), ("off", "让人自己选")],
            "tools" => &[("add", "加入名单"), ("remove", "移出名单")],
            "idle" => &[("0", "关闭空转熔断"), ("3", "连续 3 次无工具调用即停")],
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
        assert!(result.data.get("status_update").is_none());
    }

    #[test]
    fn display_marks_errors() {
        assert_eq!(display("bad", true).data["is_error"], true);
    }

    #[test]
    fn the_status_update_matches_the_wire_shape() {
        let result = display_with_status("hello", false, Some("🌙 3/100".to_owned()));
        assert_eq!(result.data["status_update"]["id"], STATUS_ITEM_ID);
        assert_eq!(result.data["status_update"]["text"], "🌙 3/100");
    }

    /// 状态栏 id 必须落在宿主接受的字符集内（与 session_state 键同一套规则）。
    #[test]
    fn the_status_item_id_is_safe() {
        assert!(!STATUS_ITEM_ID.is_empty());
        assert!(
            STATUS_ITEM_ID
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "状态栏 id 含可疑字符：{STATUS_ITEM_ID}"
        );
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
    fn the_cap_parser_rejects_zero_and_junk() {
        assert_eq!(parse_positive("200"), Some(200));
        assert_eq!(parse_positive(" 200 "), Some(200));
        assert_eq!(parse_positive("0"), None);
        assert_eq!(parse_positive("-1"), None);
        assert_eq!(parse_positive("many"), None);
        assert_eq!(parse_positive(""), None);
    }

    /// 熔断阈值 0 是合法输入：它表示「关闭熔断」。
    #[test]
    fn the_idle_parser_accepts_zero() {
        assert_eq!(parse_non_negative("0"), Some(0));
        assert_eq!(parse_non_negative("3"), Some(3));
        assert_eq!(parse_non_negative("-1"), None);
        assert_eq!(parse_non_negative("x"), None);
    }

    #[test]
    fn rendering_empty_lists_and_a_disabled_breaker_reads_clearly() {
        assert_eq!(render_tools(&[]), "（无）");
        assert_eq!(render_tools(&["askUser".to_owned()]), "askUser");
        assert_eq!(render_idle(0), "关闭");
        assert_eq!(render_idle(3), "3");
    }

    #[test]
    fn splitting_on_the_first_space_keeps_the_rest_whole() {
        assert_eq!(
            split_once_whitespace("tools add my tool"),
            ("tools".to_owned(), "add my tool".to_owned())
        );
        assert_eq!(
            split_once_whitespace("status"),
            ("status".to_owned(), String::new())
        );
    }

    /// `/sleep set 继续 按计划 推进` 的文本要保持整段，不能被再切一次。
    #[test]
    fn the_set_text_keeps_spaces() {
        let (head, tail) = split_once_whitespace("set 继续按计划推进，先修测试");
        assert_eq!(head, "set");
        assert_eq!(tail, "继续按计划推进，先修测试");
    }

    #[test]
    fn the_argument_prefix_stops_at_the_cursor() {
        assert_eq!(argument_prefix("tool", 2), "to");
        assert_eq!(argument_prefix("  Answer ", 6), "answ");
    }

    /// 光标落在多字节边界内时不能 panic 或切出半个字符。
    #[test]
    fn the_argument_prefix_respects_char_boundaries() {
        let argument = "中文";
        assert_eq!(argument_prefix(argument, 1), "");
        assert_eq!(argument_prefix(argument, 3), "中");
    }

    #[test]
    fn completions_offer_subcommands_then_their_values() {
        let items = completions(String::new());
        let labels = labels_of(&items);
        assert!(labels.contains(&"status"), "{labels:?}");
        assert!(labels.contains(&"answer"), "{labels:?}");

        let items = completions("answer o".to_owned());
        assert_eq!(labels_of(&items), vec!["on", "off"]);
    }

    #[test]
    fn an_unknown_head_offers_no_completions() {
        assert!(labels_of(&completions("nonsense ".to_owned())).is_empty());
    }

    fn labels_of(result: &HandlerResult) -> Vec<&str> {
        result.data["items"]
            .as_array()
            .expect("补全项应当是数组")
            .iter()
            .map(|item| item["label"].as_str().expect("label 应当是字符串"))
            .collect()
    }

    #[test]
    fn the_help_lists_every_subcommand() {
        let help = render_help();
        for (name, _) in SUBCOMMANDS {
            assert!(help.contains(name), "帮助里缺 {name}：{help}");
        }
    }
}
