//! Worker 装配：注册 `anki_write_apkg` 工具与 `prompt_build` 引导。
//!
//! 职责边界（与 [`prompt::GUIDANCE`] 呼应）：模型用内置文件工具读 vault、组织卡片，
//! 本扩展只提供「JSON 牌组规范 → .apkg」这最后一个确定性环节。工具是**新增**而非
//! 覆盖内置（AstrCode 的 `ToolRegistry` 对重名注册直接报错，见 hashline-edit 的同款注释）。

use std::path::Path;

use astrcode_extension_worker::worker_prelude::*;
use serde_json::json;

use crate::{apkg, error::DeckError, prompt, spec::{self, DeckSpecArgs}};

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-anki-deck";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));

    worker.tool(
        tool("anki_write_apkg")
            .description(
                "Write an Anki deck package (.apkg) from a JSON deck spec. Gather flashcards \
                 from markdown notes yourself, then call this once per top-level deck. Cards \
                 carry front/back HTML, optional cloze:true (front must contain {{c1::...}} \
                 markers), whitespace-free tags, and an id — a stable identity like \
                 \"vault:notes/rust.md#ownership\" that fixes the note GUID, so re-importing \
                 a regenerated deck updates cards instead of duplicating them. media are \
                 referenced in card HTML by bare file name.",
            )
            .parameters(json!({
                "type": "object",
                "properties": {
                    "output": {
                        "type": "string",
                        "description": "Target .apkg path (relative to the working directory or absolute)."
                    },
                    "deck": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "Deck name; use :: for subdecks (e.g. \"Rust::Ownership\"). No double quotes, no leading/trailing whitespace."
                            },
                            "cards": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "front": {
                                            "type": "string",
                                            "description": "Front side, HTML allowed. For cloze cards, put {{c1::...}} markers here."
                                        },
                                        "back": {
                                            "type": "string",
                                            "description": "Back side, HTML allowed. For cloze cards this is the extra text shown after the cloze."
                                        },
                                        "cloze": {
                                            "type": "boolean",
                                            "description": "True for cloze cards; the front must then contain at least one {{cN::...}} marker."
                                        },
                                        "tags": {
                                            "type": "array",
                                            "items": { "type": "string" },
                                            "description": "Anki tags; must not contain whitespace."
                                        },
                                        "id": {
                                            "type": "string",
                                            "description": "Stable identity (e.g. \"vault:notes/rust.md#ownership\") that determines the note GUID together with the deck name; same id + deck name updates the card on re-import."
                                        }
                                    },
                                    "required": ["front"],
                                    "additionalProperties": false
                                }
                            },
                            "media": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Paths of media files (images, audio) referenced by the cards; reference them in HTML by their bare file name."
                            },
                            "css": {
                                "type": ["string", "null"],
                                "description": "Optional custom card CSS overriding the built-in style."
                            }
                        },
                        "required": ["name", "cards"],
                        "additionalProperties": false
                    }
                },
                "required": ["output", "deck"],
                "additionalProperties": false
            }))
            .build(),
        tool_planner_args(|args: DeckSpecArgs, ctx| async move {
            // 权限面 = 写输出文件 + 读媒体源文件。逐个声明，宿主据此放行。
            let working_dir = ctx.working_dir().to_path_buf();
            let mut accesses = vec![ResourceAccess::read_write_file(apkg::resolve_path(
                &working_dir,
                &args.output,
            ))];
            for media in &args.deck.media {
                accesses.push(ResourceAccess::read_file(apkg::resolve_path(
                    &working_dir,
                    media,
                )));
            }
            Ok(ToolPlan::new(accesses))
        }),
        tool_handler_args(move |args: DeckSpecArgs, ctx| async move {
            let working_dir = ctx.working_dir().to_path_buf();
            let joined = tokio::task::spawn_blocking(move || execute(&args, &working_dir)).await;
            into_handler_result(joined)
        }),
    )?;

    worker.on_prompt_build(prompt_build_handler(
        |_input: PromptBuildHookInput, _ctx| async move {
            Ok(PromptContributions {
                system_prompts: vec![prompt::GUIDANCE.to_owned()],
                ..Default::default()
            })
        },
    ))?;

    worker.run_stdio().await
}

/// 工具执行体：纯校验 → 解析媒体与 GUID → 写 apkg。返回进工具结果的 JSON 摘要。
fn execute(args: &DeckSpecArgs, working_dir: &Path) -> Result<String, DeckError> {
    spec::validate(args)?;
    let plan = apkg::build_plan(args, working_dir)?;
    let output = apkg::resolve_path(working_dir, &args.output);
    let stats = apkg::write_apkg(&plan, &output)?;
    Ok(json!({
        "apkg": output.display().to_string(),
        "notes": stats.notes,
        "cloze_notes": stats.cloze_notes,
        "cards": stats.cards,
        "media": stats.media,
    })
    .to_string())
}

/// 把阻塞线程池的结果折成 S5R 的工具结果。
///
/// 与 hashline-edit 同构：领域失败（`[E_*]`）走 `is_error = true` 的正常结果，让纠正
/// 指引逐字抵达模型；只有线程池本身出错才返回 `Err`。
fn into_handler_result(
    joined: Result<Result<String, DeckError>, tokio::task::JoinError>,
) -> Result<HandlerResult, ErrorPayload> {
    match joined {
        Ok(Ok(summary)) => Ok(tool_text(summary, false)),
        Ok(Err(error)) => Ok(tool_text(error.render(), true)),
        Err(task_error) => Err(ErrorPayload::new(
            WireErrorCode::InternalError,
            format!("anki tool task failed: {task_error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn a_summary_becomes_a_successful_tool_result() {
        let result =
            into_handler_result(Ok(Ok(String::from("{\"cards\":3}")))).expect("应当成功");
        assert_eq!(result.data["content"], json!("{\"cards\":3}"));
        assert_eq!(result.data["is_error"], json!(false));
    }

    #[test]
    fn a_deck_error_becomes_an_error_tool_result_with_the_marker() {
        let result = into_handler_result(Ok(Err(DeckError::new(
            ErrorCode::EmptyFront,
            "cards[0].front is empty",
        ))))
        .expect("应当成功");
        assert_eq!(
            result.data["content"],
            json!("[E_EMPTY_FRONT] cards[0].front is empty")
        );
        assert_eq!(result.data["is_error"], json!(true));
    }

    #[tokio::test]
    async fn a_join_error_becomes_an_error_payload() {
        let joined: Result<Result<String, DeckError>, tokio::task::JoinError> =
            tokio::task::spawn_blocking(|| panic!("boom")).await;
        let error = into_handler_result(joined).expect_err("应当报基础设施错误");
        assert_eq!(error.code, WireErrorCode::InternalError.as_str());
    }
}
