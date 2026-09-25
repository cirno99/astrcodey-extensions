//! `post_tool_use` 钩子：超阈值时把工具输出换成可检索的占位符。
//!
//! 替换发生在工具结果即将提交时，宿主会把占位符写进 durable 记录，因此原文只能
//! 由本插件自己保存——这就是先落盘、再返回 `ModifyResult` 的顺序不可颠倒的原因。

use astrcode_extension_sdk::s5r::hooks::PostToolUseHookInput;
use astrcode_extension_worker::worker_prelude::*;

use crate::{
    offload::{self, OffloadPolicy},
    store::OffloadStore,
    tool,
};

/// S5R handler 适配层：从宿主 context 取出扩展与会话身份，转交 [`offload`]。
pub async fn handle(
    input: PostToolUseHookInput,
    ctx: WorkerInvocationContext,
    policy: OffloadPolicy,
) -> Result<PostToolUseResult, ErrorPayload> {
    let store = OffloadStore::for_session(ctx.extension_id(), ctx.session_id());
    offload(input, &store, policy).await
}

/// 与宿主 context 无关的核心逻辑：超阈值则落盘原文并返回占位符，否则原样放行。
///
/// 存储作为参数注入，测试因此可以指向临时目录，不必触碰真实用户数据目录。
pub async fn offload(
    input: PostToolUseHookInput,
    store: &OffloadStore,
    policy: OffloadPolicy,
) -> Result<PostToolUseResult, ErrorPayload> {
    if tool::is_self_call(&input) {
        return Ok(PostToolUseResult::Allow);
    }

    let PostToolUseHookInput {
        tool_call_id,
        tool_name,
        tool_result,
        is_error,
        ..
    } = input;
    let content = tool_result.content;

    if !offload::should_offload(&content, &policy) {
        return Ok(PostToolUseResult::Allow);
    }

    let reference = offload::make_ref(&tool_call_id, &content);
    let placeholder =
        offload::render_placeholder(&reference, &tool_name, &content, is_error, &policy);

    let store = store.clone();
    let write_reference = reference.clone();
    tokio::task::spawn_blocking(move || store.write(&write_reference, &content))
        .await
        .map_err(|error| {
            ErrorPayload::new(
                WireErrorCode::InternalError,
                format!("offload write task failed: {error}"),
            )
        })?
        .map_err(|error| {
            ErrorPayload::new(
                WireErrorCode::IoError,
                format!("store offloaded tool output: {error}"),
            )
        })?;

    Ok(PostToolUseResult::ModifyResult {
        content: placeholder,
    })
}
