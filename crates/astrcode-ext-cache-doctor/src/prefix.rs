//! Provider 可见消息的前缀指纹与断点判定。
//!
//! # 为什么是「逐消息指纹 + 最长公共前缀」而不是文本 diff
//!
//! provider 的 prompt 缓存是**前缀缓存**：只有从第一条消息开始逐字节一致的部分才有
//! 可能命中。所以真正要回答的问题不是「哪里变了」，而是「共同前缀有多长、断点落在
//! 第几条消息上」。逐消息指纹正好给出这个粒度，且不需要保留正文。
//!
//! # 不保留正文
//!
//! 指纹是消息序列化字节的 FNV-1a 哈希；摘要只记录结构标签与字节数。插件在内存里
//! 留下的是「第 7 条 tool 消息，8.2K 字符」这样的结构性事实，而不是 prompt 内容。

use astrcode_extension_sdk::llm::{LlmContent, LlmMessage};
use astrcode_ext_common::paths::fnv1a_bytes;

/// 消息的结构标签：只区分形态，不含正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    System,
    User,
    AssistantText,
    AssistantToolCalls,
    AssistantTextAndToolCalls,
    /// 既无文本也无工具调用的 assistant 消息（正常不应出现，但要能如实报告）。
    AssistantEmpty,
    Tool,
}

impl Label {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::AssistantText => "assistant(text)",
            Self::AssistantToolCalls => "assistant(tool_calls)",
            Self::AssistantTextAndToolCalls => "assistant(text+tool_calls)",
            Self::AssistantEmpty => "assistant(empty)",
            Self::Tool => "tool",
        }
    }
}

/// 单条 provider 可见消息的指纹与体量。
///
/// `bytes` 是序列化后的字节数，用来近似「这条消息占多少 prompt」；它不参与相等判定，
/// 相等判定只看 [`MessageDigest::fingerprint`] 与 [`MessageDigest::label`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageDigest {
    pub label: Label,
    pub bytes: usize,
    pub fingerprint: u64,
}

/// 为整条消息序列计算指纹。
///
/// 序列化缓冲在调用内复用，因此热路径上只有一次分配。指纹覆盖角色的序列化形状与
/// 全部内容字段，任何一处字节变化都会改变指纹。
pub fn digest_messages(messages: &[LlmMessage]) -> Vec<MessageDigest> {
    let mut buffer = Vec::new();
    messages
        .iter()
        .map(|message| digest_message(message, &mut buffer))
        .collect()
}

fn digest_message(message: &LlmMessage, buffer: &mut Vec<u8>) -> MessageDigest {
    buffer.clear();
    let fingerprint = match serde_json::to_writer(&mut *buffer, message) {
        Ok(()) => fnv1a_bytes(buffer),
        // `LlmMessage` 的字段都能序列化，这条分支实际不可达。真发生时用角色与体量
        // 兜底：宁可指纹退化，也不能让 provider 请求路径上的钩子报错。
        Err(_) => fnv1a_bytes(format!("{}:{}", message.role.as_str(), buffer.len()).as_bytes()),
    };
    MessageDigest {
        label: label_of(message),
        bytes: buffer.len(),
        fingerprint,
    }
}

fn label_of(message: &LlmMessage) -> Label {
    match message.role {
        astrcode_extension_sdk::llm::LlmRole::System => Label::System,
        astrcode_extension_sdk::llm::LlmRole::User => Label::User,
        astrcode_extension_sdk::llm::LlmRole::Tool => Label::Tool,
        astrcode_extension_sdk::llm::LlmRole::Assistant => {
            let text = message
                .content
                .iter()
                .any(|content| matches!(content, LlmContent::Text { text } if !text.is_empty()));
            let calls = message
                .content
                .iter()
                .filter(|content| matches!(content, LlmContent::ToolCall { .. }))
                .count();
            match (text, calls) {
                (true, 0) => Label::AssistantText,
                (false, 0) => Label::AssistantEmpty,
                (true, _) => Label::AssistantTextAndToolCalls,
                (false, _) => Label::AssistantToolCalls,
            }
        },
    }
}

