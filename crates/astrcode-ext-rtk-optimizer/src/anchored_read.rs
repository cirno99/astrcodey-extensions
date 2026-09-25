//! 锚点式 `read` 输出的识别与「锚点安全」变换。
//!
//! 宿主 `read` 的输出每行是 `{:>6}\t内容`（行号右对齐 6 位 + Tab）。这一组代码负责：
//!
//! 1. **识别**整段输出是不是锚点式（锚点行占比够高、且行号递增）；
//! 2. 把它拆成「前导信息行 + 锚点行 + 尾部信息行」，让后续的过滤与截断只作用于锚点行；
//! 3. 把变换结果**重新映射**回带锚点的行——变换只增删行、不改写保留行的正文；
//! 4. 硬截断时**宁可不截，也不切穿一个锚点行**，并插入锚点安全标记。
//!
//! 上游 `pi-rtk-optimizer` 针对 Pi hashline 的三条正则在这里一条都匹配不到，本模块
//! 按 astrcode 的 `read` 格式重写。注意 astrcode 的 `edit` 用 `oldText` 对文件正文做
//! 精确匹配，行号前缀本身不是编辑锚点（工具说明里写明「without line numbers」），
//! 因此锚点安全解决的是「前缀不被切碎、模型仍能可靠剥离」；而「整行被丢掉导致
//! `oldText` 匹配失败」是有损压缩的固有代价——这正是 `readCompaction` 默认关闭的原因。

use std::sync::LazyLock;

use regex::Regex;

use crate::{
    compact::should_apply_read_source_filtering,
    config::{Config, SourceFilterLevel},
    techniques::{detect_language, filter_source_code, smart_truncate},
};

/// 判定「这是锚点式 read 输出」所需的最少锚点行数。
const ANCHORED_READ_LINE_MIN_MATCHES: usize = 2;
/// 锚点行占相关行的最低比例。
const ANCHORED_READ_LINE_MIN_RATIO: f64 = 0.5;
/// 判定时最多采样多少行。
const ANCHORED_READ_LINE_SAMPLE_LIMIT: usize = 200;

/// astrcode `read` 输出的行首锚点：右对齐行号 + Tab + 正文。
pub(crate) static ANCHORED_READ_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*([0-9]+)\t(.*)$").expect("anchored read line pattern is valid")
});

/// 不属于锚点的信息行：空行、`<file>` 包装、`...`、`[提示]`、`Read x: N lines`。
static ANCHORED_READ_INFORMATIONAL_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:$|<\/?file>|\.{3}|\[[^\]]+\]|Read\s+.+:\s+[0-9]+\s+lines\b)")
        .expect("informational line pattern is valid")
});

/// 锚点式 read 输出的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnchoredReadLine {
    line_number: usize,
    content: String,
    original_line: String,
}

/// 锚点安全意义上的行：展示文本 + 正文。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnchorSafeReadLine {
    text: String,
    content: String,
}

/// 锚点式 read 输出被拆成「前导信息行 + 锚点行 + 尾部信息行」。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnchorSafeReadParts {
    prefix_lines: Vec<String>,
    anchored_lines: Vec<AnchoredReadLine>,
    suffix_lines: Vec<String>,
    trailing_newline: bool,
}

pub(crate) fn split_read_lines(text: &str) -> (Vec<String>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }

    let trailing_newline = text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    if trailing_newline {
        lines.pop();
    }
    (lines, trailing_newline)
}

fn join_read_lines(lines: &[String], trailing_newline: bool) -> String {
    let joined = lines.join("\n");
    if trailing_newline && !joined.is_empty() {
        format!("{joined}\n")
    } else {
        joined
    }
}

fn parse_anchored_read_line(line: &str) -> Option<AnchoredReadLine> {
    let captures = ANCHORED_READ_LINE.captures(line)?;
    let line_number: usize = captures.get(1)?.as_str().parse().ok()?;
    if line_number == 0 {
        return None;
    }
    Some(AnchoredReadLine {
        line_number,
        content: captures.get(2).map_or_else(String::new, |value| value.as_str().to_owned()),
        original_line: line.to_owned(),
    })
}

