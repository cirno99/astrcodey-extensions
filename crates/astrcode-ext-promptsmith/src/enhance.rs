//! 调 enhancer 模型做改写，并把回包清洗成可直接发送的提示词正文。
//!
//! # 取值点
//!
//! 改写发生在 `/smith` 命令处理器里（会话空闲时），不在任何 prompt 注入钩子上。宿主给磁盘
//! 扩展的改写接入点只有 `ExtensionCommandResult::StartTurn`：它的 `instructions` **整体替换**
//! 用户消息，原始的 `/smith …` 命令文本一个字都不会进 transcript
//! （`astrcode-server/src/session_command_service.rs:426-432`）。上游那种「读编辑器草稿并原地
//! 改写」做不到——`user_message_envelope` 钩子对磁盘扩展是显式忽略的
//! （`astrcode-extensions/src/s5r_ext/mod.rs:324`）。
//!
//! # 回包清洗比上游宽松
//!
//! 上游要求「恰好一个哨兵块，且块外不得有任何文本」，否则整次改写判为失败。这里不这么做，
//! 理由是本插件默认只预览：清洗结果先经人的眼睛，再决定发不发。判失败让用户重跑一次，代价
//! 比「拿到一段带前言的改写、由用户自己删掉前言」更大。因此 [`extract`] 分四种形态，并把形态
//! 随结果一起带出去，让预览能如实标注可信度。

use astrcode_extension_sdk::llm::LlmMessage;
use astrcode_extension_worker::worker_prelude::*;

use crate::{
    config::{Config, Enhancer},
    intent::{self, EffectiveMode, TaskIntent},
    prompt::{self, MAX_OUTPUT_TOKENS, SENTINEL_CLOSE, SENTINEL_OPEN},
};

/// 一次改写的产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// 清洗后的提示词正文，可直接作为 `StartTurn` 的 instructions。
    pub prompt: String,
    pub intent: TaskIntent,
    pub mode: EffectiveMode,
    /// 实际打到哪个档位；配置请求 small 但宿主没配小模型时会是 `"main"`。
    pub enhancer_used: &'static str,
    /// 是否发生了档位回落（配置与实际不一致），预览里要如实说明。
    pub enhancer_fell_back: bool,
    /// 回包提取形态。
    pub extraction: Extraction,
}

/// 回包提取形态。四种都要在预览里说清楚：用户在决定发不发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extraction {
    /// 恰好一个闭合的哨兵块。
    Sentinel,
    /// 有多个哨兵块，取了第一个，其余丢弃。
    MultipleBlocks,
    /// 哨块有开无闭，取了开标签之后的全部剩余。
    UnclosedBlock,
    /// 完全没有哨兵块，整段回包当正文（只剥外层围栏）。
    WholeResponse,
}

impl Extraction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sentinel => "sentinel",
            Self::MultipleBlocks => "multiple-blocks",
            Self::UnclosedBlock => "unclosed-block",
            Self::WholeResponse => "whole-response",
        }
    }

    /// 预览里对该形态的一句话说明；`Sentinel` 是理想形态，不额外啰嗦。
    pub fn note(self) -> Option<&'static str> {
        match self {
            Self::Sentinel => None,
            Self::MultipleBlocks => Some("回包里有多个哨兵块，已取第一段，其余丢弃。"),
            Self::UnclosedBlock => Some("回包的哨兵块没有闭合，取了开标签之后的全部内容。"),
            Self::WholeResponse => Some("回包里没有哨兵块，整段模型输出直接当正文。"),
        }
    }
}

/// 改写失败的可展示原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhanceError(pub String);

impl EnhanceError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// 改写一条草稿。
///
/// `model_id` 只用于写进上下文区，告诉 enhancer 改写结果最终要给哪个模型用；家族判定已经
/// 不做（见 [`crate::prompt`] 模块头）。
pub async fn enhance(draft: &str, config: &Config, model_id: &str) -> Result<Outcome, EnhanceError> {
    let draft = validate_draft(draft)?;

    let intent = intent::detect(draft);
    let mode = intent::resolve_effective_mode(config.mode, intent);
    let messages = prompt::build_request(draft, intent, mode, config.strength, model_id);

    let (enhancer_used, fell_back, output) = call(config.enhancer, messages).await?;

    let extracted = extract(&output.content);
    if extracted.text.trim().is_empty() {
        return Err(EnhanceError::new(format!(
            "改写模型回了空正文（档位 {}，提取 {}）。可以再试一次，或 `/smith enhancer main` 换个档位。",
            enhancer_used,
            extracted.extraction.as_str()
        )));
    }

    Ok(Outcome {
        prompt: normalize_body(&extracted.text),
        intent,
        mode,
        enhancer_used,
        enhancer_fell_back: fell_back,
        extraction: extracted.extraction,
    })
}

