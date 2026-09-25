//! Worker 装配：注册三个工具与 `prompt_build` 引导。
//!
//! # 为什么工具是「增量」的
//!
//! 内置已有 `read` / `write` / `edit` / `patch`。原版 Pi 覆盖了内置 `read` 并禁用
//! `edit`，但 AstrCode 的 `ToolRegistry` 对重名注册直接报错（`duplicate tool registered`），
//! 磁盘扩展无法遮蔽内置工具。因此这里与原版 DSH 移植一样，注册**新增**的三个工具，
//! 并靠 system prompt 引导模型优先用 `replace`。
//!
//! 这个取舍不影响安全性：`replace` 的 served 守卫只认自己 `hashline_read` 展示过的
//! 锚点，模型用内置 `read` 拿到的行号在 `replace` 里会被直接拒绝。

use astrcode_extension_sdk::tool::ExecutionMode;
use astrcode_extension_worker::worker_prelude::*;
use serde_json::json;

use crate::{
    fsops,
    hashline::error::EditError,
    prompt,
    state::StateRegistry,
    tools::{
        ToolOutcome,
        read::ReadArgs,
        replace::ReplaceArgs,
        undo::UndoArgs,
    },
};

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-hashline-edit";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));
    let registry = StateRegistry::default();

    let read_registry = registry.clone();
    worker.tool(
        tool("hashline_read")
            .description(
                "Read a UTF-8 text file as HASH│content rows. Every line carries a unique 3-char \
                 alphanumeric hash (the line's address) — no line numbers. Use the hashes in \
                 replace calls (remove_from/remove_to). Anchors of lines you did not edit stay \
                 valid across replaces. Pass raw:true to get plain numbered content without \
                 anchors.",
            )
            .parameters(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative or absolute)."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based first line to return. Defaults to 1."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of lines to return. Defaults to 2000."
                    },
                    "raw": {
                        "type": "boolean",
                        "description": "When true, return plain line-numbered content without hash anchors (use for inspection, not editing)."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }))
            .execution_mode(ExecutionMode::Parallel)
            .build(),
        tool_planner_args(|args: ReadArgs, ctx| async move {
            Ok(ToolPlan::new([ResourceAccess::read_file(fsops::resolve_path(
                ctx.working_dir(),
                &args.path,
            ))]))
        }),
        tool_handler_args(move |args: ReadArgs, ctx| {
            let registry = read_registry.clone();
            async move {
                let state = registry.session(EXTENSION_ID, ctx.session_id());
                let working_dir = ctx.working_dir().to_path_buf();
                let joined = tokio::task::spawn_blocking(move || {
                    let mut state = lock(&state);
                    crate::tools::read::execute(&args, &working_dir, &mut state)
                })
                .await;
                into_handler_result(joined)
            }
        }),
    )?;

    let replace_registry = registry.clone();
    worker.tool(
        tool("replace")
            .description(
                "Replace a range of lines in a UTF-8 text file, targeted by the 3-char HASH \
                 anchors from hashline_read output. remove_from and remove_to must each be a BARE \
                 3-character hash: copy only the hash from the leftmost column of a read row (row \
                 `ve7│function hello() {` means \"remove_from\": \"ve7\"). Never pass line content, \
                 code lines, or paragraphs into these fields. replacement_text is the new content \
                 as one string with \\n separators (\"\" deletes the range). The file must have been \
                 read with hashline_read first; stale anchors are rejected with fresh-anchor \
                 feedback.",
            )
            .parameters(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative or absolute)."
                    },
                    "remove_from": {
                        "type": "string",
                        "description": "Bare 3-char HASH only (e.g. \"aB3\") — marks the FIRST line to remove (inclusive)."
                    },
                    "remove_to": {
                        "type": "string",
                        "description": "Bare 3-char HASH only (e.g. \"aB3\") — marks the LAST line to remove (inclusive)."
                    },
                    "replacement_text": {
                        "type": "string",
                        "description": "Replacement text as a single string with \\n line separators; every \\n separates lines, so a trailing \\n adds a final empty line. Mirror the removed lines exactly, blank lines included. Use \"\" to delete the range."
                    }
                },
                "required": ["path", "remove_from", "remove_to", "replacement_text"],
                "additionalProperties": false
            }))
            .build(),
        tool_planner_args(|args: ReplaceArgs, ctx| async move {
            Ok(ToolPlan::new([ResourceAccess::read_write_file(
                fsops::resolve_path(ctx.working_dir(), &args.path),
            )]))
        }),
        tool_handler_args(move |args: ReplaceArgs, ctx| {
            let registry = replace_registry.clone();
            async move {
                let state = registry.session(EXTENSION_ID, ctx.session_id());
                let working_dir = ctx.working_dir().to_path_buf();
                let joined = tokio::task::spawn_blocking(move || {
                    let mut state = lock(&state);
                    crate::tools::replace::execute(&args, &working_dir, &mut state)
                })
                .await;
                into_handler_result(joined)
            }
        }),
    )?;

    let undo_registry = registry.clone();
    worker.tool(
        tool("undo_last_replace")
            .description(
                "Revert the last successful replace on a file, restoring its previous content. \
                 The undo record survives restarts (stored per session). Fails with [E_UNDO_STALE] \
                 when the file was modified after the replace or no longer exists.",
            )
            .parameters(json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to undo (relative or absolute)."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }))
            .build(),
        tool_planner_args(|args: UndoArgs, ctx| async move {
            Ok(ToolPlan::new([ResourceAccess::read_write_file(
                fsops::resolve_path(ctx.working_dir(), &args.path),
            )]))
        }),
        tool_handler_args(move |args: UndoArgs, ctx| {
            let registry = undo_registry.clone();
            async move {
                let state = registry.session(EXTENSION_ID, ctx.session_id());
                let working_dir = ctx.working_dir().to_path_buf();
                let joined = tokio::task::spawn_blocking(move || {
                    let mut state = lock(&state);
                    crate::tools::undo::execute(&args, &working_dir, &mut state)
                })
                .await;
                into_handler_result(joined)
            }
        }),
    )?;

    worker.on_prompt_build(prompt_build_handler(|_input: PromptBuildHookInput, _ctx| async move {
        Ok(PromptContributions {
            system_prompts: vec![prompt::GUIDANCE.to_owned()],
            ..Default::default()
        })
    }))?;

    worker.run_stdio().await
}