/// 一次请求相对上一次请求的前缀对比。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixReport {
    pub previous_len: usize,
    pub current_len: usize,
    /// 从第 0 条起完全一致的消息条数。
    pub common: usize,
}

impl PrefixReport {
    /// 上一次请求的消息是否全部落在本次请求的前缀里。
    ///
    /// 为真意味着本次只是在历史之后追加，provider 的前缀缓存仍然可用。
    pub fn preserved(&self) -> bool {
        self.common == self.previous_len
    }

    /// 断点位置：第 `common` 条消息起与上一次不同。前缀完整时为 `None`。
    pub fn break_index(&self) -> Option<usize> {
        (!self.preserved()).then_some(self.common)
    }

    /// 本次相对上一次新增的消息条数。
    pub fn added(&self) -> usize {
        self.current_len.saturating_sub(self.previous_len)
    }

    /// 本次请求比上一次短：历史被裁剪（例如上下文压缩）。
    pub fn truncated(&self) -> bool {
        self.current_len < self.previous_len
    }

    /// 上一次请求的消息里仍被本次复用的比例。
    pub fn preserved_ratio(&self) -> f64 {
        if self.previous_len == 0 {
            1.0
        } else {
            self.common as f64 / self.previous_len as f64
        }
    }
}

/// 求最长公共前缀。相等判定是逐条指纹比较，因此比较代价与消息数成正比，与正文无关。
pub fn compare(previous: &[MessageDigest], current: &[MessageDigest]) -> PrefixReport {
    let common = previous
        .iter()
        .zip(current)
        .take_while(|(left, right)| left == right)
        .count();
    PrefixReport {
        previous_len: previous.len(),
        current_len: current.len(),
        common,
    }
}

#[cfg(test)]
mod tests {
    use astrcode_extension_sdk::llm::{LlmContent, LlmMessage, LlmRole};
    use serde_json::json;

    use super::*;