/// 空草稿在进入任何模型调用之前判掉，返回修剪后的草稿。
fn validate_draft(draft: &str) -> Result<&str, EnhanceError> {
    let trimmed = draft.trim();
    if trimmed.is_empty() {
        return Err(EnhanceError::new("草稿是空的，没什么可改写。"));
    }
    Ok(trimmed)
}

/// 一次提取的结果。
struct Extracted {
    text: String,
    extraction: Extraction,
}

/// 打一次模型。返回 `(实际档位, 是否回落, 回包)`。
async fn call(
    requested: Enhancer,
    messages: Vec<LlmMessage>,
) -> Result<(&'static str, bool, HostLlmChatOutput), EnhanceError> {
    // 请求 small 而宿主没配小模型时回落 main：小模型档位是可选配置，不该让改写因此不可用。
    // host_supports 报错只可能是「不在 worker 上下文里」，当成「不可用」走回落更稳。
    let use_small = requested == Enhancer::Small
        && HostClient::host_supports(HostOperation::LlmSmallChat).unwrap_or(false);

    let request = llm_chat_request(messages).with_max_output_tokens(MAX_OUTPUT_TOKENS);

    if use_small {
        return HostClient::models()
            .small_chat_collected_request(request)
            .await
            .map(|output| ("small", false, output))
            .map_err(|error| EnhanceError::new(format!("小模型改写调用失败：{error}")));
    }

    let fell_back = requested == Enhancer::Small;
    if !HostClient::host_supports(HostOperation::LlmMainChat).unwrap_or(false) {
        return Err(EnhanceError::new(
            "宿主既没有可用的小模型也没有可用的主模型，改写没法进行。",
        ));
    }
    HostClient::models()
        .main_chat_collected_request(request)
        .await
        .map(|output| ("main", fell_back, output))
        .map_err(|error| EnhanceError::new(format!("主模型改写调用失败：{error}")))
}

/// 从回包里取正文。规则见模块头「回包清洗比上游宽松」。
fn extract(response: &str) -> Extracted {
    match find_blocks(response).as_slice() {
        [] => Extracted {
            text: strip_code_fence(response),
            extraction: Extraction::WholeResponse,
        },
        [only] => only.into(),
        // 多块时标签要说「多块」而不是第一个块自身的形态：块自身的形态（未闭合）不可能与
        // 多块共存——遇到未闭合就直接返回单块了，见 [`find_blocks`]。
        [first, ..] => Extracted {
            text: first.text.clone(),
            extraction: Extraction::MultipleBlocks,
        },
    }
}

/// 一个已发现的哨兵块。
struct Block {
    text: String,
    extraction: Extraction,
}

/// 找出所有 `<smith-prompt>…</smith-prompt>` 段。
///
/// 手写扫描而不是正则：哨兵是固定字面量，一次遍历就能同时处理「有开无闭」「多块」「闭标签
/// 先于开标签」这些畸形形态，而小模型恰恰最容易回这些。
fn find_blocks(response: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cursor = 0usize;

    while let Some(open_at) = find_from(response, cursor, SENTINEL_OPEN) {
        let content_start = open_at + SENTINEL_OPEN.len();
        match find_from(response, content_start, SENTINEL_CLOSE) {
            Some(close_at) => {
                blocks.push(Block {
                    text: response[content_start..close_at].to_owned(),
                    extraction: Extraction::Sentinel,
                });
                cursor = close_at + SENTINEL_CLOSE.len();
            },
            None => {
                // 有开无闭：把后面全部当正文并立即收尾。继续往后找只会把同一块内容重复计一次。
                blocks.push(Block {
                    text: response[content_start..].to_owned(),
                    extraction: Extraction::UnclosedBlock,
                });
                return blocks;
            },
        }
    }

    blocks
}

/// 从 `from` 字节处往后找 `needle`，返回相对整个 `haystack` 的下标。
///
/// `from` 恒由 ASCII 哨兵标签长度累加而来，一定落在字符边界上，因此切片不会 panic。
fn find_from(haystack: &str, from: usize, needle: &str) -> Option<usize> {
    haystack.get(from..)?.find(needle).map(|index| from + index)
}

/// 剥掉正文外层的东西：BOM、CRLF、整段代码围栏、首尾空白。
fn normalize_body(text: &str) -> String {
    let without_bom = text.trim_start_matches('\u{feff}');
    let unified = without_bom.replace("\r\n", "\n");
    strip_code_fence(&unified).trim().to_owned()
}

/// 剥掉**整段**被一层 ``` 围栏包住的情况。
///
/// 只处理整段被包住的情形：正文内部的代码块必须原样留着，那是用户自己的命令与路径。
fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    let Some(after_marker) = trimmed.strip_prefix("```") else {
        return text.to_owned();
    };
    // 围栏首行可以带语言标注（```text），正文从首个换行之后开始。
    let Some(newline) = after_marker.find('\n') else {
        return text.to_owned();
    };
    let body = &after_marker[newline + 1..];
    let Some(close_start) = body.rfind("```") else {
        return text.to_owned();
    };
    let inner = body[..close_start].trim();
    if inner.is_empty() {
        text.to_owned()
    } else {
        inner.to_owned()
    }
}

