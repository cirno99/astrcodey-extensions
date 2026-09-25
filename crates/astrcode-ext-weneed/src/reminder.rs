//! 贴尾风格提醒的正文。
//!
//! 与 [`crate::spec::WE_NEED_SPEC`] 一样，这是**被调优过的英文工件**：模型可见，措辞改动
//! 会改变行为。仓库根 `AGENTS.md` 与本节互为对照，两处不一致即为 bug。
//!
//! 为什么需要它：完整规范只落在 system prompt 的静态前缀区（`PlatformInstructions`），离
//! 上下文尾部很远，而模型对远离尾部的指令遵循度衰减很快——这正是 phi-deepseek-enhanced
//! 「很少进入 We-need 思维链」的主因。AstrCode 没有「往当前用户消息末尾追加」的钩子
//! （`user_message_envelope` 在 S5R worker 上被显式拒绝），但 `provider_contribution`
//! 的 `AppendMessages` 可以在请求组装前追加一条 request-local 消息，落在尾部同一位置。

/// 提醒正文的稳定标记，用于测试与人工核对。
pub const REMINDER_MARKER: &str = "Reasoning style reminder";

/// 追加到请求尾部的提醒正文。
///
/// 措辞刻意**不声称模型已经漂移**：它在每会话首次请求（还没有任何推理可供观测）与检测到
/// 漂移时都会用到，一句「你上一步没按规范」在首轮就是假话，而假前提会让模型去纠正一个
/// 不存在的问题。
pub const REMINDER_TEXT: &str = r#"**Reasoning style reminder:** this session requires the "we need" reasoning style. Open the first sentence of your reasoning with `We need to ...` / `We need ...`; open every following sentence with `We need to ...` / `We need ...`, `I will ...`, or `I am ...` / `I'm ...`. One concrete action per sentence. Classify the task first, then act. Never write reasoning text, or this reminder, into the final reply."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reminder_carries_its_marker() {
        assert!(REMINDER_TEXT.starts_with("**Reasoning style reminder:**"));
        assert!(REMINDER_TEXT.contains(REMINDER_MARKER));
    }

    #[test]
    fn the_reminder_restates_all_three_openers() {
        assert!(REMINDER_TEXT.contains("`We need to ...` / `We need ...`"));
        assert!(REMINDER_TEXT.contains("`I will ...`"));
        assert!(REMINDER_TEXT.contains("`I am ...` / `I'm ...`"));
    }

    /// 提醒只在推理通道里生效，不能改变最终回复的语言与语气。
    #[test]
    fn the_reminder_limits_itself_to_reasoning() {
        assert!(REMINDER_TEXT.contains("Never write reasoning text"));
    }

    /// 提醒不得声称模型已经漂移：首轮注入时那是假前提。
    #[test]
    fn the_reminder_does_not_claim_a_previous_violation() {
        for claim in ["previous", "did not", "drifted", "last reasoning"] {
            assert!(
                !REMINDER_TEXT.contains(claim),
                "提醒不应声称此前的违规：{claim}"
            );
        }
    }

    /// 回归：负向提及（「不要用某个开头词」）本身会把模型往那个开头词上引。
    #[test]
    fn the_reminder_does_not_name_the_replaced_opener() {
        assert!(!REMINDER_TEXT.to_lowercase().contains("let me"));
    }
}
