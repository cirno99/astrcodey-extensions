//! `retrieve` 工具：按占位符里的 ref 取回完整原文。
//!
//! 分页而非一次给全：取回的原文会原样进入上下文，一次给全等于把 offload 省下的
//! token 又还回去。默认页大小与 offload 阈值同量级，一页通常就够用。

use astrcode_extension_sdk::s5r::hooks::PostToolUseHookInput;
use astrcode_extension_worker::worker_prelude::*;
use serde::Deserialize;

use crate::store::OffloadStore;

/// 工具名。`post_tool_use` 钩子用它排除自身：取回的内容若再被 offload，模型会
/// 陷入「retrieve → 又得到占位符」的死循环。
pub const TOOL_NAME: &str = "retrieve";

/// 单页默认字符数。
pub const DEFAULT_PAGE_CHARS: usize = 8_000;

/// 单页上限：即使模型要求更多，也不让一次取回吃掉过多上下文。
pub const MAX_PAGE_CHARS: usize = 20_000;

#[derive(Debug, Deserialize)]
pub struct RetrieveArgs {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// 一次取回的结果。
#[derive(Debug, PartialEq, Eq)]
pub struct Retrieval {
    pub text: String,
    /// 引用不存在时为真，对应工具结果里的错误标记。
    pub is_error: bool,
}

/// S5R handler 适配层。
pub fn handle(
    args: RetrieveArgs,
    ctx: &WorkerInvocationContext,
) -> Result<HandlerResult, ErrorPayload> {
    let store = OffloadStore::for_session(ctx.extension_id(), ctx.session_id());
    let retrieval = retrieve(args, &store)?;
    Ok(tool_text(retrieval.text, retrieval.is_error))
}

/// 与宿主 context 无关的核心逻辑：读回原文并渲染一页。
pub fn retrieve(args: RetrieveArgs, store: &OffloadStore) -> Result<Retrieval, ErrorPayload> {
    let content = store.read(&args.reference).map_err(|error| {
        ErrorPayload::new(
            WireErrorCode::IoError,
            format!("read offloaded output: {error}"),
        )
    })?;

    let Some(content) = content else {
        // 幻觉出来的 ref 只花一次工具调用，不报错——与占位符提示的语义一致。
        return Ok(Retrieval {
            text: format!(
                "No offloaded output for ref `{}` in this session. Use the ref shown in the 📦 \
                 offload placeholder.",
                args.reference
            ),
            is_error: true,
        });
    };

    Ok(Retrieval {
        text: render_page(&args, &content),
        is_error: false,
    })
}

/// 渲染一页原文，并附上分页进度。
fn render_page(args: &RetrieveArgs, content: &str) -> String {
    let total = content.chars().count();
    let limit = args
        .limit
        .unwrap_or(DEFAULT_PAGE_CHARS)
        .clamp(1, MAX_PAGE_CHARS);
    let offset = args.offset.min(total);

    let page: String = content.chars().skip(offset).take(limit).collect();
    let end = offset + page.chars().count();
    let progress = if end < total {
        format!("showing chars {offset}-{end} of {total} · next offset={end}")
    } else {
        format!("showing chars {offset}-{end} of {total} · complete")
    };

    format!("{page}\n\n[offload #{} · {progress}]", args.reference)
}

/// 该工具调用是否应当绕过 offload。
pub fn is_self_call(input: &PostToolUseHookInput) -> bool {
    input.tool_name == TOOL_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(reference: &str, offset: usize, limit: Option<usize>) -> RetrieveArgs {
        RetrieveArgs {
            reference: reference.into(),
            offset,
            limit,
        }
    }

    #[test]
    fn first_page_reports_progress_and_next_offset() {
        let content = "abcdefghij";
        let rendered = render_page(&args("call-1", 0, Some(4)), content);
        assert_eq!(
            rendered,
            "abcd\n\n[offload #call-1 · showing chars 0-4 of 10 · next offset=4]"
        );
    }

    #[test]
    fn last_page_is_marked_complete() {
        let content = "abcdefghij";
        let rendered = render_page(&args("call-1", 8, Some(4)), content);
        assert_eq!(
            rendered,
            "ij\n\n[offload #call-1 · showing chars 8-10 of 10 · complete]"
        );
    }

    #[test]
    fn offset_past_the_end_yields_an_empty_complete_page() {
        let rendered = render_page(&args("call-1", 99, Some(4)), "abc");
        assert_eq!(
            rendered,
            "\n\n[offload #call-1 · showing chars 3-3 of 3 · complete]"
        );
    }

    #[test]
    fn limit_is_clamped_to_the_upper_bound() {
        let content = "x".repeat(MAX_PAGE_CHARS + 500);
        let rendered = render_page(&args("call-1", 0, Some(usize::MAX)), &content);
        assert!(rendered.starts_with(&"x".repeat(MAX_PAGE_CHARS)));
        assert!(rendered.contains(&format!("showing chars 0-{MAX_PAGE_CHARS} of")));
    }

    #[test]
    fn page_boundaries_count_characters_not_bytes() {
        let content = "错误信息内容";
        let rendered = render_page(&args("call-1", 1, Some(2)), content);
        assert!(rendered.starts_with("误信\n"));
    }

    #[test]
    fn retrieve_never_offloads_its_own_output() {
        let mut input: PostToolUseHookInput = serde_json::from_value(serde_json::json!({
            "session_id": "s-1",
            "working_dir": "/workspace",
            "model": { "profile_name": "default", "model": "m", "provider_kind": "openai" },
            "tool_call_id": "call-1",
            "tool_name": TOOL_NAME,
            "tool_input": {},
            "tool_result": { "content": "x", "is_error": false, "metadata": {} },
            "is_error": false
        }))
        .unwrap();
        assert!(is_self_call(&input));
        input.tool_name = "shell".into();
        assert!(!is_self_call(&input));
    }
}
