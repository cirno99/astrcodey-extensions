//! Worker 装配与 `/smith` 命令面。
//!
//! 命令输出是给人看的，用中文；发给 enhancer 模型的英文工件在 [`crate::prompt`]。
//!
//! # 默认只预览
//!
//! `/smith <草稿>` 永远只做两件事：改写 + 暂存 + 把正文用围栏包好显示出来。**不自动发送**。
//! 发送是用户的显式动作，二选一：
//!
//! - `/smith go` —— 原样发出暂存的正文；
//! - 直接复制预览正文，自己编辑后再发（复制粘贴这条路不经过本插件，正文里不含任何插件注入的痕迹）。
//!
//! # 动词与草稿的歧义怎么消
//!
//! 用户的草稿是自由文本，第一个词完全可能是 `go`、`mode`、`show`（`/smith go run the tests`
//! 是想改写，不是要发送）。因此子命令只在**整个参数就是一个裸动词**、或**动词 + 一个合法取值**
//! 时才当命令解释，其余一律当草稿。判据在 [`Route`]，有单元测试钉住。

use astrcode_ext_common::config::EnsureOutcome;
use astrcode_extension_worker::worker_prelude::*;
use serde_json::json;

use crate::{
    COMMAND_NAME, EXTENSION_ID,
    config::{self, Config, Enhancer, Strength},
    enhance::{self, EnhanceError, Outcome},
    intent::RewriteMode,
    pending::{self, Pending},
};

/// 子命令表，顺序即 `help` 与补全的展示顺序。
const SUBCOMMANDS: [(&str, &str); 8] = [
    ("go", "原样发送上次预览的正文"),
    ("show", "重新显示上次预览的正文"),
    ("clear", "丢弃暂存的正文"),
    ("mode", "改写模式：/smith mode auto|plain|execution-contract"),
    ("strength", "改写力度：/smith strength light|balanced|strong"),
    ("enhancer", "改写档位：/smith enhancer small|main"),
    ("status", "显示当前配置与是否有暂存"),
    ("help", "显示本说明"),
];

/// 三种模式的中文说明，`mode` 与 `status`、补全共用。
const MODES: [(&str, &str); 3] = [
    ("auto", "由意图决定：解释类走 plain，其余走 execution-contract"),
    ("plain", "只做加强改写，不编成执行契约"),
    ("execution-contract", "编成可执行的紧凑任务契约"),
];

const STRENGTHS: [(&str, &str); 3] = [
    ("light", "只做措辞与结构收紧"),
    ("balanced", "默认：补范围、约束与验收"),
    ("strong", "尽量把隐含的执行要求都显式写出来"),
];

const ENHANCERS: [(&str, &str); 2] = [
    ("small", "默认：小模型，快且省；宿主没配小模型时自动回落 main"),
    ("main", "主模型：质量更稳，每条草稿多付一轮主模型延迟"),
];

/// 命令输出的统一前缀。
const PREFIX: &str = "Promptsmith：";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
///
/// 只注册命令，不注册任何 prompt / provider 钩子——本插件不往 system prompt 里塞东西，
/// 只在用户显式调用 `/smith` 时工作一次。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    // 改写要调模型（small 优先、main 兜底），暂存要走 session_state。
    worker.capability(ExtensionCapability::SmallModel);
    worker.capability(ExtensionCapability::MainModel);

    if let EnsureOutcome::Failed(error) = config::store().ensure_exists::<Config>() {
        eprintln!("{EXTENSION_ID}: {error}");
    }

    worker.command(
        command(COMMAND_NAME)
            .description(
                "提示词改写：/smith <草稿> 只预览不改发送，/smith go 才发出。\
                 /smith help 看全部子命令。",
            )
            .argument_completions(true)
            .build(),
        command_handler(|ctx| async move { dispatch(&ctx).await }),
    )?;

    worker.run_stdio().await
}

async fn dispatch(ctx: &WorkerCommandContext) -> Result<HandlerResult, ErrorPayload> {
    match ctx.invocation() {
        WorkerCommandInvocation::Complete { cursor } => {
            Ok(completions(ctx.argument(), cursor))
        },
        WorkerCommandInvocation::Execute => execute(ctx).await,
    }
}

