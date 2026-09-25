//! 六态提示词组装，逐条对照上游 `src/templates.ts`。
//!
//! 上游对模块正文做两条清洗后才嵌进 `<instruction>`：去掉各模块自带的
//! 「当前为xxx（xxx难度）。」开头（与 `taskBlock` 首行重复），以及各模块自带的
//! transition 流转指令（统一由 `<actions>` 管理）。两条正则逐字照搬。

use std::sync::LazyLock;

use regex::Regex;

use crate::machine::format_next_state_hint;
use crate::prompts::assemble::get_prompt_content;
use crate::types::{
    Difficulty, MasterTaskType, SubTaskType, TOOL_TASK_INFO, TOOL_TRANSITION, ThinkingState,
    difficulty_label, get_max_state_turns, master_task_hint, master_task_type_label, state_label,
    sub_task_type_label,
};

/// 去掉模块自带的「当前为xxx（xxx难度）。」开头（含其后最多一个空行）。
static LEADING_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^当前为.+?（.+?难度）。[^\n]*\n\n?").expect("内置正则必须可编译")
});

/// 去掉模块自带的 transition 流转指令行。
static TRANSITION_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"[^\n]*{TOOL_TRANSITION}[^\n]*\n?")).expect("内置正则必须可编译")
});

/// 任务画像描述（`statusLineWithTask` 的第二段）。
fn task_desc(
    master: Option<MasterTaskType>,
    sub: Option<SubTaskType>,
    difficulty: Option<Difficulty>,
) -> String {
    let (Some(master), Some(difficulty)) = (master, difficulty) else {
        return "尚未设定".to_owned();
    };
    let master_label = master_task_type_label(master);
    let sub_label = sub
        .map(|sub| format!("-{}", sub_task_type_label(sub)))
        .unwrap_or_default();
    format!("{master_label}{sub_label} · {}", difficulty_label(difficulty))
}

/// 大类型领域提示。
fn task_hint(master: Option<MasterTaskType>) -> &'static str {
    master.map(master_task_hint).unwrap_or("")
}

/// 全链路路径感知：按 `visited` 生成适配段（纯函数）。
///
/// 三种情形：直达 EXECUTE（TRIVIAL 最短路径）、跳理解到 DESIGN（SIMPLE 跳 DEEP_UNDERSTAND）、
/// 从 EXECUTE 回退到 DESIGN。
pub fn path_adapter(state: ThinkingState, visited: &[ThinkingState]) -> &'static str {
    let has = |target: ThinkingState| visited.contains(&target);

    if state == ThinkingState::EXECUTE && !has(ThinkingState::DESIGN) {
        return "> ⚡ 本任务直达执行模式：未经过深度理解与方案设计。请先自行明确需求要点与执行步骤，再直接实施。";
    }
    if state == ThinkingState::DESIGN && !has(ThinkingState::DEEP_UNDERSTAND) {
        return "> ⚡ 本任务未经过深度理解。请先简要明确需求边界，再列出方案。";
    }
    if state == ThinkingState::DESIGN && has(ThinkingState::EXECUTE) {
        return "> ⚡ 曾进入 EXECUTE 后回退。方案需覆盖已执行部分的调整与修正。";
    }
    ""
}

/// 清洗模块正文：去首行难度描述、去 transition 指令行、去首尾空白。
fn clean_prompt_content(raw: &str) -> String {
    let without_head = LEADING_HEAD.replace(raw, "");
    TRANSITION_LINE
        .replace_all(&without_head, "")
        .trim()
        .to_owned()
}

