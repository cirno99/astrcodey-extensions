//! 「we need」思维链引导规范正文。
//!
//! 正文移植自 <https://github.com/scp3500/oh-we-need>（MIT）经
//! <https://github.com/Nwflower/dsh-weneed>（MIT）包装的 DeepSeek V4 特化思维链规范，
//! 7 条语义保持一致。移植到 AstrCode 时有两点必要偏离，见 [`WE_NEED_SPEC`] 的文档。

/// 注入到 system prompt 的规范正文。
///
/// 与原版的两点偏离：
///
/// 1. **去掉身份句**。原版首段含 `You are a helpful software engineer assistant.`，
///    用于给思维链定人设。AstrCode 的 Identity 段已经承担这个职责，重复声明会让
///    两段互相竞争，因此这里只保留风格指令。
/// 2. **第 6 条改为推理通道**。原版要求把推理写进 ` thinking` 标签，那是 DSH 的承载方式；
///    AstrCode 走 provider 原生的 `reasoning_content` 字段（`astrcode-core::llm::LlmMessage`），
///    模型若照抄 ` thinking` 标签会把它当成可见正文输出，正好违反该条后半句。
pub const WE_NEED_SPEC: &str = r#"When you think, start with "we need...". We need to follow this style for all internal reasoning (chain-of-thought):

1. **`we need to ...` / `we need ...` is the core pattern.** Either can open any sentence, not only the first. We need one concrete action per sentence.
2. **Interleave modal verbs:** I'll (next action) · I can (viable option) · I need (what must be done) · I should (what ought to be done) · I will (committed step).
3. **Avoid `let me ...`.** We need to prefer `we need to ...` / `we need ...` for opening steps.
4. **Short and colloquial.** We need one sentence per step, decision-level summaries only, we / I perspective.
5. **Classify every task first.** We need to pick a stable end: build (produce, verify, fix) · fix (read, locate, minimal change, verify) · weak (classify first, then build or fix).
6. **Reasoning channel only.** We need every reasoning step written in the reasoning channel, never in the final reply. We need to never emit reasoning text or reasoning tags as visible output.
7. **Scope.** We need this to shape reasoning only. Final replies follow the user's language and tone."#;

/// 规范出处，用于 `/weneed status` 与 README 的归属说明。
pub const SPEC_SOURCE: &str = "scp3500/oh-we-need (MIT) → Nwflower/dsh-weneed (MIT)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_keeps_all_seven_rules() {
        for rule in 1..=7 {
            let marker = format!("\n{rule}. **");
            assert!(
                WE_NEED_SPEC.contains(&marker),
                "规范缺少第 {rule} 条：{marker}"
            );
        }
    }

    #[test]
    fn spec_anchors_on_the_we_need_opening() {
        assert!(WE_NEED_SPEC.starts_with("When you think, start with \"we need...\"."));
    }

    /// 第 6 条必须指向推理通道，且不能把 ` thinking` 标签写成要求。
    /// 模型照抄标签会把它输出成可见正文，正好破坏该条的意图。
    #[test]
    fn spec_rule_six_targets_the_reasoning_channel_not_a_visible_tag() {
        assert!(WE_NEED_SPEC.contains("Reasoning channel only."));
        assert!(!WE_NEED_SPEC.contains("thinking tag"));
    }

    /// 身份句由 AstrCode 的 Identity 段提供，规范正文不得重复声明。
    #[test]
    fn spec_does_not_redeclare_an_identity() {
        assert!(!WE_NEED_SPEC.contains("You are a helpful software engineer assistant"));
        assert!(!WE_NEED_SPEC.contains("You are Astrcode"));
    }

    /// 第 7 条保证规范只塑形推理，不改变最终回复的语言与语气。
    #[test]
    fn spec_limits_itself_to_reasoning() {
        assert!(WE_NEED_SPEC.contains("Final replies follow the user's language and tone."));
    }
}