/// 判断整段输出是否是锚点式 read：锚点行占比够高，且行号递增。
pub(crate) fn looks_like_anchored_read_output(text: &str) -> bool {
    let (lines, _) = split_read_lines(text);
    let mut match_count = 0usize;
    let mut relevant_line_count = 0usize;
    let mut previous_line_number: Option<usize> = None;
    let mut has_increasing_anchors = false;

    for line in lines.iter().take(ANCHORED_READ_LINE_SAMPLE_LIMIT) {
        if !ANCHORED_READ_INFORMATIONAL_LINE.is_match(line) {
            relevant_line_count += 1;
        }

        let Some(anchored) = parse_anchored_read_line(line) else {
            continue;
        };

        match_count += 1;
        if previous_line_number.is_some_and(|previous| anchored.line_number > previous) {
            has_increasing_anchors = true;
        }
        previous_line_number = Some(anchored.line_number);
    }

    if match_count < ANCHORED_READ_LINE_MIN_MATCHES || !has_increasing_anchors {
        return false;
    }

    let ratio_base = relevant_line_count.max(match_count);
    match_count as f64 / ratio_base as f64 >= ANCHORED_READ_LINE_MIN_RATIO
}

fn extract_anchored_read_parts(text: &str) -> Option<AnchorSafeReadParts> {
    if !looks_like_anchored_read_output(text) {
        return None;
    }

    let (lines, trailing_newline) = split_read_lines(text);
    let parsed: Vec<Option<AnchoredReadLine>> = lines
        .iter()
        .map(|line| parse_anchored_read_line(line))
        .collect();
    let first_anchor = parsed.iter().position(Option::is_some)?;
    let last_anchor = parsed
        .iter()
        .rposition(Option::is_some)
        .expect("first anchor implies a last anchor");

    let mut anchored_lines = Vec::with_capacity(last_anchor - first_anchor + 1);
    for entry in parsed.iter().take(last_anchor + 1).skip(first_anchor) {
        // 首尾锚点之间必须全是锚点行，否则不按锚点式处理。
        anchored_lines.push(entry.clone()?);
    }

    Some(AnchorSafeReadParts {
        prefix_lines: lines[..first_anchor].to_vec(),
        anchored_lines,
        suffix_lines: lines[last_anchor + 1..].to_vec(),
        trailing_newline,
    })
}

fn to_anchor_safe_lines(anchored_lines: &[AnchoredReadLine]) -> Vec<AnchorSafeReadLine> {
    anchored_lines
        .iter()
        .map(|line| AnchorSafeReadLine {
            text: line.original_line.clone(),
            content: line.content.clone(),
        })
        .collect()
}