async fn execute(ctx: &WorkerCommandContext) -> Result<HandlerResult, ErrorPayload> {
    let argument = ctx.argument().to_owned();
    let model_id = ctx.model().model.clone();
    // 每个命令都重读配置：读一个小 JSON 的成本远低于「用户手改了 config.json 却不生效」的困惑。
    let mut config = load_config();

    match route(&argument) {
        Route::Rewrite(draft) => rewrite(&draft, &config, &model_id).await,
        Route::Go => go().await,
        Route::Show => show(&config, &model_id).await,
        Route::Clear => clear().await,
        Route::Status => status(&config).await,
        Route::Help => Ok(display(&help_text(), false)),
        Route::SetMode(value) => {
            let Some(mode) = RewriteMode::parse(&value) else {
                return Ok(display(&invalid_value("mode", &value, &MODES), true));
            };
            config.mode = mode;
            save_and_confirm(&config, "mode", mode.as_str()).await
        },
        Route::SetStrength(value) => {
            let Some(strength) = Strength::parse(&value) else {
                return Ok(display(&invalid_value("strength", &value, &STRENGTHS), true));
            };
            config.strength = strength;
            save_and_confirm(&config, "strength", strength.as_str()).await
        },
        Route::SetEnhancer(value) => {
            let Some(enhancer) = Enhancer::parse(&value) else {
                return Ok(display(&invalid_value("enhancer", &value, &ENHANCERS), true));
            };
            config.enhancer = enhancer;
            save_and_confirm(&config, "enhancer", enhancer.as_str()).await
        },
        Route::Empty => Ok(display(&usage_hint(), false)),
    }
}

/// 一次改写：调模型、暂存、显示预览。
async fn rewrite(draft: &str, config: &Config, model_id: &str) -> Result<HandlerResult, ErrorPayload> {
    match enhance::enhance(draft, config, model_id).await {
        Err(EnhanceError(reason)) => Ok(display(&format!("{PREFIX}{reason}"), true)),
        Ok(outcome) => {
            let pending = Pending::from_outcome(draft, &outcome, config, model_id);
            // 暂存写不进去也要把预览给出去：正文已经在手上，复制粘贴这条路不受影响。
            // 只是 `/smith go` 会因此不可用，所以在预览里说明。
            let stored = pending::save(&pending).await.is_ok();
            Ok(display(&render_preview(&outcome, stored), false))
        },
    }
}

/// `/smith go`：把暂存的正文原样发出去。
async fn go() -> Result<HandlerResult, ErrorPayload> {
    let Some(pending) = pending::load().await else {
        return Ok(display(
            &format!("{PREFIX}没有可发送的暂存正文。先 `/smith <草稿>` 预览一次，再 `/smith go`。"),
            true,
        ));
    };
    // instructions 会整体作为用户消息进入 transcript，原始的 `/smith go` 文本不会留下痕迹。
    Ok(HandlerResult::effect(
        HandlerEffect::Ok,
        json!({ "kind": "start_turn", "instructions": pending.prompt }),
    ))
}

/// `/smith show`：重新显示暂存正文。
async fn show(config: &Config, model_id: &str) -> Result<HandlerResult, ErrorPayload> {
    let Some(pending) = pending::load().await else {
        return Ok(display(
            &format!("{PREFIX}本会话还没有暂存的正文。用 `/smith <草稿>` 改写一条。"),
            true,
        ));
    };
    Ok(display(&render_stored(&pending, config, model_id), false))
}

async fn clear() -> Result<HandlerResult, ErrorPayload> {
    match pending::clear().await {
        Ok(()) => Ok(display(&format!("{PREFIX}已丢弃暂存的正文。"), false)),
        Err(error) => Ok(display(
            &format!("{PREFIX}丢弃暂存失败：{error}（正文仍在，可再试一次）"),
            true,
        )),
    }
}

async fn status(config: &Config) -> Result<HandlerResult, ErrorPayload> {
    let stored = pending::load().await;
    let mut lines = vec![
        format!("{PREFIX}当前配置"),
        format!("  mode      {} —— {}", config.mode.as_str(), describe(&MODES, config.mode.as_str())),
        format!("  strength  {} —— {}", config.strength.as_str(), describe(&STRENGTHS, config.strength.as_str())),
        format!("  enhancer  {} —— {}", config.enhancer.as_str(), describe(&ENHANCERS, config.enhancer.as_str())),
        format!("  配置文件   {}", config::default_path().display()),
        String::new(),
    ];
    match stored {
        Some(pending) => {
            lines.push(format!("  暂存正文   {} 字，为 `{}` 改写", pending.prompt.chars().count(), pending.target_model));
            lines.push("             `/smith show` 回看，`/smith go` 发送，`/smith clear` 丢弃。".to_owned());
        },
        None => lines.push("  暂存正文   无".to_owned()),
    }
    Ok(display(&lines.join("\n"), false))
}

