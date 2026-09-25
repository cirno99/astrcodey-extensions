//! Worker 装配与 `/usage` 命令处理。

use astrcode_extension_worker::worker_prelude::*;

use crate::{report, scan};

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-cache-usage";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    // `session_history` 覆盖 `astrcode.session.read_events`：读取当前会话的 durable 事件流。
    worker.capability(ExtensionCapability::SessionHistory);

    worker.command(
        command("usage")
            .description("Show prompt-cache hit rate for the current session")
            .build(),
        command_handler(|ctx| async move { dispatch(&ctx).await }),
    )?;

    worker.run_stdio().await
}

async fn dispatch(ctx: &WorkerCommandContext) -> Result<HandlerResult, ErrorPayload> {
    match ctx.invocation() {
        // 未声明 `argument_completions`，正常不会收到补全调用；返回空补全而非报错。
        WorkerCommandInvocation::Complete { .. } => Ok(command_result("", false, None)),
        WorkerCommandInvocation::Execute => execute(ctx).await,
    }
}

async fn execute(ctx: &WorkerCommandContext) -> Result<HandlerResult, ErrorPayload> {
    let outcome = scan::scan_session(ctx.session_id()).await?;
    let text = report::render(&outcome);
    let status = report::status_text(&outcome);
    Ok(command_result(&text, false, Some(status)))
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
        let result = command_result("hello", true, Some("cache 42.0%".into()));
        assert_eq!(result.data["kind"], "display");
        assert_eq!(result.data["is_error"], true);
        assert_eq!(result.data["status_update"]["id"], report::STATUS_ITEM_ID);
        assert_eq!(result.data["status_update"]["text"], "cache 42.0%");
    }
}