fn render_anchor_safe_read_body(lines: &[AnchorSafeReadLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_anchor_safe_read_text(parts: &AnchorSafeReadParts, lines: &[AnchorSafeReadLine]) -> String {
    let mut all: Vec<String> = parts.prefix_lines.clone();
    all.extend(lines.iter().map(|line| line.text.clone()));
    all.extend(parts.suffix_lines.iter().cloned());
    join_read_lines(&all, parts.trailing_newline)
}

/// 把「在正文上做过的变换」重新映射回带锚点的行。
///
/// 变换只增删行、不改写保留行的正文，因此按顺序在源行里查找匹配即可。匹配不到的行
/// （新插入的省略标记）按裸文本处理，不带锚点。
fn remap_transformed_content_to_anchor_safe_lines(
    source_lines: &[AnchorSafeReadLine],
    transformed_content: &str,
) -> Vec<AnchorSafeReadLine> {
    let (transformed_lines, _) = split_read_lines(transformed_content);
    let mut remapped: Vec<AnchorSafeReadLine> = Vec::new();
    let mut search_start = 0usize;

    for transformed_line in &transformed_lines {
        let matched = source_lines
            .iter()
            .enumerate()
            .skip(search_start)
            .find(|(_, line)| line.content == *transformed_line);

        match matched {
            Some((index, line)) => {
                remapped.push(line.clone());
                search_start = index + 1;
            },
            None => remapped.push(AnchorSafeReadLine {
                text: transformed_line.clone(),
                content: transformed_line.clone(),
            }),
        }
    }

    remapped
}

/// 锚点安全硬截断：宁可不截，也不切穿一个锚点行。
fn truncate_anchor_safe_read_lines(
    lines: &[AnchorSafeReadLine],
    max_chars: usize,
) -> Vec<AnchorSafeReadLine> {
    if render_anchor_safe_read_body(lines).chars().count() <= max_chars {
        return lines.to_vec();
    }

    const MARKER: &str =
        "[RTK anchor-safe truncate: remaining anchored read lines omitted to preserve complete anchors]";

    let mut truncated: Vec<AnchorSafeReadLine> = Vec::new();
    let mut char_count = 0usize;

    for (index, line) in lines.iter().enumerate() {
        let separator_length = usize::from(!truncated.is_empty());
        let next_char_count = char_count + separator_length + line.text.chars().count();
        let remaining_after = lines.len() - index - 1;
        let marker_length = if remaining_after > 0 {
            usize::from(next_char_count > 0) + MARKER.chars().count()
        } else {
            0
        };

        if next_char_count + marker_length > max_chars {
            let marker_line = AnchorSafeReadLine {
                text: MARKER.to_owned(),
                content: MARKER.to_owned(),
            };
            return if truncated.is_empty() {
                vec![marker_line]
            } else {
                truncated.push(marker_line);
                truncated
            };
        }

        truncated.push(line.clone());
        char_count = next_char_count;
    }

    truncated
}

/// 压缩锚点式 `read` 输出：过滤与截断只作用于锚点行，前缀与后缀原样保留。
///
/// 任何一步没有真正改变正文时都不记技术名，因此「命中了这条路径」与「真的压掉了东西」
/// 是两件事。
pub(crate) fn compact_anchored_read_text(
    text: &str,
    file_path: &str,
    config: &Config,
) -> (String, Vec<String>) {
    let Some(parts) = extract_anchored_read_parts(text) else {
        return (text.to_owned(), Vec::new());
    };

    let compaction = &config.output_compaction;
    let language = detect_language(file_path);
    let mut lines = to_anchor_safe_lines(&parts.anchored_lines);
    let mut techniques: Vec<String> = Vec::new();

    if compaction.source_code_filtering_enabled
        && compaction.source_code_filtering != SourceFilterLevel::None
        && should_apply_read_source_filtering(text, config)
    {
        let current_source = lines
            .iter()
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let filtered = filter_source_code(&current_source, language, compaction.source_code_filtering);
        let filtered_lines = remap_transformed_content_to_anchor_safe_lines(&lines, &filtered);
        if render_anchor_safe_read_body(&filtered_lines) != render_anchor_safe_read_body(&lines) {
            lines = filtered_lines;
            techniques.push(format!("source:{}", compaction.source_code_filtering.as_str()));
        }
    }

    if compaction.smart_truncate.enabled && lines.len() > compaction.smart_truncate.max_lines {
        let current_source = lines
            .iter()
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let compacted = smart_truncate(
            &current_source,
            compaction.smart_truncate.max_lines,
            language,
        );
        let compacted_lines = remap_transformed_content_to_anchor_safe_lines(&lines, &compacted);
        if render_anchor_safe_read_body(&compacted_lines) != render_anchor_safe_read_body(&lines) {
            lines = compacted_lines;
            techniques.push("smart-truncate".to_owned());
        }
    }

    if compaction.truncate.enabled
        && render_anchor_safe_read_text(&parts, &lines).chars().count() > compaction.truncate.max_chars
    {
        let non_body_overhead = render_anchor_safe_read_text(&parts, &[]).chars().count();
        let body_max_chars = compaction
            .truncate
            .max_chars
            .saturating_sub(non_body_overhead)
            .max(1);
        let truncated = truncate_anchor_safe_read_lines(&lines, body_max_chars);
        if render_anchor_safe_read_body(&truncated) != render_anchor_safe_read_body(&lines) {
            lines = truncated;
            techniques.push("truncate".to_owned());
        }
    }

    (render_anchor_safe_read_text(&parts, &lines), techniques)
}
