//! enhancer 请求的提示词工件。
//!
//! # 为什么是英文
//!
//! 这段文本不发给用户，只发给改写模型，属于「被调优的英文工件」：措辞与顺序会改变改写结果的
//! 风格，因此保持英文原文、不做翻译，与本仓库 `prompt.rs` / `spec.rs` 里的其它注入工件同一
//! 约定。**改这里等于改行为**，要连同一并更新 `AGENTS.md` 里的逐字引用。
//!
//! # 与上游 pi-promptsmith 的差异
//!
//! 上游按目标模型家族分两套指引（GPT 走 OpenAI prompt guidance、Claude 允许 XML 分段）。
//! 本仓库使用者的激活模型是 deepseek-* / glm-* / qwen-*，两套分支都不会命中，因此合并成**一套
//! 中立指引**：outcome-first、紧凑、按需分节、不强制 XML。这不是取某一系的偏好冒充通用，而是
//! 三系通用模型都能稳定遵循的最大公约数。
//!
//! 另外补了上游没有的一条：`Preserve the draft's language`。上游全英文环境不需要它，而中文
//! 草稿配上英文指令时，模型很容易回一段英文 prompt——那样改写就退化成翻译，把用户的原意换了
//! 一种语言重说一遍，对后续 turn 没有任何帮助。

use crate::{
    config::Strength,
    intent::{EffectiveMode, TaskIntent},
};

/// 改写结果的哨兵开标签。解析靠它，因此两侧必须成对出现。
pub const SENTINEL_OPEN: &str = "<smith-prompt>";

/// 改写结果的哨兵闭标签。
pub const SENTINEL_CLOSE: &str = "</smith-prompt>";

/// enhancer 请求的输出上限。
///
/// 比上游的 1200 放宽到 2048：本插件默认打小模型，而国模小模型大多默认带推理通道，
/// 推理 token 也计在这个额度里，额度不够会直接截断正文。
pub const MAX_OUTPUT_TOKENS: usize = 2_048;

/// 构造 enhancer 的两条消息：system 定角色与硬约束，user 带指引、上下文与草稿。
pub fn build_request(
    draft: &str,
    intent: TaskIntent,
    mode: EffectiveMode,
    strength: Strength,
    model_id: &str,
) -> Vec<astrcode_extension_sdk::llm::LlmMessage> {
    use astrcode_extension_sdk::llm::LlmMessage;

    let mut user = Vec::new();
    match mode {
        EffectiveMode::Plain => user.extend(plain_instructions(intent)),
        EffectiveMode::ExecutionContract => user.extend(contract_instructions(intent)),
    }
    user.push(context_sections(draft, intent, mode, strength, model_id));

    vec![
        LlmMessage::system(system_prompt()),
        LlmMessage::user(user.join("\n\n")),
    ]
}

/// system 正文。逐条对应上游 `buildSharedSystemPrompt`，加上语言保持与家族分叉的删除。
fn system_prompt() -> String {
    [
        "You are Promptsmith, an expert prompt rewriter for a coding agent.",
        "Follow the resolved rewrite mode from the provided context.",
        "If the resolved rewrite mode is plain, rewrite the draft into a stronger prompt without deliberately compiling it into an execution contract.",
        "If the resolved rewrite mode is execution-contract, compile the draft into a concise, executable task contract for a coding-agent workflow.",
        // 语言保持：改写不是翻译。
        "Preserve the draft's language: write the rewritten prompt in the same language as the draft, and never translate it.",
        "Preserve the user's original intent.",
        "Preserve explicit constraints, file paths, commands, APIs, acceptance criteria, and other concrete details.",
        "Do not invent facts, requirements, files, commands, or context that the user did not provide.",
        "Avoid speculative implementation details, generic filler, and duplicated sections.",
        "Keep the output concise.",
        "Do not add commentary about your rewrite.",
        "Do not use tools.",
        sentinel_reminder(),
    ]
    .join("\n")
}

