//! 源码过滤与按行智能截断。
//!
//! 上游 `techniques/source.ts` 的移植。
//!
//! - `minimal`：去掉非文档注释、折叠多余空行，保留 userscript 元数据块。
//! - `aggressive`：在 `minimal` 之上只保留导入、签名、常量与实现块的首尾花括号，
//!   实现体换成 `// ... implementation`。
//! - [`smart_truncate`]：超行数时保留签名/导入/常量行与前一半行，其余折叠成省略标记。

use std::sync::LazyLock;

use regex::Regex;

use crate::config::SourceFilterLevel;

static USERSCRIPT_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^//\s*==\s*userscript\s*==$").expect("userscript start is valid")
});
static USERSCRIPT_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^//\s*==\s*/userscript\s*==$").expect("userscript end is valid")
});

/// 连续三个以上换行折叠成两个。
static COLLAPSE_BLANK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("blank collapse is valid"));

static IMPORT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(use\s+|import\s+|from\s+|require\(|#include)").expect("import pattern is valid")
});
static SIGNATURE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(pub\s+)?(async\s+)?(fn|def|function|func|class|struct|enum|trait|interface|type)\s+[A-Za-z0-9_]+",
    )
    .expect("signature pattern is valid")
});
static CONST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(const|static|let|pub\s+const|pub\s+static)\s+").expect("const pattern is valid")
});

/// 源码语言。只用于挑选注释语法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    TypeScript,
    JavaScript,
    Python,
    Rust,
    Go,
    Java,
    C,
    Cpp,
    Unknown,
}

struct CommentPatterns {
    line: Option<&'static str>,
    block_start: Option<&'static str>,
    block_end: Option<&'static str>,
    doc_line: Option<&'static str>,
    doc_block_start: Option<&'static str>,
}

impl Language {
    fn comment_patterns(self) -> CommentPatterns {
        match self {
            Self::Python => CommentPatterns {
                line: Some("#"),
                block_start: Some("\"\"\""),
                block_end: Some("\"\"\""),
                doc_line: None,
                doc_block_start: Some("\"\"\""),
            },
            Self::Rust => CommentPatterns {
                line: Some("//"),
                block_start: Some("/*"),
                block_end: Some("*/"),
                doc_line: Some("///"),
                doc_block_start: Some("/**"),
            },
            Self::Unknown => CommentPatterns {
                line: Some("//"),
                block_start: Some("/*"),
                block_end: Some("*/"),
                doc_line: None,
                doc_block_start: None,
            },
            Self::TypeScript | Self::JavaScript | Self::Go | Self::Java | Self::C | Self::Cpp => {
                CommentPatterns {
                    line: Some("//"),
                    block_start: Some("/*"),
                    block_end: Some("*/"),
                    doc_line: None,
                    doc_block_start: Some("/**"),
                }
            },
        }
    }
}

/// 按扩展名识别语言；无法识别时返回 [`Language::Unknown`]。
pub fn detect_language(file_path: &str) -> Language {
    let Some(dot) = file_path.rfind('.') else {
        return Language::Unknown;
    };
    match file_path[dot..].to_lowercase().as_str() {
        ".ts" | ".tsx" => Language::TypeScript,
        ".js" | ".jsx" | ".mjs" => Language::JavaScript,
        ".py" | ".pyw" => Language::Python,
        ".rs" => Language::Rust,
        ".go" => Language::Go,
        ".java" => Language::Java,
        ".c" | ".h" => Language::C,
        ".cpp" | ".hpp" | ".cc" => Language::Cpp,
        _ => Language::Unknown,
    }
}