/// 落盘并把结果说成人话。
async fn save_and_confirm(config: &Config, key: &str, value: &str) -> Result<HandlerResult, ErrorPayload> {
    match config::store().save(config) {
        Ok(()) => Ok(display(&format!("{PREFIX}`{key}` 已设为 `{value}`（全局）。"), false)),
        Err(error) => Ok(display(
            &format!("{PREFIX}`{key}` 本次生效但落盘失败：{error}"),
            true,
        )),
    }
}

/// 读全局配置；解析失败已在 store 层回落默认值，这里只取归一化后的结果。
fn load_config() -> Config {
    let store = config::store();
    if let EnsureOutcome::Failed(error) = store.ensure_exists::<Config>() {
        eprintln!("{EXTENSION_ID}: {error}");
    }
    let loaded = store.load::<Config>();
    if let Some(warning) = loaded.warning {
        eprintln!("{EXTENSION_ID}: {warning}");
    }
    loaded.config
}

// ─── 参数路由 ────────────────────────────────────────────────────────────

/// 参数被解释成什么。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    /// 没有参数：给用法提示。
    Empty,
    /// 一条待改写的草稿。
    Rewrite(String),
    Go,
    Show,
    Clear,
    Status,
    Help,
    SetMode(String),
    SetStrength(String),
    SetEnhancer(String),
}

/// 把参数切成「子命令」或「草稿」。
///
/// 规则：**只有**下列形态才当子命令，其余全当草稿——
///
/// 1. 整个参数恰好是一个裸动词（`go` / `show` / `clear` / `status` / `help`）；
/// 2. 设置类动词后面跟**恰好一个**词，且那个词是该动词的合法取值（`mode plain`）。
///
/// 第 2 条里「必须是合法取值」很关键：`/smith mode 切成暗色主题` 是草稿，不是非法的取值命令。
fn route(argument: &str) -> Route {
    let trimmed = argument.trim();
    if trimmed.is_empty() {
        return Route::Empty;
    }

    // 按空白切词，但最多只看两个：第三个词出现就说明后面还有正文，那是草稿。
    let mut words = trimmed.split_whitespace();
    let head = words.next().unwrap_or_default();
    let second = words.next();
    let third = words.next();

    if second.is_none() && third.is_none() {
        return match head {
            "go" => Route::Go,
            "show" => Route::Show,
            "clear" => Route::Clear,
            "status" => Route::Status,
            "help" => Route::Help,
            // `/smith mode`（光杆）不当命令：给草稿用，让用户看见「mode 开头的草稿」
            // 仍然能改写。设置类的当前值由 `/smith status` 提供。
            _ => Route::Rewrite(trimmed.to_owned()),
        };
    }

    if third.is_none() {
        // `unwrap` 安全：上面已排除 `second.is_none()` 的分支。
        let value = second.unwrap();
        let set = match head {
            "mode" => RewriteMode::parse(value).map(|_| Route::SetMode(value.to_owned())),
            "strength" => Strength::parse(value).map(|_| Route::SetStrength(value.to_owned())),
            "enhancer" => Enhancer::parse(value).map(|_| Route::SetEnhancer(value.to_owned())),
            _ => None,
        };
        if let Some(route) = set {
            return route;
        }
    }

    Route::Rewrite(trimmed.to_owned())
}

// ─── 渲染 ────────────────────────────────────────────────────────────────