impl From<&Block> for Extracted {
    fn from(block: &Block) -> Self {
        Self {
            text: block.text.clone(),
            extraction: block.extraction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_draft_is_rejected_without_a_model_call() {
        let error = validate_draft("   ").unwrap_err();
        assert!(error.0.contains("空"), "空草稿要给人话原因");
        assert!(validate_draft("  add a toggle ").is_ok());
    }

    #[test]
    fn single_sentinel_block_is_extracted_and_outside_text_dropped() {
        let extracted = extract("前置废话 <smith-prompt>\nGoal\nfix it\n</smith-prompt> 尾部废话");
        assert_eq!(extracted.extraction, Extraction::Sentinel);
        assert_eq!(extracted.text, "\nGoal\nfix it\n");
    }

    #[test]
    fn multiple_blocks_take_the_first_and_are_flagged() {
        let extracted =
            extract("<smith-prompt>first</smith-prompt><smith-prompt>second</smith-prompt>");
        assert_eq!(extracted.extraction, Extraction::MultipleBlocks);
        assert_eq!(extracted.text, "first");
    }

    /// 有开无闭：取到结尾，不能因为没闭合就丢整段——小模型最常见的畸形形态就是这个。
    #[test]
    fn unclosed_block_falls_back_to_the_remainder() {
        let extracted = extract("<smith-prompt>\nGoal\nadd a toggle");
        assert_eq!(extracted.extraction, Extraction::UnclosedBlock);
        assert_eq!(extracted.text, "\nGoal\nadd a toggle");
    }

    #[test]
    fn missing_sentinel_uses_whole_response_and_is_flagged() {
        let extracted = extract("Here is the rewrite:\nGoal\nadd a toggle");
        assert_eq!(extracted.extraction, Extraction::WholeResponse);
        assert!(extracted.text.contains("Goal"));
    }

    /// 闭标签出现在开标签之前的畸形回包不能 panic，也不能把闭标签前的噪声当正文。
    #[test]
    fn stray_close_tag_before_open_does_not_panic() {
        let extracted = extract("</smith-prompt>noise<smith-prompt>body</smith-prompt>");
        assert_eq!(extracted.text, "body");
        assert_eq!(extracted.extraction, Extraction::Sentinel);
    }

    #[test]
    fn whole_fences_are_stripped_but_inner_inline_code_survives() {
        let normalized = normalize_body("```text\nrun `cargo test`\n\nkeep this\n```");
        assert!(normalized.contains("`cargo test`"), "正文里的行内代码不能被吃掉");
        assert!(normalized.contains("keep this"));
        assert!(!normalized.contains("```text"));
    }

    #[test]
    fn crlf_and_bom_are_normalized() {
        assert_eq!(normalize_body("\u{feff}line one\r\nline two  "), "line one\nline two");
    }

    #[test]
    fn unclosed_fence_is_left_untouched() {
        // 模型只开了个围栏没收：宁可留着 ``` 也不要吞掉正文。
        let normalized = normalize_body("```\nGoal\nfix it");
        assert!(normalized.contains("Goal"));
        assert!(normalized.contains("```"));
    }

    #[test]
    fn empty_block_body_is_not_a_silent_success() {
        // `<smith-prompt></smith-prompt>` 提取出来是空串，enhance 会走空正文分支；
        // 这里钉住提取层的行为，别让上层误判成「有正文」。
        let extracted = extract("<smith-prompt>   </smith-prompt>");
        assert_eq!(extracted.extraction, Extraction::Sentinel);
        assert!(extracted.text.trim().is_empty());
    }

    #[test]
    fn extraction_labels_and_notes_are_distinct() {
        assert_eq!(Extraction::Sentinel.as_str(), "sentinel");
        assert_eq!(Extraction::MultipleBlocks.as_str(), "multiple-blocks");
        assert_eq!(Extraction::UnclosedBlock.as_str(), "unclosed-block");
        assert_eq!(Extraction::WholeResponse.as_str(), "whole-response");
        assert_eq!(Extraction::Sentinel.note(), None, "理想形态不该啰嗦");
        assert!(Extraction::UnclosedBlock.note().is_some());
    }

    #[test]
    fn find_from_is_boundary_safe_and_offset_correct() {
        let text = "中文前缀<smith-prompt>正文</smith-prompt>";
        let open = find_from(text, 0, SENTINEL_OPEN).expect("应找到开标签");
        assert_eq!(&text[open..open + SENTINEL_OPEN.len()], SENTINEL_OPEN);
        assert_eq!(find_from(text, text.len(), SENTINEL_OPEN), None);
    }
}
