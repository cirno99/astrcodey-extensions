//! 工具层的领域错误。
//!
//! 与 hashline-edit 的 `EditError` 同构：`[E_*]` 前缀的渲染文本会以
//! `is_error = true` 的工具结果逐字抵达模型，模型据此自行修正入参重试；
//! 只有线程池等基础设施故障才走 `Err(ErrorPayload)`。

/// 错误码，渲染成 `[E_XXX]` 前缀。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// 牌组里一张卡都没有。
    NoCards,
    /// front 为空（或全空白）。
    EmptyFront,
    /// `cloze: true` 但 front 里没有 `{{cN::…}}` 标记。
    BadCloze,
    /// 标签含空白（Anki 标签以空格分隔，不允许内部空白）。
    TagWhitespace,
    /// 牌组名非法（空、带引号、首尾空白）。
    DeckName,
    /// 媒体文件不存在或不是普通文件。
    MediaMissing,
    /// 两个媒体源文件归一化后是同一个 basename。
    MediaDuplicate,
    /// 文件系统 IO 失败。
    Io,
    /// SQLite 构建或 zip 打包失败。
    Packaging,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NoCards => "E_NO_CARDS",
            ErrorCode::EmptyFront => "E_EMPTY_FRONT",
            ErrorCode::BadCloze => "E_BAD_CLOZE",
            ErrorCode::TagWhitespace => "E_TAG_WHITESPACE",
            ErrorCode::DeckName => "E_DECK_NAME",
            ErrorCode::MediaMissing => "E_MEDIA_MISSING",
            ErrorCode::MediaDuplicate => "E_MEDIA_DUPLICATE",
            ErrorCode::Io => "E_IO",
            ErrorCode::Packaging => "E_PACKAGING",
        }
    }
}

/// 领域错误：错误码 + 面向模型的人类可读说明。
#[derive(Debug, Clone)]
pub struct DeckError {
    pub code: ErrorCode,
    pub message: String,
}

impl DeckError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 渲染成 `[E_XXX] 说明`。前缀是模型认的稳定标记。
    pub fn render(&self) -> String {
        format!("[{}] {}", self.code.as_str(), self.message)
    }
}

impl std::fmt::Display for DeckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

impl std::error::Error for DeckError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_the_stable_marker_prefix() {
        let error = DeckError::new(ErrorCode::BadCloze, "front has no cloze marker");
        assert_eq!(error.render(), "[E_BAD_CLOZE] front has no cloze marker");
    }

    #[test]
    fn every_code_is_upper_snake_case() {
        for code in [
            ErrorCode::NoCards,
            ErrorCode::EmptyFront,
            ErrorCode::BadCloze,
            ErrorCode::TagWhitespace,
            ErrorCode::DeckName,
            ErrorCode::MediaMissing,
            ErrorCode::MediaDuplicate,
            ErrorCode::Io,
            ErrorCode::Packaging,
        ] {
            let rendered = code.as_str();
            assert!(rendered.starts_with("E_"));
            assert!(rendered.chars().all(|c| c.is_ascii_uppercase() || c == '_'));
        }
    }
}
