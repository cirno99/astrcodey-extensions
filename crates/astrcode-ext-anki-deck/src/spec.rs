//! JSON 牌组规范的类型与校验。
//!
//! 校验刻意保持**纯函数**：不碰文件系统，只查 spec 自洽性。媒体文件的存在性检查
//! 需要 working_dir，放在工具层（`worker` / `apkg`）做。校验失败全部以 `[E_*]` 渲染，
//! 模型能据此自行修正入参。

use serde::Deserialize;

use crate::error::{DeckError, ErrorCode};

/// `anki_write_apkg` 的入参。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckSpecArgs {
    /// 输出 .apkg 路径（相对 working_dir 或绝对路径）。
    pub output: String,
    /// 牌组内容。
    pub deck: DeckSpec,
}

/// 单个牌组（含子牌组层级，用 `::` 表达）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckSpec {
    /// 牌组名；`::` 分层，如 `Rust::Ownership`。
    pub name: String,
    /// 卡片列表，至少一张。
    pub cards: Vec<CardSpec>,
    /// 媒体文件路径；卡片 HTML 里用裸文件名引用。
    #[serde(default)]
    pub media: Vec<String>,
    /// 可选自定义 CSS，覆盖内置样式。
    #[serde(default)]
    pub css: Option<String>,
}

/// 一张卡。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardSpec {
    /// 卡面（HTML）；挖空卡在这里写 `{{c1::…}}`。
    pub front: String,
    /// 卡背（HTML）；挖空卡里是挖空后显示的补充。
    #[serde(default)]
    pub back: String,
    /// 是否挖空卡；为 true 时 front 必须含 `{{cN::…}}`。
    #[serde(default)]
    pub cloze: bool,
    /// Anki 标签，不允许空白。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 稳定身份（如 `vault:notes/rust.md#所有权`）。与牌组名一起决定 GUID，
    /// 缺省时退回按卡面内容派生（genanki 行为）。
    #[serde(default)]
    pub id: Option<String>,
}

/// 校验 spec 的纯自洽性。通过则原样返回（借用不变），失败带首个错误。
///
/// 有意只报**第一个**错误：模型拿到一条明确修正指引就够了，一次泼十条反而稀释。
pub fn validate(spec: &DeckSpecArgs) -> Result<(), DeckError> {
    validate_deck(&spec.deck)
}

fn validate_deck(deck: &DeckSpec) -> Result<(), DeckError> {
    let name = deck.name.trim();
    if name.is_empty() {
        return Err(DeckError::new(
            ErrorCode::DeckName,
            "deck.name must not be empty",
        ));
    }
    if deck.name != deck.name.trim() {
        return Err(DeckError::new(
            ErrorCode::DeckName,
            format!(
                "deck.name has leading/trailing whitespace: {:?}; trim it",
                deck.name
            ),
        ));
    }
    if deck.name.contains('"') {
        return Err(DeckError::new(
            ErrorCode::DeckName,
            format!(
                "deck.name must not contain double quotes (Anki rejects them): {:?}",
                deck.name
            ),
        ));
    }
    if deck.cards.is_empty() {
        return Err(DeckError::new(
            ErrorCode::NoCards,
            format!("deck {:?} has no cards", deck.name),
        ));
    }

    for (index, card) in deck.cards.iter().enumerate() {
        if card.front.trim().is_empty() {
            return Err(DeckError::new(
                ErrorCode::EmptyFront,
                format!("cards[{index}].front is empty"),
            ));
        }
        if card.cloze && extract_cloze_ords(&card.front).is_empty() {
            return Err(DeckError::new(
                ErrorCode::BadCloze,
                format!(
                    "cards[{index}] has cloze:true but its front contains no {{{{cN::…}}}} marker"
                ),
            ));
        }
        for tag in &card.tags {
            if tag.trim().is_empty() || tag.split_whitespace().count() != 1 {
                return Err(DeckError::new(
                    ErrorCode::TagWhitespace,
                    format!(
                        "cards[{index}] has tag {:?}; tags must not contain whitespace",
                        tag
                    ),
                ));
            }
        }
    }

    Ok(())
}