/// 组装一轮注入给模型的引导文本。
///
/// `turn` 是任务轮次（`task_turn_count`），`visited` 是本次任务走过的状态路径。
pub fn build_template(
    state: ThinkingState,
    turn: u32,
    master: Option<MasterTaskType>,
    sub: Option<SubTaskType>,
    difficulty: Option<Difficulty>,
    visited: &[ThinkingState],
) -> String {
    let max_turns = get_max_state_turns(state, difficulty);
    let label = state_label(state);
    let status_line = format!("第{turn}/{max_turns}轮 {label}阶段");
    let status_line_with_task = format!("{status_line} · {}", task_desc(master, sub, difficulty));

    // ── START（启动：评估 + 设定画像）──
    if state == ThinkingState::START {
        return format!(
            "<task{turn}>

<instruction spec=\"markdown\">
{status_line}
第task{turn}轮任务启动

遵守《渐近式思考状态机操作规范》

本次任务目标是深度评估调用所有tool工具与skill技能，获取外部信息与本地记忆，挖掘用户指令隐藏信息。
深度评估是否需要使用memory_list/memory_save 本地记忆系统、web-search 网络搜索、fetch-url 网页获取，必须给出简要评估结论（节省输出token）。
深入分析用户指令的任务类型和难度，为后续阶段做准备。

工作纪律：
- 每轮开始先判断当前进度对应状态机的哪个阶段；状态不符时先调用 `{TOOL_TRANSITION}` 工具流转到对应状态，再继续执行
- 轮次超限且需等待用户决策时，停止当前操作与轮次，待用户下次输入再继续
</instruction>

<actions spec=\"markdown\">
- 调用 `{TOOL_TASK_INFO}` 工具设定任务画像
</actions>

</task{turn}>"
        )
        .trim()
        .to_owned();
    }

    // ── END（任务终态：极简）──
    if state == ThinkingState::END {
        return format!(
            "<task{turn}>

<instruction spec=\"markdown\">
{status_line}

遵守《渐近式思考状态机操作规范》

上一任务已完成，保持空闲等待新指令，不执行任何修改操作。
</instruction>

</task{turn}>"
        )
        .trim()
        .to_owned();
    }

    let hint = task_hint(master);
    let prompt_content = clean_prompt_content(&get_prompt_content(master, sub, difficulty, state));
    let path_segment = path_adapter(state, visited);
    let flow_hint = format_next_state_hint(state, difficulty);

    let hint_part = if hint.is_empty() {
        String::new()
    } else {
        format!("\n> {hint}")
    };
    let path_part = if path_segment.is_empty() {
        String::new()
    } else {
        format!("\n{path_segment}")
    };
    let prompt_part = if prompt_content.is_empty() {
        String::new()
    } else {
        format!("\n\n{prompt_content}")
    };

    let task_block = format!(
        "<instruction spec=\"markdown\">
{status_line_with_task}
遵守《渐近式思考状态机操作规范》
{hint_part}{path_part}
{prompt_part}

> 🔄 可用流转：{flow_hint}
</instruction>"
    );

    let body = match state {
        ThinkingState::DEEP_UNDERSTAND => format!(
            "<actions spec=\"markdown\">
- 逐条列出需求的功能边界、约束条件和验收标准
- 缺失信息用工具检索补齐，不猜测
- 完成本阶段职责后，调用 `{TOOL_TRANSITION}` 工具流转状态
</actions>

<constraints spec=\"markdown\">
- 只读文件辅助理解，不执行写操作
</constraints>"
        ),
        ThinkingState::DESIGN => format!(
            "<actions spec=\"markdown\">
- 方案按顺序列出：涉及文件 → 前置步骤 → 实施路径 → 验收标准
- 每个步骤写明具体工具名和文件路径
- 完成本阶段职责后，调用 `{TOOL_TRANSITION}` 工具流转状态
</actions>

<constraints spec=\"markdown\">
- 设计完成前不执行任何代码修改
</constraints>"
        ),
        ThinkingState::EXECUTE => format!(
            "<actions spec=\"markdown\">
- 严格按方案步骤顺序执行
- 每步执行后自检结果，确认通过再推进下一步
- 方案不可行时回退 DESIGN 重新设计
- 完成本阶段职责后，调用 `{TOOL_TRANSITION}` 工具流转状态
</actions>"
        ),
        ThinkingState::VERIFY => format!(
            "<output spec=\"markdown\">
| 维度 | 状态 | 说明 |
|:--|:--:|:--|
| 需求理解正确完整 | ✅/❌ | ... |
| 实施步骤全部完成 | ✅/❌ | ... |
| 输出格式符合规范 | ✅/❌ | ... |
</output>

<actions spec=\"markdown\">
- 逐项如实标注 ✅ 或 ❌
- 有 ❌ 项修正后全部重新验证
- 全部 ✅ 后调用 `{TOOL_TRANSITION}` 工具流转状态
</actions>"
        ),
        // START / END 已在上方早退。
        ThinkingState::START | ThinkingState::END => String::new(),
    };

    format!("<task{turn}>\n\n{task_block}\n\n{body}\n\n</task{turn}>")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_desc_falls_back_when_the_profile_is_missing() {
        assert_eq!(task_desc(None, None, None), "尚未设定");
        assert_eq!(
            task_desc(Some(MasterTaskType::CODING), None, None),
            "尚未设定"
        );
        assert_eq!(
            task_desc(
                Some(MasterTaskType::CODING),
                Some(SubTaskType::RUST_DEV),
                Some(Difficulty::COMPLEX)
            ),
            "编程类-Rust开发 · 复杂"
        );
        // 子类型缺失时只显示大类型。
        assert_eq!(
            task_desc(Some(MasterTaskType::CODING), None, Some(Difficulty::HARD)),
            "编程类 · 困难"
        );
    }

    #[test]
    fn path_adapter_covers_the_three_shapes() {
        assert_eq!(
            path_adapter(ThinkingState::EXECUTE, &[]),
            "> ⚡ 本任务直达执行模式：未经过深度理解与方案设计。请先自行明确需求要点与执行步骤，再直接实施。"
        );
        assert_eq!(
            path_adapter(ThinkingState::DESIGN, &[]),
            "> ⚡ 本任务未经过深度理解。请先简要明确需求边界，再列出方案。"
        );
        assert_eq!(
            path_adapter(
                ThinkingState::DESIGN,
                &[ThinkingState::DEEP_UNDERSTAND, ThinkingState::EXECUTE]
            ),
            "> ⚡ 曾进入 EXECUTE 后回退。方案需覆盖已执行部分的调整与修正。"
        );
        assert_eq!(
            path_adapter(
                ThinkingState::DESIGN,
                &[ThinkingState::DEEP_UNDERSTAND]
            ),
            ""
        );
    }

    /// 两条清洗正则必须与上游一致：去首行难度描述、去 transition 指令行。
    #[test]
    fn prompt_content_is_cleaned_like_upstream() {
        let raw = "当前为编程类Rust开发（复杂难度）。\n\n确定技术栈。\n完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》";
        assert_eq!(
            clean_prompt_content(raw),
            "确定技术栈。\n\n严格遵守《编程与架构准则》"
        );
    }

    /// 首行之后没有空行时，正则只吃掉一个换行。
    #[test]
    fn the_head_regex_swallows_at_most_one_blank_line() {
        assert_eq!(clean_prompt_content("当前为X（简单难度）。\n正文"), "正文");
        assert_eq!(clean_prompt_content("当前为X（简单难度）。\n\n正文"), "正文");
    }

    #[test]
    fn start_guides_the_model_to_set_a_profile() {
        let text = build_template(ThinkingState::START, 3, None, None, None, &[]);
        assert!(text.starts_with("<task3>\n\n<instruction spec=\"markdown\">\n第3/1轮 启动阶段\n第task3轮任务启动"));
        assert!(text.contains("- 调用 `asymptotic-think_set-task-info` 工具设定任务画像"));
        assert!(text.ends_with("</task3>"));
    }

    #[test]
    fn end_is_minimal() {
        let text = build_template(ThinkingState::END, 4, None, None, None, &[]);
        assert!(text.contains("上一任务已完成，保持空闲等待新指令，不执行任何修改操作。"));
        assert!(!text.contains("<actions"));
    }

    /// 业务态的骨架：instruction（含领域提示与流转提示）+ actions。
    #[test]
    fn a_business_state_carries_the_instruction_and_actions() {
        let text = build_template(
            ThinkingState::DEEP_UNDERSTAND,
            2,
            Some(MasterTaskType::CODING),
            Some(SubTaskType::RUST_DEV),
            Some(Difficulty::COMPLEX),
            &[ThinkingState::DEEP_UNDERSTAND],
        );
        assert!(text.contains("第2/3000轮 深度理解阶段 · 编程类-Rust开发 · 复杂"));
        assert!(text.contains("> 编程类任务——注意代码结构、测试覆盖和错误处理"));
        assert!(text.contains("> 🔄 可用流转：本阶段完成可前移至[方案设计(DESIGN)、执行(EXECUTE)、自检验证(VERIFY)]状态；"));
        assert!(text.contains("<constraints spec=\"markdown\">"));
        // 模块自带的 transition 指令行已被清掉，只剩 actions 里那一条。
        assert_eq!(text.matches("asymptotic-think_transition").count(), 1);
    }
}