/// 去掉一行里的注释与字符串，只保留代码部分。
///
/// 字符串字面量（含引号本身）整段丢弃——这样字符串里的 `//`、`/*`、`{`、`}` 都不会
/// 被当成代码，这正是上游的意图：`let s = "{ {";` 不该贡献两个左花括号。
///
/// # 为什么按字节而不是按 `Vec<char>` 走
///
/// 上游 TS 的写法是先把整行拆成字符数组，再对每个位置拿 `slice` 和模式数组比较。
/// 照搬成 Rust 会变成「每行一次 `Vec<char>` 分配 + 每个字符位置一次
/// `pattern.chars().collect::<Vec<char>>()`」——5000 行实测 9.9ms，是本插件最慢的
/// 一处。这里改成在 `&str` 上按下标推进：模式匹配用 `str::starts_with`，块注释的
/// 结束位置用 `str::find`，两者都不分配。
///
/// 下标始终落在字符边界上（初值 0，之后每次只按 `char::len_utf8` 或 ASCII 模式串的
/// 长度前进），因此切片是安全的。所有注释模式都是 ASCII，按字节比较与按字符比较等价。
fn get_code_portion(line: &str, language: Language) -> String {
    let patterns = language.comment_patterns();
    let mut code = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut index = 0usize;

    while index < line.len() {
        let character = line[index..]
            .chars()
            .next()
            .expect("index 始终落在字符边界上");
        let width = character.len_utf8();

        if escaped {
            escaped = false;
            index += width;
            continue;
        }

        if let Some(active) = quote {
            if character == '\\' {
                escaped = true;
                index += width;
                continue;
            }
            if character == active {
                quote = None;
            }
            index += width;
            continue;
        }

        if patterns
            .line
            .is_some_and(|pattern| line[index..].starts_with(pattern))
        {
            break;
        }

        if let (Some(block_start), Some(block_end)) = (patterns.block_start, patterns.block_end)
            && line[index..].starts_with(block_start)
        {
            match line[index + block_start.len()..].find(block_end) {
                Some(offset) => {
                    index += block_start.len() + offset + block_end.len();
                    continue;
                },
                None => break,
            }
        }

        if matches!(character, '"' | '\'' | '`') {
            quote = Some(character);
            index += width;
            continue;
        }

        code.push(character);
        index += width;
    }

    code
}

/// 代码部分里的花括号开合数。花括号是 ASCII，按字节数即可。
fn count_braces(code: &str) -> (usize, usize) {
    let mut open = 0usize;
    let mut close = 0usize;
    for byte in code.bytes() {
        if byte == b'{' {
            open += 1;
        } else if byte == b'}' {
            close += 1;
        }
    }
    (open, close)
}

/// `minimal` 过滤：去掉非文档注释、折叠空行，保留 userscript 元数据块。
pub fn filter_minimal(content: &str, language: Language) -> String {
    let patterns = language.comment_patterns();
    let mut result: Vec<&str> = Vec::new();
    let mut in_block_comment = false;
    let mut in_docstring = false;
    let mut in_userscript_metadata = false;

    for line in content.split('\n') {
        let trimmed = line.trim();

        if USERSCRIPT_START.is_match(trimmed) {
            in_userscript_metadata = true;
            result.push(line);
            continue;
        }

        if in_userscript_metadata {
            // 块内每一行都原样保留：元数据键值行（`// @name`）与其余行没有区别。
            result.push(line);
            if USERSCRIPT_END.is_match(trimmed) {
                in_userscript_metadata = false;
            }
            continue;
        }

        if let (Some(block_start), Some(block_end)) = (patterns.block_start, patterns.block_end) {
            if !in_docstring
                && trimmed.contains(block_start)
                && !patterns
                    .doc_block_start
                    .is_some_and(|doc| trimmed.starts_with(doc))
            {
                in_block_comment = true;
            }

            if in_block_comment {
                if trimmed.contains(block_end) {
                    in_block_comment = false;
                }
                continue;
            }
        }

        if language == Language::Python && trimmed.starts_with("\"\"\"") {
            in_docstring = !in_docstring;
            result.push(line);
            continue;
        }

        if in_docstring {
            result.push(line);
            continue;
        }

        if let Some(line_comment) = patterns.line
            && trimmed.starts_with(line_comment)
        {
            if patterns
                .doc_line
                .is_some_and(|doc| trimmed.starts_with(doc))
            {
                result.push(line);
            }
            continue;
        }

        if trimmed.is_empty() {
            result.push("");
            continue;
        }

        result.push(line);
    }

    let joined = result.join("\n");
    COLLAPSE_BLANK.replace_all(&joined, "\n\n").trim().to_owned()
}

