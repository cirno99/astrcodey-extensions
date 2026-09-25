//! 哈希锚点编辑的失败分类。
//!
//! 每个变体逐字对应原版 `dsh-hashline-edit-pro` 的 `[E_*]` 标记。模型是靠这些标记
//! 学会自我纠正的（「锚点过期 → 重新 hashline_read」），因此正文一律照抄原版，
//! 只做语言层面的搬运，不改写措辞。

/// 原版的 `[E_*]` 错误标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// 参数形状不对，例如把 `replacement_text` 写成字符串数组。
    BadShape,
    /// 锚点字段不是裸的 3 字符哈希。
    BadRef,
    /// 范围本身非法：`remove_from` 解析出的行在 `remove_to` 之后。
    BadOp,
    /// 锚点已过期：该哈希在当前文件里不存在。
    StaleAnchor,
    /// 锚点有歧义：同一个哈希匹配多行。
    AmbiguousAnchor,
    /// 被替换范围内的行没有全部展示给过模型（served 守卫）。
    RangeStale,
    /// 拒绝把一个非空文件清空。
    WouldEmpty,
    /// 文件超过 100MB 或 238328 行上限。
    FileTooLarge,
    /// 目标是图片。
    Image,
    /// 目标是二进制或非 UTF-8 文件。
    Binary,
    /// undo 记录存在，但文件在 replace 之后被外部改过。
    UndoStale,
    /// 原版里就没有 `[E_*]` 标记的失败（文件不存在、不是普通文件、写入失败）。
    /// 与 [`Self::Internal`] 一样不渲染标记，区别只在映射到线缆错误码时的语义。
    Plain,
    /// 内部不变量被破坏（哈希数组与行数组长度不一致）。正常路径不可能触发。
    Internal,
}

impl ErrorCode {
    /// 渲染进正文的标记名，不含方括号。
    pub const fn marker(self) -> &'static str {
        match self {
            Self::BadShape => "E_BAD_SHAPE",
            Self::BadRef => "E_BAD_REF",
            Self::BadOp => "E_BAD_OP",
            Self::StaleAnchor => "E_STALE_ANCHOR",
            Self::AmbiguousAnchor => "E_AMBIGUOUS_ANCHOR",
            Self::RangeStale => "E_RANGE_STALE",
            Self::WouldEmpty => "E_WOULD_EMPTY",
            Self::FileTooLarge => "E_FILE_TOO_LARGE",
            Self::Image => "E_IMAGE",
            Self::Binary => "E_BINARY",
            Self::UndoStale => "E_UNDO_STALE",
            Self::Plain => "",
            Self::Internal => "",
        }
    }
}

/// 一次编辑失败。
///
/// `body` 是**不含**标记的正文；[`EditError::render`] 负责拼上 `[E_*]` 前缀。
/// 这样拆分是为了让 [`ErrorCode`] 与正文永远一致——原版把标记写死在字符串里，
/// 分类只能靠正则回读。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditError {
    code: ErrorCode,
    body: String,
    /// 已经展示给模型的锚点。出错时调用方把它们补记进 served 集合，模型下一轮
    /// 就能直接用这批新锚点重试，不必再读一次文件。
    feedback_hashes: Vec<String>,
}

impl EditError {
    pub fn new(code: ErrorCode, body: impl Into<String>) -> Self {
        Self {
            code,
            body: body.into(),
            feedback_hashes: Vec::new(),
        }
    }

    /// 附上「随错误一起展示的新锚点」。
    #[must_use]
    pub fn with_feedback(mut self, hashes: Vec<String>) -> Self {
        self.feedback_hashes = hashes;
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn feedback_hashes(&self) -> &[String] {
        &self.feedback_hashes
    }

    /// 交给模型的完整正文。
    pub fn render(&self) -> String {
        if self.code.marker().is_empty() {
            return self.body.clone();
        }
        format!("[{}] {}", self.code.marker(), self.body)
    }

    /// 原版里就没有标记的失败。
    pub(crate) fn plain(body: impl Into<String>) -> Self {
        Self::new(ErrorCode::Plain, body)
    }

    /// 内部不变量被破坏。
    pub(crate) fn internal(body: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, body)
    }
}

impl std::fmt::Display for EditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.render())
    }
}

impl std::error::Error for EditError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_prefixes_the_marker() {
        let error = EditError::new(ErrorCode::WouldEmpty, "Cannot empty a non-empty file.");
        assert_eq!(
            error.render(),
            "[E_WOULD_EMPTY] Cannot empty a non-empty file."
        );
    }

    #[test]
    fn internal_errors_render_without_a_marker() {
        let error = EditError::internal("valEdit: fileHashes.length (2) must match fileLines.length (3).");
        assert_eq!(
            error.render(),
            "valEdit: fileHashes.length (2) must match fileLines.length (3)."
        );
    }

    #[test]
    fn feedback_hashes_are_carried_but_not_rendered() {
        let error = EditError::new(ErrorCode::StaleAnchor, "stale").with_feedback(vec!["aB3".into()]);
        assert_eq!(error.feedback_hashes(), ["aB3"]);
        assert_eq!(error.render(), "[E_STALE_ANCHOR] stale");
    }
}