/// 预览正文：元信息一行 + 围栏包住的正文 + 发送步骤。
fn render_preview(outcome: &Outcome, stored: bool) -> String {
    let mut lines = vec![format!(
        "{PREFIX}已改写（意图 {} · 模式 {} · 档位 {}）",
        outcome.intent.as_str(),
        outcome.mode.as_str(),
        outcome.enhancer_used,
    )];

    if outcome.enhancer_fell_back {
        lines.push("  宿主没有可用的小模型，这次实际打的是主模型。".to_owned());
    }
    if let Some(note) = outcome.extraction.note() {
        lines.push(format!("  {note}"));
    }
    lines.push(String::new());

    let fence = fence_for(&outcome.prompt);
    lines.push(format!("{fence}text"));
    lines.push(outcome.prompt.clone());
    lines.push(fence);
    lines.push(String::new());

    if stored {
        lines.push("下一步（二选一）：".to_owned());
        lines.push("  · `/smith go` —— 原样发送上面这份正文".to_owned());
        lines.push("  · 直接复制上面正文，自己编辑后再发（不必经过插件）".to_owned());
    } else {
        lines.push("注意：暂存写入失败，`/smith go` 这次不可用。".to_owned());
        lines.push("请复制上面的正文自己编辑后发送。".to_owned());
    }
    lines.join("\n")
}

/// 重新显示暂存正文，措辞与首次预览区分开，并提示模型是否已经换过。
fn render_stored(pending: &Pending, _config: &Config, model_id: &str) -> String {
    let mut lines = vec![format!(
        "{PREFIX}上次改写的正文（意图 {} · 模式 {} · 档位 {}）",
        pending.intent, pending.mode, pending.enhancer
    )];
    if !pending.targets(model_id) {
        lines.push(format!(
            "  这份正文是为 `{}` 改写的，当前模型已换成 `{model_id}`，建议重新改写一次。",
            pending.target_model
        ));
    }
    lines.push(String::new());
    let fence = fence_for(&pending.prompt);
    lines.push(format!("{fence}text"));
    lines.push(pending.prompt.clone());
    lines.push(fence);
    lines.push(String::new());
    lines.push("  `/smith go` 原样发送，`/smith clear` 丢弃。".to_owned());
    lines.join("\n")
}

/// 挑一层比正文里最长的反引号串更长的围栏，避免正文里的 ``` 提前把围栏关掉。
fn fence_for(content: &str) -> String {
    let mut run = 0usize;
    let mut longest_run = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            run += 1;
            longest_run = longest_run.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest_run.max(2) + 1)
}

fn help_text() -> String {
    let mut lines = vec![
        "提示词改写用法：".to_owned(),
        format!("  /{COMMAND_NAME} <草稿>   改写并只预览，不自动发送"),
        String::new(),
        "  子命令：".to_owned(),
    ];
    for (name, description) in SUBCOMMANDS {
        lines.push(format!("  /{COMMAND_NAME} {name:<9} {description}"));
    }
    lines.extend([
        String::new(),
        "  草稿里以 go / mode / show 这类词开头没关系：只有「整个参数就是一个裸动词」".to_owned(),
        "  或「动词 + 一个合法取值」才当子命令，其余一律当草稿改写。".to_owned(),
        String::new(),
        "  改写默认打小模型；配置在 config.json，也可用 /smith mode|strength|enhancer 改。".to_owned(),
    ]);
    lines.join("\n")
}

fn usage_hint() -> String {
    format!(
        "{PREFIX}给我一条草稿才会改写：`/{COMMAND_NAME} <草稿>`。\n\
         只想要说明就用 `/{COMMAND_NAME} help`。"
    )
}

fn invalid_value(key: &str, given: &str, options: &[(&str, &str)]) -> String {
    let mut lines = vec![format!("{PREFIX}`{key}` 不认识 `{given}`。可用取值：")];
    for (name, description) in options {
        lines.push(format!("  {name:<18} {description}"));
    }
    lines.push(format!("例：`/{COMMAND_NAME} {key} {}`", options[0].0));
    lines.join("\n")
}

fn describe(options: &[(&'static str, &'static str)], name: &str) -> &'static str {
    options
        .iter()
        .find(|(option, _)| *option == name)
        .map(|(_, description)| *description)
        .unwrap_or("（无说明）")
}

// ─── 线缆形状 ─────────────────────────────────────────────────────────────

/// 构造 `ExtensionCommandResult::Display` 的线缆形状。
fn display(content: &str, is_error: bool) -> HandlerResult {
    HandlerResult::effect(
        HandlerEffect::Ok,
        json!({ "kind": "display", "content": content, "is_error": is_error }),
    )
}

