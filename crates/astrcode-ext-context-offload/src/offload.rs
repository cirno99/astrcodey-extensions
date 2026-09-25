//! offload 决策与确定性占位符渲染。
//!
//! 占位符必须对同一份输入产生逐字节相同的输出：它会被宿主写进 durable 记录，
//! 之后每一轮都作为历史消息重放。任何非确定性（时间戳、随机 id、哈希迭代顺序）
//! 都会让 provider 的前缀缓存失效，并让「同一份工具输出对应两段不同的历史」成为
//! 可能。因此这里只使用内容本身可推导的信息。

use astrcode_ext_common::{
    paths::fnv1a,
    text::{char_count_exceeds, format_tokens},
};

/// offload 阈值与渲染参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffloadPolicy {
    /// 超过该字符数才 offload；小于等于时原文原样保留。
    pub min_chars: usize,
    /// 占位符预览行的最大字符数。
    pub preview_chars: usize,
}

impl Default for OffloadPolicy {
    fn default() -> Self {
        Self {
            min_chars: 8_000,
            preview_chars: 120,
        }
    }
}

/// 内容是否值得 offload。
///
/// 按字符数而非 token 数判定：工具热路径上不该为了精确阈值再走一次宿主 IPC。
/// 中文与代码的字符/token 比差异很大，但 8000 字符对两者都已经明显值得换出。
///
/// 判定走 [`char_count_exceeds`] 的字节长度快路径：绝大多数工具输出是短 ASCII，
/// 用字节数就能直接判否，不必扫全文。
pub fn should_offload(content: &str, policy: &OffloadPolicy) -> bool {
    char_count_exceeds(content, policy.min_chars)
}

/// 渲染写回 transcript 的占位符。
///
/// 两行：第一行给出「哪来的、多大、开头是什么」，第二行给出取回方式。模型据此
/// 判断是否需要 `retrieve`，不需要的场合就省下全部原文 token。
pub fn render_placeholder(
    reference: &str,
    tool_name: &str,
    content: &str,
    is_error: bool,
    policy: &OffloadPolicy,
) -> String {
    let error_mark = if is_error { " · error" } else { "" };
    let mut header = format!(
        "📦 [offload #{reference} · {tool_name}{error_mark} · {} chars]",
        format_count(content.chars().count())
    );
    if let Some(preview) = preview_line(content, policy.preview_chars) {
        header.push(' ');
        header.push_str(&preview);
    }

    format!(
        "{header}\n   → retrieve(ref=\"{reference}\") returns the full output, paginated via \
         offset/limit"
    )
}

/// 由工具调用标识派生占位符引用名。
///
/// `tool_call_id` 由宿主生成、会话内唯一，是天然的稳定身份，因此直接采用。为空时
/// 退回内容哈希：跨进程边界传来的值不能假定非空，而两个不同的空 id 若落到同一个
/// 文件名，后写的会覆盖先写的，`retrieve` 就会拿到别人的原文。
pub fn make_ref(tool_call_id: &str, content: &str) -> String {
    if tool_call_id.is_empty() {
        return format!("anon-{:016x}", fnv1a(content));
    }
    tool_call_id.to_string()
}

/// 取第一个非空行作为预览，超出 `max_chars` 时截断并追加省略号。
fn preview_line(content: &str, max_chars: usize) -> Option<String> {
    let line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;

    let mut characters = line.chars();
    let truncated: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        Some(format!("{truncated}…"))
    } else {
        Some(truncated)
    }
}

/// 紧凑的字符计数：`987` / `12.3K` / `1.23M`。
///
/// 复用 `text::format_tokens` 的紧凑化规则，但这里的量纲是字符数而非 token 数；
/// 单独命名是为了让调用点不会把两个量纲读混。
fn format_count(count: usize) -> String {
    format_tokens(count as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> OffloadPolicy {
        OffloadPolicy::default()
    }

    #[test]
    fn threshold_is_exclusive() {
        let policy = OffloadPolicy {
            min_chars: 10,
            ..OffloadPolicy::default()
        };
        assert!(!should_offload("0123456789", &policy));
        assert!(should_offload("01234567890", &policy));
    }

    #[test]
    fn threshold_counts_characters_not_bytes() {
        let policy = OffloadPolicy {
            min_chars: 3,
            ..OffloadPolicy::default()
        };
        // 4 个汉字是 12 字节；按字符数判定，不应因为字节数超阈值而误判。
        assert!(should_offload("错误信息", &policy));
        assert!(!should_offload("错误", &policy));
    }

    #[test]
    fn placeholder_is_deterministic() {
        let content = "line one\nline two\n";
        let first = render_placeholder("call-1", "shell", content, false, &policy());
        let second = render_placeholder("call-1", "shell", content, false, &policy());
        assert_eq!(first, second);
    }

    #[test]
    fn placeholder_reports_reference_tool_and_size() {
        let content = "x".repeat(12_300);
        let rendered = render_placeholder("call-1", "shell", &content, false, &policy());
        assert!(rendered.starts_with("📦 [offload #call-1 · shell · 12.3K chars]"));
        assert!(rendered.contains("retrieve(ref=\"call-1\")"));
    }

    #[test]
    fn placeholder_marks_error_results() {
        let content = "x".repeat(100);
        let rendered = render_placeholder("call-1", "shell", &content, true, &policy());
        assert!(rendered.contains("shell · error · "));
    }

    #[test]
    fn placeholder_uses_first_non_empty_line_as_preview() {
        let content = "\n\n   npm run build   \nmore output";
        let rendered = render_placeholder("call-1", "shell", content, false, &policy());
        assert!(rendered.contains("] npm run build\n"));
    }

    #[test]
    fn placeholder_truncates_a_long_preview_line() {
        let content = format!("{}\ntail", "y".repeat(200));
        let rendered = render_placeholder("call-1", "shell", &content, false, &policy());
        let preview: String = "y".repeat(120);
        assert!(rendered.contains(&format!("] {preview}…\n")));
    }

    #[test]
    fn placeholder_omits_preview_when_content_has_no_visible_line() {
        let rendered = render_placeholder("call-1", "shell", "   \n\t\n", false, &policy());
        assert!(rendered.starts_with("📦 [offload #call-1 · shell · 6 chars]\n"));
    }

    #[test]
    fn reference_prefers_the_tool_call_id() {
        assert_eq!(make_ref("call-1", "whatever"), "call-1");
    }

    #[test]
    fn reference_falls_back_to_content_hash_when_the_id_is_empty() {
        let first = make_ref("", "payload");
        let second = make_ref("", "payload");
        let other = make_ref("", "different");
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert!(first.starts_with("anon-"));
    }

    #[test]
    fn preview_line_stops_at_the_character_budget() {
        assert_eq!(preview_line("abcdef", 3).as_deref(), Some("abc…"));
        assert_eq!(preview_line("abc", 3).as_deref(), Some("abc"));
        assert_eq!(preview_line("  \n ", 3), None);
    }
}
