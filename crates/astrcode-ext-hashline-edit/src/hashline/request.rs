//! `replace` 参数的解析、归一化与自动纠正。
//!
//! 模型经常把 `hashline_read` 的整行（`aB3│content`）或 diff 预览里的 `+`/`-` 行
//! 直接粘进 `remove_from`/`remove_to`/`replacement_text`。原版不直接报错，而是把
//! 可识别的粘贴痕迹剥掉并附上警告——这样一次工具调用就能纠正，而不是让模型再猜
//! 一轮。这里逐条对齐那些剥离规则与警告措辞。

use rustc_hash::{FxHashMap, FxHashSet};

use serde_json::Value;

use super::{
    error::{EditError, ErrorCode},
    hash::{Anchor, HASH_LEN, HASH_SEP, is_hash},
};

/// `replace` 的原始参数（尚未归一化）。
#[derive(Debug, Clone)]
pub struct RawEdit {
    pub remove_from: String,
    pub remove_to: String,
    /// 故意用 `Value`：模型传数组时要回一句可操作的 `E_BAD_SHAPE` 指引，
    /// 而不是让 serde 抛一个「类型不匹配」。
    pub replacement_text: Value,
}

/// 归一化后的锚点引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashRef {
    pub hash: String,
}

/// 归一化后的编辑请求：范围由两个裸锚点圈定，内容是 LF 分隔的行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    pub content_lines: Vec<String>,
    pub hash_bounds: [HashRef; 2],
}

/// 解析 `replacement_text`。
///
/// `""` 表示删除整个范围；全为 `\n` 的字符串表示等量的空行；其余按 `\n` 切分。
pub fn parse_text(value: &Value) -> Result<Vec<String>, EditError> {
    let Some(text) = value.as_str() else {
        return Err(EditError::new(
            ErrorCode::BadShape,
            "\"replacement_text\" must be a string with \\n line separators, not an array. Do not \
             pass an array of lines — pass the replacement text as one string: \"line1\\nline2\". \
             Use \"\" to delete a range.",
        ));
    };
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
        return Ok(Vec::new());
    }
    if normalized.chars().all(|character| character == '\n') {
        return Ok(vec![String::new(); normalized.len()]);
    }
    Ok(normalized.split('\n').map(str::to_owned).collect())
}

/// 解析一个锚点字段。只接受裸的 3 字符哈希；其余形状给出针对性的纠正指引。
pub fn parse_hash_ref(reference: &str) -> Result<HashRef, EditError> {
    let trimmed = reference.trim();
    if is_hash(trimmed) {
        return Ok(HashRef {
            hash: trimmed.to_owned(),
        });
    }
    if trimmed.is_empty() {
        return Err(EditError::new(
            ErrorCode::BadRef,
            "Invalid anchor. Expected a 3-char alphanumeric anchor (e.g. \"aB3\").",
        ));
    }
    if trimmed.starts_with(|character: char| character.is_ascii_digit()) {
        return Err(EditError::new(
            ErrorCode::BadRef,
            "Invalid anchor. Use the hash alone (e.g. \"aB3\") — no line numbers or trailing \
             content.",
        ));
    }
    if trimmed.contains(HASH_SEP) {
        return Err(EditError::new(
            ErrorCode::BadRef,
            format!(
                "Invalid anchor \"{trimmed}\". remove_from and remove_to must contain the 3-char \
                 hash only — remove everything from \"{HASH_SEP}\" onward."
            ),
        ));
    }
    Err(EditError::new(
        ErrorCode::BadRef,
        format!(
            "Invalid anchor \"{trimmed}\". Expected a 3-char alphanumeric anchor (e.g. \"aB3\")."
        ),
    ))
}

/// 解析整条编辑请求，并剥离锚点字段里的粘贴痕迹。
pub fn res_edit(raw: &RawEdit, warnings: &mut Vec<String>) -> Result<EditRequest, EditError> {
    let content_lines = parse_text(&raw.replacement_text)?;
    let mut bounds: [String; 2] = [String::new(), String::new()];
    for (slot, reference) in [&raw.remove_from, &raw.remove_to].into_iter().enumerate() {
        let trimmed = reference.trim();
        if let Some((sign, hash)) = anchor_row_prefix(trimmed) {
            let detail = match sign {
                Some('+') => format!(
                    "Autocorrected: stripped diff-preview marker copied from the diff preview in \
                     remove_from/remove_to entry \"{trimmed}\"."
                ),
                Some('-') => format!(
                    "Autocorrected: stripped leading \"-\" marker in remove_from/remove_to entry \
                     \"{trimmed}\"."
                ),
                _ => format!(
                    "Autocorrected: stripped \"HASH{HASH_SEP}\" prefix copied from read output in \
                     remove_from/remove_to entry \"{trimmed}\"."
                ),
            };
            warnings.push(format!("[E_BAD_REF] {detail}"));
            bounds[slot] = hash.to_owned();
        } else {
            bounds[slot] = reference.clone();
        }
    }
    Ok(EditRequest {
        content_lines,
        hash_bounds: [parse_hash_ref(&bounds[0])?, parse_hash_ref(&bounds[1])?],
    })
}

