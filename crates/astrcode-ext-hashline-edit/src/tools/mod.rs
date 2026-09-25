//! 三个模型的工具：`hashline_read` / `replace` / `undo_last_replace`。

pub mod read;
pub mod replace;
pub mod undo;

use crate::hashline::{HASH_SEP, error::EditError};

/// diff 前后各展示多少行上下文。与原版一致。
pub(crate) const DIFF_CONTEXT_LINES: usize = 1;

/// 一次工具调用的结果。
///
/// 编辑类失败（锚点过期、范围被外部改过、拒绝清空文件……）都走 `is_error = true`
/// 的正常结果，而不是 `Err`：那段 `[E_*]` 正文就是给模型看的纠正指引，必须逐字
/// 抵达模型。`Err` 只留给真正的基础设施故障。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

impl From<EditError> for ToolOutcome {
    fn from(error: EditError) -> Self {
        Self::error(error.render())
    }
}

/// 渲染 diff 区块。diff 为空时不加任何修饰。
pub(crate) fn render_diff(diff: &str) -> String {
    if diff.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nDiff (HASH{HASH_SEP}anchored; \"+\" rows carry the fresh anchors for chained \
             edits, \"-\" rows the removed lines):\n\n{diff}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_renders_nothing() {
        assert_eq!(render_diff(""), "");
    }

    #[test]
    fn diff_header_names_the_anchor_convention() {
        let rendered = render_diff(" aB3│x");
        assert!(rendered.starts_with("\n\nDiff (HASH│anchored;"));
        assert!(rendered.contains("\"+\" rows carry the fresh anchors"));
        assert!(rendered.ends_with("\n\n aB3│x"));
    }

    #[test]
    fn an_edit_error_becomes_an_error_outcome_with_the_marker() {
        let outcome = ToolOutcome::from(EditError::new(
            crate::hashline::ErrorCode::WouldEmpty,
            "nope",
        ));
        assert!(outcome.is_error);
        assert_eq!(outcome.text, "[E_WOULD_EMPTY] nope");
    }
}