/// 取会话状态。锁被毒化时继续用内部值——状态只是缓存，不值得让整个扩展停摆。
fn lock(state: &std::sync::Arc<std::sync::Mutex<crate::state::SessionState>>) -> std::sync::MutexGuard<'_, crate::state::SessionState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 把阻塞线程池的结果折成 S5R 的工具结果。
///
/// 编辑失败（`[E_*]`）走 `is_error = true` 的正常结果，让那段纠正指引逐字抵达模型；
/// 只有线程池本身出错才返回 `Err`。
fn into_handler_result(
    joined: Result<Result<ToolOutcome, EditError>, tokio::task::JoinError>,
) -> Result<HandlerResult, ErrorPayload> {
    match joined {
        Ok(Ok(outcome)) => Ok(tool_text(outcome.text, outcome.is_error)),
        Ok(Err(error)) => Ok(tool_text(error.render(), true)),
        Err(error) => Err(ErrorPayload::new(
            WireErrorCode::InternalError,
            format!("hashline tool task failed: {error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashline::{EditError, ErrorCode};

    #[test]
    fn a_tool_outcome_becomes_a_tool_result() {
        let result = into_handler_result(Ok(Ok(ToolOutcome::ok("done")))).expect("应当成功");
        assert_eq!(result.data["content"], json!("done"));
        assert_eq!(result.data["is_error"], json!(false));
    }

    #[test]
    fn an_edit_error_becomes_an_error_tool_result_with_the_marker() {
        let result = into_handler_result(Ok(Err(EditError::new(ErrorCode::WouldEmpty, "nope"))))
            .expect("应当成功");
        assert_eq!(result.data["content"], json!("[E_WOULD_EMPTY] nope"));
        assert_eq!(result.data["is_error"], json!(true));
    }

    #[tokio::test]
    async fn a_join_error_becomes_an_error_payload() {
        let joined: Result<Result<ToolOutcome, EditError>, tokio::task::JoinError> =
            tokio::task::spawn_blocking(|| panic!("boom")).await;
        let error = into_handler_result(joined).expect_err("应当报基础设施错误");
        assert_eq!(error.code, WireErrorCode::InternalError.as_str());
    }
}
