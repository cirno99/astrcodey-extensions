//! 推理风格漂移判定。
//!
//! 读 assistant 消息的推理通道（`LlmMessage::reasoning_content`）判断它有没有按
//! [`crate::spec::WE_NEED_SPEC`] 的首句硬规则展开。这是 phi-deepseek-enhanced 明确做不到的
//! 一条（PXB 协议不向扩展推 assistant 推理文本），AstrCode 的 `after_provider_response`
//! 钩子会带上完整的消息列表，因此可以落地。
//!
//! 判定只看**首句**，不逐句检查。规范的首句规则是硬性的、且容易判定；而「每句都以三种
//! 开头词之一起手」在真实推理里会被编号列表、代码块、工具叙述大量误伤，逐句判定会把
//! 提醒刷成噪音。

/// 一次推理观测的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drift {
    /// 推理通道为空——没有可判定的观测。
    ///
    /// 不是漂移：DeepSeek 的非思考模式、或别的 provider 不回推理通道时都会落到这里。
    /// 把它当成漂移会让提醒在每一轮无条件刷屏。
    NoObservation,
    /// 首句按规范展开。
    OnStyle,
    /// 首句没有以 `we need` 开头。
    OffStyle,
}

impl Drift {
    pub const fn is_off_style(self) -> bool {
        matches!(self, Self::OffStyle)
    }
}

/// 判定一段推理文本是否偏离规范。
pub fn inspect(reasoning: &str) -> Drift {
    match first_sentence(reasoning) {
        None => Drift::NoObservation,
        Some(sentence) if opens_with_we_need(sentence) => Drift::OnStyle,
        Some(_) => Drift::OffStyle,
    }
}

/// 取首句：剥掉行首装饰，截到第一个句末标点或换行。
fn first_sentence(reasoning: &str) -> Option<&str> {
    let opening = strip_leading_decoration(reasoning);
    if opening.is_empty() {
        return None;
    }
    let end = opening.find(['.', '!', '?', '\n']).unwrap_or(opening.len());
    let sentence = opening[..end].trim();
    (!sentence.is_empty()).then_some(sentence)
}

/// 剥掉行首的 markdown 装饰与有序列表序号。
///
/// 模型常把首句写成 `- We need to ...` 或 `1. We need to ...`；这些仍然是合规的首句。
fn strip_leading_decoration(text: &str) -> &str {
    let text = text.trim_start();
    let text = text.trim_start_matches(['*', '_', '`', '#', '>', '-', ' ', '\t']);
    match ordered_marker(text) {
        Some(rest) => rest.trim_start(),
        None => text,
    }
}

/// 跳过行首的 `1.` / `2)` 这类有序列表序号。
fn ordered_marker(text: &str) -> Option<&str> {
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let rest = &text[digits..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    // 序号后必须跟空白，否则 `3.5` 这类小数会被误当成列表序号。
    rest.starts_with(' ').then_some(rest)
}

/// 首句是否以 `We need to ...` / `We need ...` 起手。
fn opens_with_we_need(sentence: &str) -> bool {
    let lowered = sentence.to_ascii_lowercase();
    let candidate = lowered.trim_start_matches(['"', '\'', '`', '*', '_']);
    let Some(rest) = candidate.strip_prefix("we need") else {
        return false;
    };
    // 词边界：`We needed` / `We needn't` 不算命中。
    match rest.chars().next() {
        None => true,
        Some(character) => {
            character.is_whitespace() || matches!(character, ':' | ',' | '.' | '-' | '—')
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_reasoning_channel_is_not_an_observation() {
        assert_eq!(inspect(""), Drift::NoObservation);
        assert_eq!(inspect("   \n\t "), Drift::NoObservation);
        assert_eq!(inspect("***"), Drift::NoObservation);
    }

    #[test]
    fn the_canonical_openers_are_on_style() {
        for reasoning in [
            "We need to read the failing test first.",
            "we need to read the failing test first.",
            "We need a plan before touching the code.",
            "WE NEED TO CHECK THE LOGS.",
            "We need",
            "We need to...",
        ] {
            assert_eq!(inspect(reasoning), Drift::OnStyle, "{reasoning}");
        }
    }

    /// 只有首句受硬规则约束：后续句子用 `I will` / `I am` 是合规的。
    #[test]
    fn later_sentences_do_not_decide_the_verdict() {
        assert_eq!(
            inspect("We need to read the file. I will open it now. Let me think."),
            Drift::OnStyle
        );
    }

    #[test]
    fn other_openers_are_off_style() {
        for reasoning in [
            "Let me check the file first.",
            "I will read the file.",
            "我需要先读文件。",
            "Looking at the test, it fails.",
        ] {
            assert_eq!(inspect(reasoning), Drift::OffStyle, "{reasoning}");
        }
    }

    /// 首句硬规则要求以 `we need` 起手，`I will` 不能顶替首句位置。
    #[test]
    fn a_modal_opener_in_the_first_sentence_is_still_off_style() {
        assert_eq!(inspect("I am going to read the file."), Drift::OffStyle);
    }

    #[test]
    fn list_and_emphasis_decoration_is_peeled_off() {
        for reasoning in [
            "- We need to read the file.",
            "* We need to read the file.",
            "1. We need to read the file.",
            "2) We need to read the file.",
            "> We need to read the file.",
            "**We need to read the file.**",
            "`We need to read the file`",
            "\"We need to read the file.\"",
            "\n\n   We need to read the file.",
        ] {
            assert_eq!(inspect(reasoning), Drift::OnStyle, "{reasoning}");
        }
    }

    /// 词边界：`we needed` / `we needn't` 与 `we need ...` 不是同一个开头词。
    #[test]
    fn only_the_exact_opener_word_counts() {
        for reasoning in [
            "We needed to check.",
            "We needn't worry.",
            "We needs a plan.",
        ] {
            assert_eq!(inspect(reasoning), Drift::OffStyle, "{reasoning}");
        }
    }

    /// 小数不该被当成有序列表序号而被剥掉。
    #[test]
    fn a_decimal_is_not_an_ordered_marker() {
        assert_eq!(inspect("3.5 We need to check."), Drift::OffStyle);
    }

    /// 首句之前的装饰剥完后只剩标点，等价于没有观测。
    #[test]
    fn decoration_only_input_is_not_an_observation() {
        assert_eq!(inspect("- **  "), Drift::NoObservation);
    }

    #[test]
    fn is_off_style_only_matches_off_style() {
        assert!(Drift::OffStyle.is_off_style());
        assert!(!Drift::OnStyle.is_off_style());
        assert!(!Drift::NoObservation.is_off_style());
    }
}