/// 从卡面提取挖空序号（`{{cN::…}}` 的 N，返回 N-1 供 `cards.ord` 用）。
///
/// 对齐 genanki `_cloze_cards` 的语义：只认正数序号，重复序号去重，`{{c0::}}` 忽略。
/// 手写扫描而不上 regex：模式极简单，不值得为它引入正则引擎。
pub fn extract_cloze_ords(front: &str) -> Vec<u32> {
    let mut ords = Vec::new();
    let bytes = front.as_bytes();
    let mut index = 0;
    while let Some(position) = front[index..].find("{{c") {
        let start = index + position + 3;
        let mut digits_end = start;
        while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
            digits_end += 1;
        }
        // 形如 `{{c12::` 才算数：数字后必须紧跟 `::`，且至少一位数字。
        let is_marker =
            digits_end > start && front[digits_end..].starts_with("::");
        if is_marker {
            // ASCII 数字切片转 u32 不会失败。
            let number: u32 = front[start..digits_end].parse().expect("ascii digits");
            if number > 0 && !ords.contains(&(number - 1)) {
                ords.push(number - 1);
            }
        }
        index = digits_end.max(start + 1);
    }
    ords
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with_cards(cards: Vec<CardSpec>) -> DeckSpecArgs {
        DeckSpecArgs {
            output: String::from("out.apkg"),
            deck: DeckSpec {
                name: String::from("Rust"),
                cards,
                media: Vec::new(),
                css: None,
            },
        }
    }

    fn card(front: &str) -> CardSpec {
        CardSpec {
            front: front.to_owned(),
            back: String::from("back"),
            cloze: false,
            tags: Vec::new(),
            id: None,
        }
    }

    #[test]
    fn accepts_a_minimal_valid_spec() {
        let spec = spec_with_cards(vec![card("What is a lifetime?")]);
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn rejects_empty_deck_name() {
        let mut spec = spec_with_cards(vec![card("q")]);
        spec.deck.name = String::from("   ");
        let error = validate(&spec).expect_err("应拒绝空牌组名");
        assert_eq!(error.code, ErrorCode::DeckName);
    }

    #[test]
    fn rejects_deck_names_with_double_quotes() {
        let mut spec = spec_with_cards(vec![card("q")]);
        spec.deck.name = String::from("Rust \"quoted\"");
        let error = validate(&spec).expect_err("应拒绝带引号的牌组名");
        assert_eq!(error.code, ErrorCode::DeckName);
    }

    #[test]
    fn rejects_a_deck_without_cards() {
        let spec = spec_with_cards(Vec::new());
        let error = validate(&spec).expect_err("应拒绝空牌组");
        assert_eq!(error.code, ErrorCode::NoCards);
    }

    #[test]
    fn rejects_blank_front() {
        let spec = spec_with_cards(vec![card("q"), card("   ")]);
        let error = validate(&spec).expect_err("应拒绝空白卡面");
        assert_eq!(error.code, ErrorCode::EmptyFront);
        assert!(error.message.contains("cards[1]"));
    }

    #[test]
    fn rejects_cloze_flag_without_marker() {
        let mut cloze = card("What is a lifetime?");
        cloze.cloze = true;
        let spec = spec_with_cards(vec![cloze]);
        let error = validate(&spec).expect_err("应拒绝无标记的挖空卡");
        assert_eq!(error.code, ErrorCode::BadCloze);
    }

    #[test]
    fn accepts_cloze_flag_with_marker() {
        let mut cloze = card("A lifetime is {{c1::how long::注}} a reference lives");
        cloze.cloze = true;
        let spec = spec_with_cards(vec![cloze]);
        assert!(validate(&spec).is_ok());
    }

    #[test]
    fn rejects_tags_with_whitespace() {
        let mut tagged = card("q");
        tagged.tags = vec![String::from("rust lifetime")];
        let spec = spec_with_cards(vec![tagged]);
        let error = validate(&spec).expect_err("应拒绝带空白的标签");
        assert_eq!(error.code, ErrorCode::TagWhitespace);
    }

    #[test]
    fn cloze_extraction_dedupes_and_ignores_zero_and_nonmarkers() {
        let front = "{{c2::b}} and {{c2::b again}} and {{c1::a}} and {{c0::no}} \
                     and {{c::bad}} and {{c x::bad}}";
        assert_eq!(extract_cloze_ords(front), vec![1, 0]);
    }

    #[test]
    fn cloze_extraction_returns_empty_for_plain_text() {
        assert!(extract_cloze_ords("no markers here").is_empty());
        // `{{c1:}}`（单冒号）不是合法挖空标记。
        assert!(extract_cloze_ords("{{c1:half}}").is_empty());
    }
}