/// plain 模式指引，对应上游 `buildPlainRewriteInstructions`（去掉了家族分叉那两行）。
fn plain_instructions(intent: TaskIntent) -> Vec<String> {
    vec![
        "Rewrite the draft into a stronger prompt.".to_owned(),
        "Use direct, practical wording, with compact sections only where they materially improve clarity.".to_owned(),
        if intent == TaskIntent::Explain {
            "Keep it primarily explanatory instead of turning it into an execution plan.".to_owned()
        } else {
            "Improve clarity, scope, and output expectations without turning it into a rigid execution contract unless the draft already asks for that.".to_owned()
        },
        "Keep the rewrite concise, concrete, and faithful to the user's wording and scope.".to_owned(),
        "Avoid filler, speculative best-practice lists, and duplicated instructions.".to_owned(),
    ]
}

/// execution-contract 模式指引，对应上游 `buildExecutionContractInstructions`。
///
/// 上游 claude/gpt 两条结构偏好合并成一条「按需分节、不强制 XML」。
fn contract_instructions(intent: TaskIntent) -> Vec<String> {
    vec![
        "Compile the draft into a concise execution contract for a coding-agent workflow.".to_owned(),
        "Produce the smallest strong contract that makes the task executable.".to_owned(),
        "Make the objective, relevant context, explicit constraints, inspection surfaces, expected changes, verification, and deliverable expectations clear when they are useful.".to_owned(),
        "Prefer compact natural sections or bullets. Do not add XML scaffolding unless the draft already uses it.".to_owned(),
        "Do not emit empty sections, generic filler, or speculative requirements.".to_owned(),
        intent_guidance(intent).to_owned(),
    ]
}

/// 按意图的侧重说明，逐条对应上游 `buildIntentGuidance`。
fn intent_guidance(intent: TaskIntent) -> &'static str {
    match intent {
        TaskIntent::Implement => {
            "Shape the contract around a clear feature goal, scope boundaries, constraints, validation, and the expected output summary."
        },
        TaskIntent::Debug => {
            "Bias toward inspecting before editing, reproducing or confirming the issue, fixing the root cause, adding or updating regression coverage when appropriate, and verifying the fix."
        },
        TaskIntent::Refactor => {
            "Bias toward preserving behavior, improving structure, avoiding unnecessary API changes, removing duplication or dead code when appropriate, and running relevant checks."
        },
        TaskIntent::Review => {
            "Bias toward inspecting the current implementation first, reporting findings before suggestions, ordering findings by severity or impact, and avoiding speculative redesign unless requested."
        },
        TaskIntent::Research => {
            "Bias toward implementation-relevant facts, focused comparison or investigation, citing sources when web research is explicitly requested, and ending with a recommended path."
        },
        TaskIntent::Docs => {
            "Bias toward updating the exact user-facing docs affected, aligning them with current runtime behavior, and keeping examples and commands accurate."
        },
        TaskIntent::TestFix => {
            "Bias toward reproducing the failing behavior, deciding whether the bug or the test is wrong, fixing the correct layer, keeping regression coverage close to the change, and rerunning relevant checks."
        },
        TaskIntent::Explain => {
            "Keep it explanatory unless the user explicitly asks for structured operational deliverables."
        },
        TaskIntent::General => {
            "Keep the contract helpful without pretending to know more than the draft provides."
        },
    }
}

fn sentinel_reminder() -> &'static str {
    "Return exactly one <smith-prompt>...</smith-prompt> block and nothing else."
}

