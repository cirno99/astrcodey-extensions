//! 领域提示词模块的组装，逐条对照上游 `src/prompts/*/*.ts` 的 `buildPrompt`。
//!
//! 正文本身在 [`super::corpus`] 里；这里只负责按「难度 × 状态」把正文取出来，
//! 并按上游的模板重建 DEEP_UNDERSTAND 的首行。

use crate::types::{
    Difficulty, MasterTaskType, SubTaskType, TOOL_TASK_INFO, ThinkingState, difficulty_label,
};

use super::{Module, Tiered, load_prompt_module};

/// DEEP_UNDERSTAND 在 HARD/EXTREME 下追加的分析要求（上游各模块里的 `extra` 常量）。
const UNDERSTAND_EXTRA: &str = " 需深度分析所有边界条件、隐含约束和潜在风险。";

/// 任务画像未设定时占位（上游 `getPromptContent` 的早退分支）。
pub fn pending_profile_placeholder() -> String {
    format!("等待模型调用 {TOOL_TASK_INFO} 工具 设定任务信息...")
}

/// 按难度取四档正文之一。
fn tiered(tiered: &Tiered, difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::TRIVIAL => tiered.trivial,
        Difficulty::SIMPLE => tiered.simple,
        Difficulty::MODERATE | Difficulty::COMPLEX => tiered.standard,
        Difficulty::HARD | Difficulty::EXTREME => tiered.hard,
    }
}

/// 单个模块在指定状态与难度下的正文。
///
/// START / END 没有模块正文（上游 `default: return ""`）。
pub fn build_prompt(module: &Module, difficulty: Difficulty, state: ThinkingState) -> String {
    match state {
        ThinkingState::DEEP_UNDERSTAND => {
            let extra = match difficulty {
                Difficulty::HARD | Difficulty::EXTREME => UNDERSTAND_EXTRA,
                _ => "",
            };
            format!(
                "当前为{}（{}难度）。{extra}{}",
                module.label,
                difficulty_label(difficulty),
                module.understand_tail
            )
        },
        ThinkingState::DESIGN => tiered(&module.design, difficulty).to_owned(),
        ThinkingState::EXECUTE => tiered(&module.execute, difficulty).to_owned(),
        ThinkingState::VERIFY => tiered(&module.verify, difficulty).to_owned(),
        ThinkingState::START | ThinkingState::END => String::new(),
    }
}

/// 上游 `templates.ts` 的 `getPromptContent`。
///
/// 任务画像三项任一缺失时返回占位文本，而不是空串——上游正是靠它把
/// 「还没设定画像」这件事说给模型听。
pub fn get_prompt_content(
    master: Option<MasterTaskType>,
    sub: Option<SubTaskType>,
    difficulty: Option<Difficulty>,
    state: ThinkingState,
) -> String {
    let (Some(master), Some(sub), Some(difficulty)) = (master, sub, difficulty) else {
        return pending_profile_placeholder();
    };

    match load_prompt_module(master, sub) {
        Some(module) => build_prompt(module, difficulty, state),
        // 语料里 `general/general` 恒存在，这条分支实际不可达；保留它是为了与上游的
        // `if (!mod) return "(未找到匹配的提示词模块)"` 逐条对齐。
        None => "(未找到匹配的提示词模块)".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::corpus;

    #[test]
    fn deep_understand_rebuilds_the_difficulty_head_line() {
        let module = &corpus::CODING_RUST_DEV;
        let trivial = build_prompt(module, Difficulty::TRIVIAL, ThinkingState::DEEP_UNDERSTAND);
        assert!(trivial.starts_with("当前为编程类Rust开发（微不足道难度）。\n\n明确Rust版本"));

        let hard = build_prompt(module, Difficulty::HARD, ThinkingState::DEEP_UNDERSTAND);
        assert!(
            hard.starts_with("当前为编程类Rust开发（困难难度）。 需深度分析所有边界条件、隐含约束和潜在风险。\n\n")
        );
        // 除首行外两档一致。
        assert_eq!(trivial.split_once("\n\n").unwrap().1, hard.split_once("\n\n").unwrap().1);
    }

    #[test]
    fn tiers_pick_the_upstream_branch() {
        let module = &corpus::CODING_RUST_DEV;
        assert_eq!(build_prompt(module, Difficulty::TRIVIAL, ThinkingState::DESIGN), "");
        assert_eq!(
            build_prompt(module, Difficulty::SIMPLE, ThinkingState::DESIGN),
            module.design.simple
        );
        // MODERATE 与 COMPLEX 共用默认分支。
        assert_eq!(
            build_prompt(module, Difficulty::MODERATE, ThinkingState::DESIGN),
            build_prompt(module, Difficulty::COMPLEX, ThinkingState::DESIGN)
        );
        // HARD 与 EXTREME 共用同一个 case 分支。
        assert_eq!(
            build_prompt(module, Difficulty::HARD, ThinkingState::EXECUTE),
            build_prompt(module, Difficulty::EXTREME, ThinkingState::EXECUTE)
        );
    }

    #[test]
    fn start_and_end_have_no_module_body() {
        let module = &corpus::CODING_RUST_DEV;
        assert_eq!(build_prompt(module, Difficulty::HARD, ThinkingState::START), "");
        assert_eq!(build_prompt(module, Difficulty::HARD, ThinkingState::END), "");
    }

    #[test]
    fn a_missing_profile_yields_the_placeholder() {
        let placeholder = pending_profile_placeholder();
        assert_eq!(
            get_prompt_content(None, None, None, ThinkingState::DESIGN),
            placeholder
        );
        assert_eq!(
            get_prompt_content(
                Some(MasterTaskType::CODING),
                Some(SubTaskType::RUST_DEV),
                None,
                ThinkingState::DESIGN
            ),
            placeholder
        );
    }

    #[test]
    fn a_set_profile_yields_the_module_body() {
        let content = get_prompt_content(
            Some(MasterTaskType::GENERAL),
            Some(SubTaskType::GENERAL),
            Some(Difficulty::MODERATE),
            ThinkingState::EXECUTE,
        );
        assert!(content.contains("贴合上下文执行"));
    }
}