    fn tool_call(call_id: &str, name: &str, arguments: serde_json::Value) -> LlmMessage {
        LlmMessage {
            role: LlmRole::Assistant,
            content: vec![LlmContent::ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                arguments,
                raw_arguments: None,
            }],
            name: None,
            reasoning_content: None,
        }
    }

    fn digests(messages: &[LlmMessage]) -> Vec<MessageDigest> {
        digest_messages(messages)
    }

    #[test]
    fn labels_distinguish_assistant_shapes() {
        assert_eq!(label_of(&LlmMessage::system("s")), Label::System);
        assert_eq!(label_of(&LlmMessage::user("u")), Label::User);
        assert_eq!(label_of(&LlmMessage::assistant("a")), Label::AssistantText);
        assert_eq!(label_of(&LlmMessage::tool("shell", "c1", "out", false)), Label::Tool);
        assert_eq!(
            label_of(&tool_call("c1", "shell", json!({}))),
            Label::AssistantToolCalls
        );
        assert_eq!(
            label_of(&LlmMessage {
                role: LlmRole::Assistant,
                content: vec![
                    LlmContent::Text { text: "hi".into() },
                    LlmContent::ToolCall {
                        call_id: "c1".into(),
                        name: "shell".into(),
                        arguments: json!({}),
                        raw_arguments: None,
                    },
                ],
                name: None,
                reasoning_content: None,
            }),
            Label::AssistantTextAndToolCalls
        );
        assert_eq!(
            label_of(&LlmMessage {
                role: LlmRole::Assistant,
                content: Vec::new(),
                name: None,
                reasoning_content: None,
            }),
            Label::AssistantEmpty
        );
    }

    #[test]
    fn an_empty_text_block_does_not_count_as_text() {
        let message = LlmMessage {
            role: LlmRole::Assistant,
            content: vec![LlmContent::Text { text: String::new() }],
            name: None,
            reasoning_content: None,
        };
        assert_eq!(label_of(&message), Label::AssistantEmpty);
    }

    #[test]
    fn identical_messages_share_a_fingerprint() {
        let a = digests(&[LlmMessage::user("hello")]);
        let b = digests(&[LlmMessage::user("hello")]);
        assert_eq!(a, b);
        assert!(a[0].bytes > 0);
    }

    #[test]
    fn any_content_change_moves_the_fingerprint() {
        let base = digests(&[LlmMessage::user("hello")]);
        for changed in [
            LlmMessage::user("hello!"),
            LlmMessage::user("Hello"),
            LlmMessage::assistant("hello"),
        ] {
            assert_ne!(
                base[0].fingerprint,
                digests(&[changed])[0].fingerprint,
                "内容或角色变化必须改变指纹"
            );
        }
    }

    #[test]
    fn reasoning_content_is_part_of_the_fingerprint() {
        let plain = LlmMessage::assistant("a");
        let mut with_reasoning = LlmMessage::assistant("a");
        with_reasoning.reasoning_content = Some("because".into());
        assert_ne!(
            digests(&[plain])[0].fingerprint,
            digests(&[with_reasoning])[0].fingerprint
        );
    }

    #[test]
    fn an_appended_history_keeps_the_prefix() {
        let previous = digests(&[LlmMessage::user("one")]);
        let current = digests(&[
            LlmMessage::user("one"),
            LlmMessage::assistant("two"),
            LlmMessage::user("three"),
        ]);
        let report = compare(&previous, &current);
        assert!(report.preserved());
        assert_eq!(report.break_index(), None);
        assert_eq!(report.added(), 2);
        assert!(!report.truncated());
        assert_eq!(report.preserved_ratio(), 1.0);
    }

    #[test]
    fn an_identical_repeat_is_preserved_with_no_additions() {
        let previous = digests(&[LlmMessage::user("one"), LlmMessage::assistant("two")]);
        let report = compare(&previous, &previous);
        assert!(report.preserved());
        assert_eq!(report.added(), 0);
    }

    #[test]
    fn rewriting_an_earlier_message_breaks_the_prefix_there() {
        let previous = digests(&[
            LlmMessage::user("one"),
            LlmMessage::assistant("two"),
            LlmMessage::user("three"),
        ]);
        let current = digests(&[
            LlmMessage::user("one"),
            LlmMessage::assistant("TWO"),
            LlmMessage::user("three"),
            LlmMessage::assistant("four"),
        ]);
        let report = compare(&previous, &current);
        assert!(!report.preserved());
        assert_eq!(report.break_index(), Some(1));
        assert_eq!(report.preserved_ratio(), 1.0 / 3.0);
        assert!(!report.truncated());
    }

    #[test]
    fn a_shorter_request_reports_truncation() {
        let previous = digests(&[
            LlmMessage::user("one"),
            LlmMessage::assistant("two"),
            LlmMessage::user("three"),
        ]);
        let current = digests(&[LlmMessage::user("one")]);
        let report = compare(&previous, &current);
        assert!(!report.preserved());
        assert!(report.truncated());
        assert_eq!(report.break_index(), Some(1));
        assert_eq!(report.added(), 0);
    }

    #[test]
    fn a_request_that_extends_a_truncated_history_still_reports_the_break() {
        let previous = digests(&[LlmMessage::user("one"), LlmMessage::user("two")]);
        let current = digests(&[LlmMessage::user("one"), LlmMessage::user("TWO"), LlmMessage::user("three")]);
        let report = compare(&previous, &current);
        assert_eq!(report.break_index(), Some(1));
        assert_eq!(report.added(), 1);
    }

    #[test]
    fn an_empty_previous_request_is_trivially_preserved() {
        let report = compare(&[], &digests(&[LlmMessage::user("one")]));
        assert!(report.preserved());
        assert_eq!(report.preserved_ratio(), 1.0);
        assert_eq!(report.added(), 1);
    }

    #[test]
    fn identical_byte_lengths_do_not_make_different_content_equal() {
        let left = digests(&[LlmMessage::assistant("aaaa")]);
        let right = digests(&[LlmMessage::assistant("bbbb")]);
        assert_eq!(left[0].bytes, right[0].bytes);
        assert_ne!(left[0].fingerprint, right[0].fingerprint);
    }
}