/// `aggressive` 过滤：在 `minimal` 之上只保留导入、签名、常量与实现块的首尾花括号。
pub fn filter_aggressive(content: &str, language: Language) -> String {
    let minimal = filter_minimal(content, language);
    let mut result: Vec<&str> = Vec::new();
    let mut brace_depth: i64 = 0;
    let mut in_implementation = false;

    for line in minimal.split('\n') {
        let trimmed = line.trim();

        if IMPORT_PATTERN.is_match(trimmed) {
            result.push(line);
            continue;
        }

        if SIGNATURE_PATTERN.is_match(trimmed) {
            result.push(line);
            in_implementation = true;
            brace_depth = 0;
            continue;
        }

        // 代码部分只算一次，花括号数与尾部比较共用它（原来各算一次）。
        let code = get_code_portion(line, language);
        let (open, close) = count_braces(&code);
        let code_trimmed = code.trim();

        if in_implementation {
            brace_depth += open as i64;
            brace_depth -= close as i64;

            if brace_depth <= 1
                && (code_trimmed == "{"
                    || code_trimmed == "}"
                    || code_trimmed.ends_with('{'))
            {
                result.push(line);
            }

            if brace_depth <= 0 {
                in_implementation = false;
                if !trimmed.is_empty() && trimmed != "}" {
                    result.push("    // ... implementation");
                }
            }
            continue;
        }

        if CONST_PATTERN.is_match(trimmed) {
            result.push(line);
        }
    }

    result.join("\n").trim().to_owned()
}

/// 按行智能截断：保留关键行与前一半行，其余折叠成省略标记。
///
/// `max_lines` 为 0 时上游会出现 `-1` 的边界比较；本 crate 用饱和减法把它收敛成
/// 「立刻收尾」，因为配置层已把该值夹到 `>= 40`，这条路径不可达。
pub fn smart_truncate(content: &str, max_lines: usize, _language: Language) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.len() <= max_lines {
        return content.to_owned();
    }

    let mut result: Vec<String> = Vec::new();
    let mut kept_lines = 0usize;
    let mut skipped_section = false;
    let half = max_lines as f64 / 2.0;

    for line in &lines {
        let trimmed = line.trim();
        let is_important = SIGNATURE_PATTERN.is_match(trimmed)
            || IMPORT_PATTERN.is_match(trimmed)
            || trimmed.starts_with("pub ")
            || trimmed.starts_with("export ")
            || trimmed == "}"
            || trimmed == "{";

        if is_important || (kept_lines as f64) < half {
            if skipped_section {
                result.push(format!("    // ... {} lines omitted", lines.len() - kept_lines));
                skipped_section = false;
            }
            result.push((*line).to_owned());
            kept_lines += 1;
        } else {
            skipped_section = true;
        }

        if kept_lines >= max_lines.saturating_sub(1) {
            break;
        }
    }

    if skipped_section || kept_lines < lines.len() {
        result.push(format!(
            "// ... {} more lines (total: {})",
            lines.len() - kept_lines,
            lines.len()
        ));
    }

    result.join("\n")
}