/// 剥离 `replacement_text` 行首粘贴进来的 `HASH│` 前缀。
pub fn strip_bare_prefixes(
    edit: &EditRequest,
    file_hashes: &[Anchor],
    warnings: &mut Vec<String>,
) -> EditRequest {
    let file_hash_set: FxHashSet<&str> = file_hashes.iter().map(Anchor::as_str).collect();
    let mut stripped: Vec<(usize, bool)> = Vec::new();
    let content_lines = edit
        .content_lines
        .iter()
        .enumerate()
        .map(|(index, line)| match bare_hash_prefix(line) {
            None => line.clone(),
            Some((prefix_len, hash)) => {
                stripped.push((index, file_hash_set.contains(hash)));
                line[prefix_len..].to_owned()
            },
        })
        .collect();

    if stripped.is_empty() {
        return edit.clone();
    }

    let locations = stripped
        .iter()
        .map(|(index, _)| format!("replacement_text line {}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let matched_count = stripped.iter().filter(|(_, matched)| *matched).count();
    // 一个都没匹配上时给出额外提醒：这行可能是本来就以 `HASH│` 开头的正文。
    let evidence = if matched_count == 0 {
        "none of the stripped hashes match current file lines".to_owned()
    } else {
        format!(
            "{matched_count} of {} stripped hash(es) match current file lines",
            stripped.len()
        )
    };
    let guidance = if matched_count == 0 {
        " Verify that these lines were pasted from read output; literal content starting with \
         'HASH│' would be altered by this strip."
    } else {
        ""
    };
    warnings.push(format!(
        "[E_BARE_HASH_PREFIX] Autocorrected: stripped \"HASH{HASH_SEP}\" prefix copied from read \
         output in {locations} ({evidence}).{guidance}"
    ));

    EditRequest {
        content_lines,
        hash_bounds: edit.hash_bounds.clone(),
    }
}

/// 剥离 `replacement_text` 行首粘贴进来的 diff 预览标记（`+aB3│` / `-aB3│` / `-   │`）。
pub fn strip_diff_prefixes(edit: &EditRequest, warnings: &mut Vec<String>) -> EditRequest {
    let mut stripped: Vec<usize> = Vec::new();
    let content_lines = edit
        .content_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if let Some(rest) = plus_hash_prefix(line) {
                stripped.push(index);
                return rest.to_owned();
            }
            if let Some(rest) = minus_prefix(line) {
                stripped.push(index);
                return rest.to_owned();
            }
            line.clone()
        })
        .collect();

    if stripped.is_empty() {
        return edit.clone();
    }

    let locations = stripped
        .iter()
        .map(|index| format!("replacement_text line {}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    warnings.push(format!(
        "[E_INVALID_PATCH] Autocorrected: stripped diff-preview marker copied from the diff preview \
         in {locations}."
    ));

    EditRequest {
        content_lines,
        hash_bounds: edit.hash_bounds.clone(),
    }
}

/// `remove_from` 排在 `remove_to` 之后时把两者对调。
///
/// 只在这两个锚点都能在当前文件里唯一定位时才动；否则留给锚点校验去报过期。
pub fn swap_reversed_ranges(
    edit: &EditRequest,
    file_hashes: &[Anchor],
    warnings: &mut Vec<String>,
) -> EditRequest {
    let line_by_hash: FxHashMap<&str, usize> = file_hashes
        .iter()
        .enumerate()
        .map(|(index, hash)| (hash.as_str(), index + 1))
        .collect();
    let start_line = line_by_hash.get(edit.hash_bounds[0].hash.as_str()).copied();
    let end_line = line_by_hash.get(edit.hash_bounds[1].hash.as_str()).copied();

    match (start_line, end_line) {
        (Some(start), Some(end)) if start > end => {
            warnings.push(format!(
                "[E_BAD_OP] Autocorrected: remove_from and remove_to were reversed (remove_from {} \
                 is after remove_to {}); swapped the pair.",
                edit.hash_bounds[0].hash, edit.hash_bounds[1].hash
            ));
            EditRequest {
                content_lines: edit.content_lines.clone(),
                hash_bounds: [edit.hash_bounds[1].clone(), edit.hash_bounds[0].clone()],
            }
        },
        _ => edit.clone(),
    }
}