/// 构造参数补全。动词补全只在第一个词还没写完时给；取值补全在动词后给。
fn completions(argument: &str, cursor: usize) -> HandlerResult {
    let prefix = argument_prefix(argument, cursor);
    let (head, tail) = split_once_whitespace(&prefix);

    let mut items: Vec<serde_json::Value> = Vec::new();
    if head.is_empty() {
        for (name, description) in SUBCOMMANDS {
            items.push(completion(name, name, description));
        }
    } else {
        let options: &[(&str, &str)] = match head.as_str() {
            "mode" => &MODES,
            "strength" => &STRENGTHS,
            "enhancer" => &ENHANCERS,
            _ => &[],
        };
        for (name, description) in options {
            if tail.is_empty() || name.starts_with(&tail) {
                items.push(completion(name, name, description));
            }
        }
    }

    HandlerResult::effect(HandlerEffect::Ok, json!({ "items": items, "truncated": false }))
}

fn completion(label: &str, insert_text: &str, detail: &str) -> serde_json::Value {
    json!({ "label": label, "insert_text": insert_text, "detail": detail })
}

/// 取光标之前的参数文本并转小写做前缀匹配。
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
    fn bare_verbs_route_to_subcommands() {
        assert_eq!(route("go"), Route::Go);
        assert_eq!(route("  show "), Route::Show);
        assert_eq!(route("clear"), Route::Clear);
        assert_eq!(route("status"), Route::Status);
        assert_eq!(route("help"), Route::Help);
        assert_eq!(route(""), Route::Empty);
    }

    /// 动词后面还有正文，那就是草稿——这是本命令面最容易做错的一处。
    #[test]
    fn verbs_with_trailing_text_route_to_rewrite() {
        assert_eq!(
            route("go run the tests and report failures"),
            Route::Rewrite("go run the tests and report failures".to_owned())
        );
        assert_eq!(
            route("mode 切换成暗色主题"),
            Route::Rewrite("mode 切换成暗色主题".to_owned())
        );
        assert_eq!(
            route("show me why the build fails"),
            Route::Rewrite("show me why the build fails".to_owned())
        );
    }

    #[test]
    fn settings_take_exactly_one_valid_value() {
        assert_eq!(route("mode plain"), Route::SetMode("plain".to_owned()));
        assert_eq!(
            route("strength balanced"),
            Route::SetStrength("balanced".to_owned())
        );
        assert_eq!(route("enhancer main"), Route::SetEnhancer("main".to_owned()));
    }

    /// 取值不合法就当草稿：用户完全可能写出以 mode / strength 开头的提示词。
    #[test]
    fn invalid_setting_values_fall_through_to_rewrite() {
        assert_eq!(
            route("mode turbo"),
            Route::Rewrite("mode turbo".to_owned()),
            "`mode turbo` 更像是草稿而不是打错的设置"
        );
        assert_eq!(
            route("enhancer huge"),
            Route::Rewrite("enhancer huge".to_owned())
        );
    }

    /// 光杆设置动词不是命令：当前值由 `/smith status` 给，`/smith mode` 该被当成草稿。
    #[test]
    fn bare_setting_verbs_are_drafts() {
        assert_eq!(route("mode"), Route::Rewrite("mode".to_owned()));
        assert_eq!(route("strength"), Route::Rewrite("strength".to_owned()));
        assert_eq!(route("enhancer"), Route::Rewrite("enhancer".to_owned()));
    }

    #[test]
    fn any_other_first_word_is_a_draft() {
        assert_eq!(route("帮我加个会话开关"), Route::Rewrite("帮我加个会话开关".to_owned()));
        assert_eq!(route("Go run it"), Route::Rewrite("Go run it".to_owned()), "大小写敏感：只有小写 go 是动词");
    }

    #[test]
    fn preview_fences_the_prompt_and_offers_both_send_paths() {
        let outcome = Outcome {
            prompt: "目标：加个开关\n\n验收：`cargo test` 通过".to_owned(),
            intent: crate::intent::TaskIntent::Implement,
            mode: crate::intent::EffectiveMode::ExecutionContract,
            enhancer_used: "small",
            enhancer_fell_back: false,
            extraction: enhance::Extraction::Sentinel,
        };
        let preview = render_preview(&outcome, true);
        assert!(preview.contains("意图 implement"));
        assert!(preview.contains("模式 execution-contract"));
        assert!(preview.contains("```text\n目标：加个开关"));
        assert!(preview.contains("/smith go"));
        assert!(preview.contains("复制上面正文"));
        // 理想提取形态不该啰嗦。
        assert!(!preview.contains("哨兵"));
    }

    #[test]
    fn preview_notes_fallback_and_malformed_extraction() {
        let outcome = Outcome {
            prompt: "目标：修好它".to_owned(),
            intent: crate::intent::TaskIntent::Debug,
            mode: crate::intent::EffectiveMode::ExecutionContract,
            enhancer_used: "main",
            enhancer_fell_back: true,
            extraction: enhance::Extraction::WholeResponse,
        };
        let preview = render_preview(&outcome, false);
        assert!(preview.contains("宿主没有可用的小模型"));
        assert!(preview.contains("没有哨兵块"));
        // 暂存写不进去时不能给出「二选一」的发送步骤，只能让用户自己复制。
        assert!(!preview.contains("下一步（二选一）"));
        assert!(preview.contains("请复制上面的正文"));
        assert!(preview.contains("暂存写入失败"));
    }

    #[test]
    fn stored_view_warns_when_the_model_changed() {
        let pending = Pending {
            prompt: "p".to_owned(),
            draft: "d".to_owned(),
            intent: "debug".to_owned(),
            mode: "execution-contract".to_owned(),
            strength: "balanced".to_owned(),
            enhancer: "small".to_owned(),
            enhancer_fell_back: false,
            extraction: "sentinel".to_owned(),
            target_model: "deepseek-v4.1-flash".to_owned(),
        };
        let same = render_stored(&pending, &Config::default(), "deepseek-v4.1-flash");
        assert!(!same.contains("建议重新改写"));
        let changed = render_stored(&pending, &Config::default(), "qwen-3.8-flash");
        assert!(changed.contains("deepseek-v4.1-flash"));
        assert!(changed.contains("qwen-3.8-flash"));
        assert!(changed.contains("建议重新改写"));
    }

    /// 正文里带代码围栏时不能提前关掉外层围栏，否则复制会带进 ``` 噪声。
    #[test]
    fn fence_sits_above_any_backtick_run_in_the_body() {
        assert_eq!(fence_for("plain body"), "```");
        assert_eq!(fence_for("a `code` run"), "```");
        let nested = fence_for("```rust\nfn main() {}\n```");
        assert_eq!(nested, "````");
        let preview = format!("{nested}text\n```rust\nfn main() {{}}\n```\n{nested}");
        // 外层围栏必须比内层长，内层的 ``` 才关不掉外层。
        assert!(preview.starts_with("````text"));
        assert!(preview.ends_with("````"));
    }

    #[test]
    fn invalid_value_lists_the_real_options() {
        let message = invalid_value("mode", "turbo", &MODES);
        assert!(message.contains("auto"));
        assert!(message.contains("execution-contract"));
        assert!(message.contains("/smith mode auto"));
    }

    #[test]
    fn describe_falls_back_for_unknown_names() {
        assert_eq!(describe(&MODES, "plain"), MODES[1].1);
        assert_eq!(describe(&ENHANCERS, "nope"), "（无说明）");
    }

    #[test]
    fn help_lists_every_subcommand() {
        let help = help_text();
        for (name, _) in SUBCOMMANDS {
            assert!(help.contains(&format!("/smith {name}")), "help 漏了 {name}");
        }
        assert!(help.contains("不自动发送"));
    }

    #[test]
    fn display_matches_the_command_wire_shape() {
        let result = display("hello", true);
        assert_eq!(result.effect, HandlerEffect::Ok);
        assert_eq!(
            result.data,
            json!({ "kind": "display", "content": "hello", "is_error": true })
        );
    }

    #[test]
    fn completions_offer_verbs_then_values() {
        let verbs = completions("", 0).data;
        let labels = verbs["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(labels, SUBCOMMANDS.iter().map(|(name, _)| (*name).to_owned()).collect::<Vec<_>>());

        let values = completions("strength ", 9).data;
        let items = values["items"].as_array().unwrap();
        assert_eq!(items.len(), STRENGTHS.len());

        let filtered = completions("mode pl", 7).data;
        let items = filtered["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["label"], "plain");

        // 草稿位置不给任何补全，别把用户正要输入的字拦下来。
        // cursor 是字节下标：3 正好落在「帮」之后，前缀是非动词的首词，此时不该给补全。
        let none = completions("帮我", 3).data;
        assert!(none["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn argument_prefix_clamps_to_a_char_boundary() {
        // 光标落在中文字节中间时不能 panic。
        let argument = "帮我改";
        let prefix = argument_prefix(argument, 4);
        assert_eq!(prefix, "帮");
    }
}