/// 按强度分发源码过滤；`none` 原样返回。
pub fn filter_source_code(content: &str, language: Language, level: SourceFilterLevel) -> String {
    match level {
        SourceFilterLevel::None => content.to_owned(),
        SourceFilterLevel::Minimal => filter_minimal(content, language),
        SourceFilterLevel::Aggressive => filter_aggressive(content, language),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上游 TS 写法的直译，只用于差分对照：整行拆成字符数组，模式也拆成字符数组
    /// 逐位比较。它是 [`get_code_portion`] 改写前的实现。
    fn character_based_reference(line: &str, language: Language) -> String {
        fn starts_with_at(characters: &[char], index: usize, pattern: &str) -> bool {
            let pattern: Vec<char> = pattern.chars().collect();
            characters.len() >= index + pattern.len()
                && characters[index..index + pattern.len()] == pattern[..]
        }

        fn find_from(characters: &[char], start: usize, pattern: &str) -> Option<usize> {
            let pattern: Vec<char> = pattern.chars().collect();
            if pattern.is_empty() {
                return Some(start);
            }
            if characters.len() < start + pattern.len() {
                return None;
            }
            (start..=characters.len() - pattern.len())
                .find(|&index| characters[index..index + pattern.len()] == pattern[..])
        }

        let patterns = language.comment_patterns();
        let characters: Vec<char> = line.chars().collect();
        let mut quote: Option<char> = None;
        let mut escaped = false;
        let mut code = String::new();
        let mut index = 0usize;

        while index < characters.len() {
            let character = characters[index];

            if escaped {
                escaped = false;
                index += 1;
                continue;
            }

            if let Some(active) = quote {
                if character == '\\' {
                    escaped = true;
                    index += 1;
                    continue;
                }
                if character == active {
                    quote = None;
                }
                index += 1;
                continue;
            }

            if patterns
                .line
                .is_some_and(|pattern| starts_with_at(&characters, index, pattern))
            {
                break;
            }

            if let (Some(block_start), Some(block_end)) =
                (patterns.block_start, patterns.block_end)
                && starts_with_at(&characters, index, block_start)
            {
                let after = index + block_start.chars().count();
                match find_from(&characters, after, block_end) {
                    Some(end) => {
                        index = end + block_end.chars().count();
                        continue;
                    },
                    None => break,
                }
            }

            if matches!(character, '"' | '\'' | '`') {
                quote = Some(character);
                index += 1;
                continue;
            }

            code.push(character);
            index += 1;
        }

        code
    }

    /// 差分对照：按字节推进的版本必须与上游写法逐字符一致。
    #[test]
    fn differential_matches_the_character_based_reference() {
        let corpus: &[(&str, Language)] = &[
            ("", Language::Rust),
            ("   ", Language::Rust),
            ("let a = 1; // note", Language::TypeScript),
            ("// whole line comment", Language::Rust),
            ("/// doc comment", Language::Rust),
            ("let a = 1; /* x */ let b = 2;", Language::TypeScript),
            ("let a = 1; /* unclosed", Language::TypeScript),
            ("/* starts here */ tail", Language::Rust),
            ("fn f() { // trailing", Language::Rust),
            ("fn f() { /* a */ } // b", Language::Rust),
            (r#"let s = "{ {";"#, Language::TypeScript),
            (r#"let url = "http://x";"#, Language::TypeScript),
            (r#"let s = 'a\'; // not a comment"#, Language::TypeScript),
            (r#"let s = "\\"; // after escape"#, Language::TypeScript),
            ("let s = `tpl ${x}`; // done", Language::JavaScript),
            ("let s = \"unterminated", Language::TypeScript),
            ("x = '''", Language::Python),
            ("def f(): # comment", Language::Python),
            ("x = '''doc''' # tail", Language::Python),
            ("x = '''unclosed # inside", Language::Python),
            ("if a { } 中文注释 // 尾巴", Language::Rust),
            ("let 变量 = \"值\"; // 注释", Language::Rust),
            ("{}\r", Language::Rust),
            ("let a = 1;\r\n", Language::TypeScript),
            ("} // }", Language::Rust),
            ("a /* b /* c */ d", Language::Rust),
            ("no comment at all", Language::Go),
            ("#include <a.h> // c", Language::Cpp),
        ];

        for (line, language) in corpus {
            assert_eq!(
                get_code_portion(line, *language),
                character_based_reference(line, *language),
                "行 {line:?} 的代码部分与上游写法不一致"
            );
        }
    }

    #[test]
    fn detects_languages_from_extensions() {
        assert_eq!(detect_language("a.ts"), Language::TypeScript);
        assert_eq!(detect_language("a.tsx"), Language::TypeScript);
        assert_eq!(detect_language("a.mjs"), Language::JavaScript);
        assert_eq!(detect_language("a.py"), Language::Python);
        assert_eq!(detect_language("a.rs"), Language::Rust);
        assert_eq!(detect_language("a.go"), Language::Go);
        assert_eq!(detect_language("a.hpp"), Language::Cpp);
        assert_eq!(detect_language("Makefile"), Language::Unknown);
        assert_eq!(detect_language("a.txt"), Language::Unknown);
    }

    /// `get_code_portion` 只保留「代码」部分：字符串字面量连同引号一起被丢掉，
    /// 这样注释符出现在字符串里不会被误判，代价是返回值不包含字面量本身。
    /// 这是上游的实际行为，也解释了为什么它只用于花括号计数与尾字符判断。
    #[test]
    fn code_portion_drops_string_literals_and_their_contents() {
        assert_eq!(
            get_code_portion(r#"let url = "http://x";"#, Language::TypeScript),
            "let url = ;"
        );
        assert_eq!(
            get_code_portion("let a = 1; // note", Language::TypeScript),
            "let a = 1; "
        );
        assert_eq!(
            get_code_portion("let a = 1; /* x */ let b = 2;", Language::TypeScript),
            "let a = 1;  let b = 2;"
        );
    }

    /// 字符串里的花括号不计入代码花括号数。
    #[test]
    fn braces_inside_strings_are_not_counted() {
        let braces = |line: &str, language: Language| {
            count_braces(&get_code_portion(line, language))
        };
        assert_eq!(braces(r#"let s = "{ {";"#, Language::TypeScript), (0, 0));
        assert_eq!(braces("fn f() {", Language::Rust), (1, 0));
        assert_eq!(braces("}", Language::Rust), (0, 1));
    }

    #[test]
    fn minimal_filter_drops_line_comments_but_keeps_rust_doc_comments() {
        let content = "\
// dropped
/// kept
pub fn a() {}

/* block
   comment */
pub fn b() {}
";
        let filtered = filter_minimal(content, Language::Rust);
        assert!(!filtered.contains("// dropped"));
        assert!(filtered.contains("/// kept"));
        assert!(!filtered.contains("block"));
        assert!(filtered.contains("pub fn a() {}"));
        assert!(filtered.contains("pub fn b() {}"));
    }

    #[test]
    fn minimal_filter_keeps_python_docstrings() {
        let content = "\
# dropped
def f():
    \"\"\"kept docstring\"\"\"
    return 1
";
        let filtered = filter_minimal(content, Language::Python);
        assert!(!filtered.contains("# dropped"));
        assert!(filtered.contains("kept docstring"));
    }

    #[test]
    fn minimal_filter_preserves_userscript_metadata_blocks() {
        let content = "\
// ==UserScript==
// @name demo
// ==/UserScript==
// dropped
const a = 1;
";
        let filtered = filter_minimal(content, Language::JavaScript);
        assert!(filtered.contains("// ==UserScript=="));
        assert!(filtered.contains("// @name demo"));
        assert!(filtered.contains("// ==/UserScript=="));
        assert!(!filtered.contains("// dropped"));
        assert!(filtered.contains("const a = 1;"));
    }

    #[test]
    fn minimal_filter_collapses_runs_of_blank_lines() {
        let content = "a\n\n\n\n\nb\n";
        assert_eq!(filter_minimal(content, Language::Rust), "a\n\nb");
    }

    /// 上游 `filterAggressive` 在签名行上把 `braceDepth` 归零、又没把签名行自带的 `{`
    /// 计进去，因此实现体在下一行就结束。之后的行不再处于实现块里，于是以 `let` 开头的
    /// 行会被 `CONST_PATTERN` 收回来，而普通表达式行会被丢掉。这是上游的既有行为，
    /// 照搬不修；该路径默认关闭（`readCompaction` 与 `sourceCodeFiltering` 都不启用）。
    #[test]
    fn aggressive_filter_keeps_signatures_and_imports() {
        let content = "\
use std::fmt;

const LIMIT: usize = 10;

pub fn compute(value: u32) -> u32 {
    let doubled = value * 2;
    let tripled = value * 3;
    doubled + tripled
}
";
        let filtered = filter_aggressive(content, Language::Rust);
        assert!(filtered.contains("use std::fmt;"));
        assert!(filtered.contains("const LIMIT: usize = 10;"));
        assert!(filtered.contains("pub fn compute(value: u32) -> u32 {"));
        assert!(filtered.contains("// ... implementation"));

        // 实现块内的第一行被标记取代。
        assert!(!filtered.contains("let doubled"));
        // 之后的行被当作常量声明保留。
        assert!(filtered.contains("let tripled"));
        // 既不是签名也不是声明的表达式行被丢掉。
        assert!(!filtered.contains("doubled + tripled"));
    }

    #[test]
    fn smart_truncate_is_identity_below_the_limit() {
        let content = "a\nb\nc";
        assert_eq!(smart_truncate(content, 10, Language::Rust), content);
    }

    #[test]
    fn smart_truncate_keeps_important_lines_and_reports_the_remainder() {
        let mut lines: Vec<String> = Vec::new();
        for index in 0..200 {
            if index == 150 {
                lines.push("pub fn important() {}".to_owned());
            } else {
                lines.push(format!("let value_{index} = {index};"));
            }
        }
        let content = lines.join("\n");
        let truncated = smart_truncate(&content, 40, Language::Rust);

        assert!(truncated.contains("pub fn important() {}"));
        assert!(truncated.contains("more lines (total: 200)"));
        assert!(truncated.lines().count() < 200);
    }

    /// 跳过区之后没有关键行时，`skippedSection` 不会被「排空」，于是只留下结尾的
    /// 总量标记，不会出现中间的 `lines omitted`。这是上游的实际行为。
    #[test]
    fn smart_truncate_without_important_lines_only_reports_the_tail() {
        let lines: Vec<String> = (0..200)
            .map(|index| format!("let value_{index} = {index};"))
            .collect();
        let truncated = smart_truncate(&lines.join("\n"), 40, Language::Rust);

        assert!(!truncated.contains("lines omitted"), "{truncated}");
        assert!(truncated.contains("180 more lines (total: 200)"), "{truncated}");
        assert!(truncated.lines().count() <= 22, "{truncated}");
    }

    /// 跳过区之后出现关键行时，会先补一条 `lines omitted` 再继续。
    #[test]
    fn smart_truncate_marks_omitted_sections_before_an_important_line() {
        let mut lines: Vec<String> = (0..200)
            .map(|index| format!("let value_{index} = {index};"))
            .collect();
        lines[150] = "pub fn important() {}".to_owned();

        let truncated = smart_truncate(&lines.join("\n"), 40, Language::Rust);
        assert!(truncated.contains("lines omitted"), "{truncated}");
        assert!(truncated.contains("pub fn important() {}"), "{truncated}");
    }

    #[test]
    fn filter_source_code_dispatches_on_level() {
        let content = "// comment\nconst a = 1;\n";
        assert_eq!(
            filter_source_code(content, Language::TypeScript, SourceFilterLevel::None),
            content
        );
        assert_eq!(
            filter_source_code(content, Language::TypeScript, SourceFilterLevel::Minimal),
            "const a = 1;"
        );
        assert_eq!(
            filter_source_code(content, Language::TypeScript, SourceFilterLevel::Aggressive),
            "const a = 1;"
        );
    }
}