/// 匹配 `([A-Za-z0-9]{3})│` 并返回其中的锚点。
fn hash_sep_prefix(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let prefix_len = HASH_LEN + HASH_SEP.len_utf8();
    if bytes.len() < prefix_len {
        return None;
    }
    if !bytes[..HASH_LEN].iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    text[HASH_LEN..].starts_with(HASH_SEP).then(|| &text[..HASH_LEN])
}

/// 匹配 `^\s*([A-Za-z0-9]{3})│`，返回（前缀字节数，锚点）。
fn bare_hash_prefix(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start_matches(char::is_whitespace);
    let indent = line.len() - rest.len();
    let hash = hash_sep_prefix(rest)?;
    Some((indent + HASH_LEN + HASH_SEP.len_utf8(), hash))
}

/// 匹配 `^\+([A-Za-z0-9]{3})│`，返回剥掉前缀后的剩余部分。
fn plus_hash_prefix(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('+')?;
    hash_sep_prefix(rest)?;
    Some(&rest[HASH_LEN + HASH_SEP.len_utf8()..])
}

/// 匹配 `^-((?:[A-Za-z0-9]{3})│| {3}│)`，返回剥掉前缀后的剩余部分。
fn minus_prefix(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('-')?;
    if hash_sep_prefix(rest).is_some() {
        return Some(&rest[HASH_LEN + HASH_SEP.len_utf8()..]);
    }
    // 无锚点的 diff 行：`-   │content`
    let blank_prefix = format!("{}{HASH_SEP}", " ".repeat(HASH_LEN));
    rest.strip_prefix(blank_prefix.as_str())
}

