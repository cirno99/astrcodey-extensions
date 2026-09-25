//! `/asymptotic-toggle`：按会话切换渐近式思考框架。
//!
//! 上游只支持无参切换（每次调用翻转一次）。移植版保留这个默认行为，另外补上
//! `on` / `off` / `status`——上游的开关状态只能靠再翻一次来确认，在会话里很难用。
//! 开关与状态机状态同放一份会话文件，因此扩展重载或宿主重启后仍然有效。

use astrcode_extension_worker::worker_prelude::*;

use crate::state::{self, StoreRegistry};
use crate::types::state_label;
use crate::worker::EXTENSION_ID;

/// 命令名，宿主侧解析为 `/asymptotic-toggle`。
pub const COMMAND_NAME: &str = "asymptotic-toggle";

/// 命令输出的统一前缀。
const PREFIX: &str = "渐近式思考：";

/// 构造 `ExtensionCommandResult::Display` 的线缆形状。
///
/// 宿主在 `session_command_service` 里把它反序列化成
/// `ExtensionCommandResult::Display { content, is_error, status_update }`。
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

/// 命令 handler。
pub fn handler(registry: StoreRegistry) -> CommandHandlerFn {
    command_handler(move |ctx: WorkerCommandContext| {
        let registry = registry.clone();
        async move {
            match ctx.invocation() {
                // 未声明 `argument_completions`，正常不会收到补全调用；返回空补全而非报错。
                WorkerCommandInvocation::Complete { .. } => Ok(display("", false)),
                WorkerCommandInvocation::Execute => execute(&registry, &ctx).await,
            }
        }
    })
}

async fn execute(
    registry: &StoreRegistry,
    ctx: &WorkerCommandContext,
) -> Result<HandlerResult, ErrorPayload> {
    let session_id = ctx.session_id().to_owned();
    let argument = ctx.argument().trim().to_ascii_lowercase();

    let target = match argument.as_str() {
        "" => None,
        "on" => Some(true),
        "off" => Some(false),
        "status" => return Ok(display(&status_line(registry, &session_id).await, false)),
        other => {
            return Ok(display(
                &format!(
                    "{PREFIX}未知参数 `{other}`。可用：`/asymptotic-toggle`（切换）、\
                     `/asymptotic-toggle on`、`/asymptotic-toggle off`、`/asymptotic-toggle status`。"
                ),
                true,
            ));
        },
    };

    let next = match target {
        Some(value) => value,
        None => !read_enabled(registry, &session_id),
    };

    set_enabled(registry, &session_id, next).await?;

    Ok(display(
        &if next {
            format!("{PREFIX}已启用。当前会话后续每轮注入状态机引导与框架规则。")
        } else {
            format!("{PREFIX}已禁用。当前会话不再注入任何内容，三个状态机工具也静默跳过。")
        },
        false,
    ))
}

async fn status_line(registry: &StoreRegistry, session_id: &str) -> String {
    let store = registry.session(EXTENSION_ID, session_id);
    let snapshot = {
        let guard = state::lock(&store);
        guard.snapshot()
    };
    let state = if snapshot.enabled { "已启用" } else { "已禁用" };
    format!(
        "{PREFIX}{state}。当前状态【{}】，任务轮次第 {} 轮，状态内第 {} 轮。",
        state_label(snapshot.machine.state),
        snapshot.machine.task_turn_count,
        snapshot.machine.state_turn_count
    )
}

fn read_enabled(registry: &StoreRegistry, session_id: &str) -> bool {
    let store = registry.session(EXTENSION_ID, session_id);
    let guard = state::lock(&store);
    guard.enabled()
}

async fn set_enabled(
    registry: &StoreRegistry,
    session_id: &str,
    enabled: bool,
) -> Result<(), ErrorPayload> {
    let store = registry.session(EXTENSION_ID, session_id);
    state::mutate(store, move |store| store.set_enabled(enabled))
        .await
        .map_err(|error| {
            ErrorPayload::new(
                WireErrorCode::InternalError,
                format!("asymptotic-thinking 写入开关失败：{error}"),
            )
        })
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
        // 本插件不写状态栏：磁盘扩展无法注册 status item。
        assert!(result.data.get("status_update").is_none());
    }

    #[test]
    fn display_marks_errors() {
        assert_eq!(display("bad", true).data["is_error"], true);
    }

    /// 命令名必须落在宿主的 `[a-z][a-z0-9_-]*` 校验内。
    #[test]
    fn the_command_name_passes_host_validation() {
        let mut chars = COMMAND_NAME.chars();
        let first = chars.next().expect("非空");
        assert!(first.is_ascii_lowercase());
        assert!(
            chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        );
    }
}