/// 上下文区，对应上游 `buildSharedContextSections`。
///
/// 上游的 `recent_conversation` / `project_metadata` / `dropped_optional_context` 三段**没有
/// 移植**：它们各自要一次额外的上下文采集，而本插件的取值点是命令处理器（会话空闲时运行），
/// 拿不到 pi 那种编辑器现场状态。`preserve_code_blocks` 则直接固化进 system 的「保留具体细节」
/// 一条，不再做成开关。
fn context_sections(
    draft: &str,
    intent: TaskIntent,
    mode: EffectiveMode,
    strength: Strength,
    model_id: &str,
) -> String {
    [
        ("configured_rewrite_strength", strength.as_str()),
        ("effective_rewrite_mode", mode.as_str()),
        ("resolved_intent", intent.as_str()),
        ("target_model", model_id),
        ("draft", draft),
    ]
    .iter()
    .map(|(name, body)| format!("<{name}>\n{body}\n</{name}>"))
    .collect::<Vec<_>>()
    .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::RewriteMode;
    use astrcode_extension_sdk::llm::LlmRole;

    fn request_text(mode: EffectiveMode, intent: TaskIntent) -> (String, String) {
        let messages = build_request("add a toggle", intent, mode, Strength::Balanced, "glm-5.3-flash");
        assert_eq!(messages.len(), 2);
        let system = text_of(&messages[0]);
        let user = text_of(&messages[1]);
        (system, user)
    }

    fn text_of(message: &astrcode_extension_sdk::llm::LlmMessage) -> String {
        message
            .content
            .iter()
            .filter_map(|part| match part {
                astrcode_extension_sdk::llm::LlmContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn system_prompt_carries_role_constraints_and_sentinel() {
        let (system, _) = request_text(EffectiveMode::Plain, TaskIntent::Implement);
        assert!(system.starts_with("You are Promptsmith"));
        assert!(system.contains("Preserve the draft's language"));
        assert!(system.contains(SENTINEL_OPEN));
        assert!(system.contains("Do not use tools"));
    }

    #[test]
    fn first_message_is_system_and_second_is_user() {
        let messages = build_request(
            "d",
            TaskIntent::General,
            EffectiveMode::Plain,
            Strength::Light,
            "qwen-3.8-flash",
        );
        assert_eq!(messages[0].role, LlmRole::System);
        assert_eq!(messages[1].role, LlmRole::User);
    }

    #[test]
    fn mode_switches_the_instruction_block() {
        let (_, plain) = request_text(EffectiveMode::Plain, TaskIntent::Implement);
        assert!(plain.contains("Rewrite the draft into a stronger prompt."));
        assert!(!plain.contains("Compile the draft into a concise execution contract"));

        let (_, contract) = request_text(EffectiveMode::ExecutionContract, TaskIntent::Implement);
        assert!(contract.contains("Compile the draft into a concise execution contract"));
        assert!(contract.contains("Shape the contract around a clear feature goal"));
    }

    /// 中立指引不该留下任何家族偏向：既不能要求 XML，也不能引用 OpenAI 指南。
    #[test]
    fn guidance_is_family_neutral() {
        let (system, contract) = request_text(EffectiveMode::ExecutionContract, TaskIntent::Debug);
        let combined = format!("{system}\n{contract}");
        for forbidden in ["GPT-style", "Claude-style", "OpenAI", "XML-like sections are allowed"] {
            assert!(!combined.contains(forbidden), "指引里不该出现 `{forbidden}`");
        }
        assert!(contract.contains("Do not add XML scaffolding"));
    }

    #[test]
    fn context_sections_carry_every_resolved_field_and_the_draft() {
        let (_, user) = request_text(EffectiveMode::Plain, TaskIntent::Explain);
        for expected in [
            "<configured_rewrite_strength>\nbalanced\n</configured_rewrite_strength>",
            "<effective_rewrite_mode>\nplain\n</effective_rewrite_mode>",
            "<resolved_intent>\nexplain\n</resolved_intent>",
            "<target_model>\nglm-5.3-flash\n</target_model>",
            "<draft>\nadd a toggle\n</draft>",
        ] {
            assert!(user.contains(expected), "缺少上下文段：{expected}");
        }
    }

    #[test]
    fn plain_intent_guidance_keeps_explanations_explanatory() {
        let (_, user) = request_text(EffectiveMode::Plain, TaskIntent::Explain);
        assert!(user.contains("Keep it primarily explanatory"));
    }

    #[test]
    fn rewrite_mode_stays_available_for_mode_resolution() {
        // 配置层与命令层都靠这两个函数，放在这里一起钉住，避免两侧各写一套映射。
        assert_eq!(
            crate::intent::resolve_effective_mode(RewriteMode::Auto, TaskIntent::Docs),
            EffectiveMode::ExecutionContract
        );
    }
}