/// 匹配 `^([+-]?)([A-Za-z0-9]{3})│`，返回（符号，锚点）。
fn anchor_row_prefix(trimmed: &str) -> Option<(Option<char>, &str)> {
    let (sign, rest) = match trimmed.as_bytes().first() {
        Some(b'+') => (Some('+'), &trimmed[1..]),
        Some(b'-') => (Some('-'), &trimmed[1..]),
        _ => (None, trimmed),
    };
    let hash = hash_sep_prefix(rest)?;
    Some((sign, hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(remove_from: &str, remove_to: &str, replacement_text: Value) -> RawEdit {
        RawEdit {
            remove_from: remove_from.to_owned(),
            remove_to: remove_to.to_owned(),
            replacement_text,
        }
    }

    #[test]
    fn parse_text_normalizes_crlf_and_blank_only_replacements() {
        assert_eq!(parse_text(&json!("")).expect("解析失败"), Vec::<String>::new());
        assert_eq!(
            parse_text(&json!("a\r\nb")).expect("解析失败"),
            ["a", "b"]
        );
        assert_eq!(parse_text(&json!("\n\n")).expect("解析失败"), ["", ""]);
        assert_eq!(parse_text(&json!("x\n")).expect("解析失败"), ["x", ""]);
        assert!(parse_text(&json!("")).is_ok());
    }

    #[test]
    fn parse_text_rejects_arrays_with_an_actionable_message() {
        let error = parse_text(&json!(["a"])).expect_err("数组应当被拒绝");
        assert_eq!(error.code(), ErrorCode::BadShape);
        assert!(error.render().contains("not an array"));
    }

    #[test]
    fn parse_hash_ref_rejects_line_numbers_content_and_prefixes() {
        assert_eq!(parse_hash_ref("aB3").expect("解析失败").hash, "aB3");
        assert_eq!(parse_hash_ref("  aB3  ").expect("解析失败").hash, "aB3");
        assert!(parse_hash_ref("12: foo").is_err());
        assert!(parse_hash_ref("aB3│content").is_err());
        assert!(parse_hash_ref("").is_err());
        assert!(parse_hash_ref("aB34").is_err());
    }

    #[test]
    fn res_edit_strips_hash_prefixes_from_anchors_with_warnings() {
        let mut warnings = Vec::new();
        let edit = res_edit(
            &raw("xY9│line content", "aB3", json!("new\nlines")),
            &mut warnings,
        )
        .expect("解析失败");
        assert_eq!(edit.hash_bounds[0].hash, "xY9");
        assert_eq!(edit.hash_bounds[1].hash, "aB3");
        assert_eq!(edit.content_lines, ["new", "lines"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("[E_BAD_REF]"));
        assert!(warnings[0].contains("stripped \"HASH│\" prefix"));
    }

    #[test]
    fn res_edit_strips_diff_markers_from_anchors() {
        let mut warnings = Vec::new();
        let edit = res_edit(&raw("+aB3│x", "-cD4│y", json!("z")), &mut warnings).expect("解析失败");
        assert_eq!(edit.hash_bounds[0].hash, "aB3");
        assert_eq!(edit.hash_bounds[1].hash, "cD4");
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("diff-preview marker"));
        assert!(warnings[1].contains("leading \"-\" marker"));
    }

    #[test]
    fn strip_bare_prefixes_reports_how_many_hashes_matched() {
        let edit = EditRequest {
            content_lines: vec!["aB3│keep".into(), "zZ9│gone".into()],
            hash_bounds: [HashRef { hash: "aB3".into() }, HashRef { hash: "aB3".into() }],
        };
        let mut warnings = Vec::new();
        let stripped = strip_bare_prefixes(&edit, &[Anchor::new("aB3")], &mut warnings);
        assert_eq!(stripped.content_lines, ["keep", "gone"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("1 of 2 stripped hash(es) match current file lines"));
        assert!(!warnings[0].contains("literal content starting with"));
    }

    #[test]
    fn strip_bare_prefixes_warns_when_nothing_matched() {
        let edit = EditRequest {
            content_lines: vec!["zZ9│literal".into()],
            hash_bounds: [HashRef { hash: "aB3".into() }, HashRef { hash: "aB3".into() }],
        };
        let mut warnings = Vec::new();
        let stripped = strip_bare_prefixes(&edit, &[Anchor::new("aB3")], &mut warnings);
        assert_eq!(stripped.content_lines, ["literal"]);
        assert!(warnings[0].contains("none of the stripped hashes match current file lines"));
        assert!(warnings[0].contains("literal content starting with 'HASH│'"));
    }

    #[test]
    fn strip_bare_prefixes_leaves_plain_lines_untouched() {
        let edit = EditRequest {
            content_lines: vec!["plain".into(), "  indented".into()],
            hash_bounds: [HashRef { hash: "aB3".into() }, HashRef { hash: "aB3".into() }],
        };
        let mut warnings = Vec::new();
        let stripped = strip_bare_prefixes(&edit, &[], &mut warnings);
        assert_eq!(stripped, edit);
        assert!(warnings.is_empty());
    }

    #[test]
    fn strip_diff_prefixes_handles_anchored_and_blank_rows() {
        let edit = EditRequest {
            content_lines: vec!["+aB3│plus".into(), "-cD4│minus".into(), "-   │blank".into()],
            hash_bounds: [HashRef { hash: "aB3".into() }, HashRef { hash: "aB3".into() }],
        };
        let mut warnings = Vec::new();
        let stripped = strip_diff_prefixes(&edit, &mut warnings);
        assert_eq!(stripped.content_lines, ["plus", "minus", "blank"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("[E_INVALID_PATCH]"));
    }

    #[test]
    fn swap_reversed_ranges_swaps_only_when_both_anchors_resolve() {
        let edit = EditRequest {
            content_lines: vec!["x".into()],
            hash_bounds: [HashRef { hash: "cD4".into() }, HashRef { hash: "aB3".into() }],
        };
        let file_hashes = [Anchor::new("aB3"), Anchor::new("bC5"), Anchor::new("cD4")];
        let mut warnings = Vec::new();
        let swapped = swap_reversed_ranges(&edit, &file_hashes, &mut warnings);
        assert_eq!(swapped.hash_bounds[0].hash, "aB3");
        assert_eq!(swapped.hash_bounds[1].hash, "cD4");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("were reversed"));

        // 锚点解析不出来时原样返回，交给过期检查报错
        let mut warnings = Vec::new();
        let untouched = swap_reversed_ranges(&edit, &[Anchor::new("zZ9")], &mut warnings);
        assert_eq!(untouched, edit);
        assert!(warnings.is_empty());
    }

    #[test]
    fn prefix_helpers_reject_near_misses() {
        assert_eq!(hash_sep_prefix("aB3│x"), Some("aB3"));
        assert_eq!(hash_sep_prefix("aB│x"), None);
        assert_eq!(hash_sep_prefix("aB34│x"), None);
        assert_eq!(bare_hash_prefix("   aB3│x"), Some((3 + 3 + 3, "aB3")));
        assert_eq!(bare_hash_prefix("aB3"), None);
        assert_eq!(plus_hash_prefix("+aB3│x"), Some("x"));
        assert_eq!(plus_hash_prefix("aB3│x"), None);
        assert_eq!(minus_prefix("-   │blank"), Some("blank"));
        assert_eq!(minus_prefix("-cD4│minus"), Some("minus"));
        assert_eq!(minus_prefix("plain"), None);
    }
}
