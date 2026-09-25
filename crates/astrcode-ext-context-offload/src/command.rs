//! Worker 装配与 `/offload` 命令处理。

use astrcode_ext_common::text::format_tokens;
use astrcode_extension_worker::worker_prelude::*;
use serde_json::json;

use crate::{
    hook,
    offload::OffloadPolicy,
    store::{OffloadEntry, OffloadStore},
    tool::{self, RetrieveArgs},
};

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-context-offload";

/// 状态栏条目 ID；只有执行过 `/offload` 之后该格才会出现（S5R 无法预注册）。
const STATUS_ITEM_ID: &str = "offload";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    // `tool_intercept` 覆盖 `post_tool_use`：本插件用它替换工具结果的可见内容。
    worker.capability(ExtensionCapability::ToolIntercept);

    let policy = OffloadPolicy::default();

    worker.tool(
        tool(tool::TOOL_NAME)
            .description(
                "Retrieve the full output of a tool result that was offloaded to save context. \
                 Use the ref shown in the 📦 offload placeholder.",
            )
            .parameters(json!({
                "type": "object",
                "properties": {
                    "ref": {
                        "type": "string",
                        "description": "Reference from the 📦 offload placeholder, e.g. call_abc123"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Character offset to start from; defaults to 0"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum characters to return; defaults to 8000, capped at 20000"
                    }
                },
                "required": ["ref"]
            }))
            .build(),
        tool_planner(|_| async { Ok(ToolPlan::default()) }),
        tool_handler_args(|args: RetrieveArgs, ctx| async move { tool::handle(args, &ctx) }),
    )?;

    worker.hook(
        LifecycleEvent::PostToolUse,
        HookMode::Blocking,
        post_tool_use_handler(
            move |input, ctx| async move { hook::handle(input, ctx, policy).await },
        ),
    )?;

    worker.command(
        command("offload")
            .description("List tool outputs offloaded in this session")
            .build(),
        command_handler(|ctx| async move { execute(&ctx).await }),
    )?;

    worker.run_stdio().await
}

async fn execute(ctx: &WorkerCommandContext) -> Result<HandlerResult, ErrorPayload> {
    let store = OffloadStore::for_session(EXTENSION_ID, ctx.session_id());
    let entries = tokio::task::spawn_blocking(move || store.entries())
        .await
        .map_err(|error| {
            ErrorPayload::new(
                WireErrorCode::InternalError,
                format!("offload listing task failed: {error}"),
            )
        })?
        .map_err(|error| {
            ErrorPayload::new(
                WireErrorCode::IoError,
                format!("list offloaded outputs: {error}"),
            )
        })?;

    let report = render_report(&entries, OffloadPolicy::default().min_chars);
    Ok(command_result(&report, false, Some(status_text(&entries))))
}

/// 渲染 `/offload` 的正文。
fn render_report(entries: &[OffloadEntry], min_chars: usize) -> String {
    if entries.is_empty() {
        return format!(
            "本会话还没有换出任何工具输出（阈值 {min_chars} 字符）。超过阈值的工具输出会被换成 \
             📦 占位符，原文保存在插件数据目录。"
        );
    }

    let total: u64 = entries.iter().map(|entry| entry.bytes).sum();
    let mut lines = vec![format!(
        "本会话已换出 {} 条工具输出，原文合计 {} 字节：",
        entries.len(),
        format_tokens(total)
    )];
    for entry in entries {
        lines.push(format!(
            "  #{}  {} 字节",
            entry.reference,
            format_tokens(entry.bytes)
        ));
    }
    lines.push(String::new());
    lines.push("用 retrieve(ref=\"...\") 取回任意一条的原文。".to_string());
    lines.join("\n")
}

/// 状态栏一格：条数与合计大小。只在执行过 `/offload` 之后出现。
fn status_text(entries: &[OffloadEntry]) -> String {
    let total: u64 = entries.iter().map(|entry| entry.bytes).sum();
    format!("offload {} · {}", entries.len(), format_tokens(total))
}

/// 构造 `ExtensionCommandResult::Display` 的线缆形状。
///
/// 宿主在 `session_command_service` 里把它反序列化成 `ExtensionCommandResult`：
/// `Display { content, is_error, status_update }`，带 `kind = "display"` 标签。
/// `status_update` 存在时宿主立刻下发一条 `StatusItemUpdate` 通知。
fn command_result(content: &str, is_error: bool, status: Option<String>) -> HandlerResult {
    let mut data = json!({
        "kind": "display",
        "content": content,
        "is_error": is_error,
    });
    if let Some(text) = status {
        data["status_update"] = json!({ "id": STATUS_ITEM_ID, "text": text });
    }
    HandlerResult::effect(HandlerEffect::Ok, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(reference: &str, bytes: u64) -> OffloadEntry {
        OffloadEntry {
            reference: reference.into(),
            bytes,
        }
    }

    #[test]
    fn report_names_the_threshold_when_nothing_was_offloaded() {
        let report = render_report(&[], 8_000);
        assert!(report.contains("还没有换出任何工具输出"));
        assert!(report.contains("8000 字符"));
    }

    #[test]
    fn report_lists_every_entry_with_totals() {
        let report = render_report(&[entry("call-a", 12_300), entry("call-b", 500)], 8_000);
        assert!(report.contains("已换出 2 条"));
        assert!(report.contains("合计 12.8K 字节"));
        assert!(report.contains("#call-a  12.3K 字节"));
        assert!(report.contains("#call-b  500 字节"));
        assert!(report.contains("retrieve(ref=\"...\")"));
    }

    #[test]
    fn status_text_summarizes_counts_and_size() {
        assert_eq!(
            status_text(&[entry("call-a", 12_300), entry("call-b", 500)]),
            "offload 2 · 12.8K"
        );
        assert_eq!(status_text(&[]), "offload 0 · 0");
    }

    #[test]
    fn command_result_matches_the_display_wire_shape() {
        let result = command_result("hello", false, None);
        assert_eq!(result.effect, HandlerEffect::Ok);
        assert_eq!(
            result.data,
            json!({ "kind": "display", "content": "hello", "is_error": false })
        );
        assert!(result.data.get("status_update").is_none());
    }

    #[test]
    fn command_result_carries_the_status_update_when_present() {
        let result = command_result("hello", false, Some("offload 1 · 12.3K".into()));
        assert_eq!(result.data["status_update"]["id"], STATUS_ITEM_ID);
        assert_eq!(result.data["status_update"]["text"], "offload 1 · 12.3K");
    }
}
