//! `post_tool_use` 钩子与输出压缩管线。
//!
//! 上游 `pi-rtk-optimizer/src/output-compactor.ts` 的移植。三条管线按工具名分派：
//! `shell` 走构建/测试/git/linter 聚合，`read` 走源码过滤与截断（含锚点安全分支），
//! `grep` 走搜索结果分组。最后统一做硬字符截断。
//!
//! # 与上游的三处适配
//!
//! 1. **内容是字符串，不是内容块数组。** S5R 的 `ToolResult.content` 已经是 `String`，
//!    上游遍历 `[{type:"text",text}]` 的那一层被去掉。
//! 2. **保留 shell 结果的退出状态前导。** 宿主 shell 工具的内容形如
//!    `Process exited with code N\nOutput:\n<输出>`，上游的 Pi bash 工具没有这段。
//!    压缩前切出前导、压缩后原样回贴，否则构建/测试过滤会把退出码一起丢掉。
//! 3. **锚点按 astrcode 的 `read` 格式识别。** 宿主 read 输出每行是
//!    `{:>6}\t内容`（行号右对齐 6 位 + Tab），上游针对 Pi hashline 的三条正则在这里
//!    一条都匹配不到。本模块把「行号 + Tab」视为不可分割的行首锚点：过滤与截断可以
//!    整行丢弃，但不切断它，硬截断时插入锚点安全标记。
//!
//!    注意 astrcode 的 `edit` 用 `oldText` 对文件正文做精确匹配，行号前缀本身不是
//!    编辑锚点（工具说明里写明「without line numbers」）。因此锚点安全解决的是
//!    「前缀不被切碎、模型仍能可靠剥离」，而「整行被丢掉导致 `oldText` 匹配失败」是
//!    有损压缩的固有代价——这正是 `readCompaction` 默认关闭的原因。

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use astrcode_ext_common::text::char_count_exceeds;
use astrcode_extension_sdk::hostpaths;
use serde_json::Value;

use crate::{
    anchored_read::{compact_anchored_read_text, looks_like_anchored_read_output},
    config::{Config, OutputCompaction},
    metrics,
    techniques::{
        aggregate_linter_output, aggregate_test_output, compact_git_output, detect_language,
        filter_build_output, filter_source_code, group_search_results, smart_truncate, strip_ansi_fast,
        truncate,
    },
};

/// 宿主 shell 工具名。
pub const SHELL_TOOL: &str = "shell";
/// 宿主 read 工具名。
pub const READ_TOOL: &str = "read";
/// 宿主 grep 工具名。
pub const GREP_TOOL: &str = "grep";

/// 不超过这个行数的 `read` 输出保持原样。
const READ_EXACT_OUTPUT_LINE_THRESHOLD: usize = 80;
/// `read` 压缩横幅前缀。
const READ_COMPACTION_BANNER_PREFIX: &str = "[RTK compacted output:";

/// 有损技术名；命中其一即认为本次压缩不可逆。
const LOSSY_TECHNIQUE_PREFIXES: [&str; 8] = [
    "build",
    "test",
    "git",
    "linter",
    "search",
    "truncate",
    "smart-truncate",
    "source:",
];


/// 一次压缩的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    /// 压缩后的正文。
    pub text: String,
    /// 本次实际生效的技术名。
    pub techniques: Vec<String>,
    /// 压缩前字符数（不含 shell 前导）。
    pub original_chars: usize,
    /// 压缩后字符数（不含 shell 前导）。
    pub compacted_chars: usize,
    /// 是否用到了有损技术。
    pub lossy: bool,
}

/// 按工具名分派压缩。返回 `None` 表示本次内容不需要改动。
///
/// `working_dir` 用于判断 `read` 是否落在技能目录内（那类读取保持精确）。
pub fn compact_tool_result(
    tool_name: &str,
    tool_input: &Value,
    content: &str,
    working_dir: &Path,
    config: &Config,
) -> Option<CompactionOutcome> {
    if !config.output_compaction.enabled || content.is_empty() {
        return None;
    }

    let (preamble, body) = split_shell_preamble(tool_name, content);

    let (text, techniques) = match tool_name {
        SHELL_TOOL => compact_shell_text(body, shell_command(tool_input), config),
        READ_TOOL => {
            // 保持精确输出时直接放行。原先是先 `to_owned()` 一份整段正文、再让调用方
            // 比出「与原文相同」后丢掉——`read` 的默认配置（`readCompaction` 关闭）
            // 正好走这条分支，等于每次读取白拷一遍。
            let file_path = read_path(tool_input);
            if should_preserve_exact_read_output(body, tool_input, file_path, working_dir, config) {
                return None;
            }
            compact_read_text(body, file_path, config)
        },
        GREP_TOOL => compact_grep_text(body, config),
        _ => return None,
    };

    if text == body {
        return None;
    }

    let outcome = CompactionOutcome {
        text: format!("{preamble}{text}"),
        lossy: has_lossy_compaction(&techniques),
        original_chars: body.chars().count(),
        compacted_chars: text.chars().count(),
        techniques,
    };

    if config.output_compaction.track_savings {
        metrics::record(
            tool_name,
            outcome.original_chars,
            outcome.compacted_chars,
            &outcome.techniques,
        );
    }

    Some(outcome)
}

/// 切出 shell 结果的退出状态前导（`Process ...\nOutput:\n`）。
///
/// 非 shell 工具、或形态不匹配时返回空前导与整段内容。
fn split_shell_preamble<'a>(tool_name: &str, content: &'a str) -> (&'a str, &'a str) {
    if tool_name != SHELL_TOOL {
        return ("", content);
    }

    if content.is_empty() {
        return ("", content);
    }

    let first_line_end = content.find('\n').map_or(content.len(), |index| index + 1);
    let first_line = &content[..first_line_end];
    if !first_line.starts_with("Process ") {
        return ("", content);
    }

    let rest = &content[first_line_end..];
    for marker in ["Output:\n", "Output so far:\n"] {
        if let Some(offset) = rest.find(marker) {
            let end = first_line_end + offset + marker.len();
            return content.split_at(end);
        }
    }

    ("", content)
}

/// 从工具入参里取 shell 命令。借用入参，不复制。
fn shell_command(tool_input: &Value) -> Option<&str> {
    tool_input.get("command").and_then(Value::as_str)
}

/// 从工具入参里取 read 的文件路径。借用入参，不复制。
fn read_path(tool_input: &Value) -> &str {
    tool_input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `read` 是否必须保持精确输出。
fn should_preserve_exact_read_output(
    text: &str,
    tool_input: &Value,
    file_path: &str,
    working_dir: &Path,
    config: &Config,
) -> bool {
    let compaction = &config.output_compaction;
    if !compaction.read_compaction.enabled {
        return true;
    }

    if has_explicit_read_range(tool_input) {
        return true;
    }

    if compaction.preserve_exact_skill_reads && is_skill_read_path(file_path, working_dir) {
        return true;
    }

    count_lines(text) <= READ_EXACT_OUTPUT_LINE_THRESHOLD
}

/// 模型是否显式指定了读取范围。
///
/// 上游只看 `offset`/`limit`（Pi 的行范围参数）；astrcode 的 `read` 另有
/// `charOffset`/`maxChars` 两种字符范围参数，同样是「精确切片」意图，一并视为精确读取。
fn has_explicit_read_range(tool_input: &Value) -> bool {
    ["offset", "limit", "charOffset", "maxChars"]
        .iter()
        .any(|key| tool_input.get(key).is_some_and(|value| !value.is_null()))
}

/// 技能根目录集合：用户级 + 工作目录各级祖先。
fn skill_roots(working_dir: &Path) -> Vec<PathBuf> {
    let home = hostpaths::user_home_dir();
    let mut roots = vec![
        home.join(".claude").join("skills"),
        home.join(".astrcode").join("skills"),
    ];

    let mut ancestors: Vec<&Path> = working_dir.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        roots.push(ancestor.join(".claude").join("skills"));
        roots.push(ancestor.join(".astrcode").join("skills"));
    }

    roots
}

fn is_skill_read_path(file_path: &str, working_dir: &Path) -> bool {
    if file_path.trim().is_empty() {
        return false;
    }

    let path = Path::new(file_path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        working_dir.join(path)
    };

    skill_roots(working_dir)
        .iter()
        .any(|root| resolved == *root || resolved.starts_with(root))
}

/// 行数口径与上游 `countLines` 一致：空串 0 行，结尾换行不产生额外行。
fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let normalized = text.strip_suffix('\n').unwrap_or(text);
    if normalized.is_empty() {
        return 1;
    }
    normalized.split('\n').count()
}

/// 是否应当为 `read` 应用有损源码过滤。
///
/// 只有下游的行数/字符截断本来就会触发时才过滤——避免「本来够短的内容被主动弄丢」。
pub(crate) fn should_apply_read_source_filtering(text: &str, config: &Config) -> bool {
    let compaction = &config.output_compaction;
    (compaction.smart_truncate.enabled && count_lines(text) > compaction.smart_truncate.max_lines)
        || (compaction.truncate.enabled
            && char_count_exceeds(text, compaction.truncate.max_chars))
}

/// 本次压缩是否用到了有损技术。
///
/// 带冒号的表项（`source:`）按前缀匹配，其余按全等匹配。原先在前缀分支里用
/// `format!("{stem}:")` 把去掉的冒号拼回来再比——拼出来的正是表项本身，白分配一次
/// 字符串；这里直接拿表项做前缀。
fn has_lossy_compaction(techniques: &[String]) -> bool {
    techniques.iter().any(|technique| {
        LOSSY_TECHNIQUE_PREFIXES
            .iter()
            .any(|prefix| match prefix.strip_suffix(':') {
                Some(_) => technique.starts_with(prefix),
                None => technique == prefix,
            })
    })
}

/// 管线中间状态。
struct CompactionState {
    text: String,
    techniques: Vec<String>,
}

impl CompactionState {
    fn new(text: &str, compaction: &OutputCompaction) -> Self {
        let mut state = Self {
            text: text.to_owned(),
            techniques: Vec::new(),
        };
        if compaction.strip_ansi {
            // 借用分支说明原文不含 `ESC`，`strip_ansi` 只会返回一份逐字相同的副本，
            // 因此这里既不必比较内容，也不该记一条 `ansi` 技术——与改动前逐位一致。
            if let Cow::Owned(stripped) = strip_ansi_fast(&state.text)
                && stripped != state.text
            {
                state.text = stripped;
                state.techniques.push("ansi".to_owned());
            }
        }
        state
    }

    /// 跑一个可空结果的技术：结果不同才采纳并记录。
    fn apply(&mut self, transform: impl Fn(&str) -> Option<String>, technique: &str) {
        let Some(compacted) = transform(&self.text) else {
            return;
        };
        if compacted != self.text {
            self.text = compacted;
            self.techniques.push(technique.to_owned());
        }
    }

    fn apply_if(&mut self, enabled: bool, transform: impl Fn(&str) -> Option<String>, technique: &str) {
        if enabled {
            self.apply(transform, technique);
        }
    }

    fn apply_truncation(&mut self, compaction: &OutputCompaction) {
        // `char_count_exceeds` 的字节快路径让「输出很短」这个常见情形完全不用扫描；
        // 走到 `truncate` 时已经确定超限，因此那里不会再数第二遍。
        if compaction.truncate.enabled
            && char_count_exceeds(&self.text, compaction.truncate.max_chars)
        {
            self.text = truncate(&self.text, compaction.truncate.max_chars);
            self.techniques.push("truncate".to_owned());
        }
    }

    fn into_parts(self) -> (String, Vec<String>) {
        (self.text, self.techniques)
    }
}

/// shell 输出管线。
fn compact_shell_text(text: &str, command: Option<&str>, config: &Config) -> (String, Vec<String>) {
    let compaction = &config.output_compaction;
    let mut state = CompactionState::new(text, compaction);

    state.apply_if(
        compaction.filter_build_output,
        |text| filter_build_output(text, command),
        "build",
    );
    state.apply_if(
        compaction.aggregate_test_output,
        |text| aggregate_test_output(text, command),
        "test",
    );
    state.apply_if(
        compaction.compact_git_output,
        |text| compact_git_output(text, command),
        "git",
    );
    state.apply_if(
        compaction.aggregate_linter_output,
        |text| aggregate_linter_output(text, command),
        "linter",
    );

    state.apply_truncation(compaction);

    state.into_parts()
}

/// grep 输出管线。
fn compact_grep_text(text: &str, config: &Config) -> (String, Vec<String>) {
    let compaction = &config.output_compaction;
    let mut state = CompactionState::new(text, compaction);

    state.apply_if(
        compaction.group_search_output,
        |text| group_search_results(text, 50),
        "search",
    );

    state.apply_truncation(compaction);

    state.into_parts()
}

/// read 输出管线。
///
/// 调用方已经确认过「不需要保持精确输出」（见 `should_preserve_exact_read_output`），
/// 因此这里不再有「原样返回」的分支——那条分支曾经把整段正文拷一份再被丢掉。
fn compact_read_text(text: &str, file_path: &str, config: &Config) -> (String, Vec<String>) {
    let compaction = &config.output_compaction;
    let mut state = CompactionState::new(text, compaction);

    if looks_like_anchored_read_output(&state.text) {
        let (compacted, techniques) = compact_anchored_read_text(&state.text, file_path, config);
        state.text = compacted;
        state.techniques.extend(techniques);
        apply_read_compaction_banner(&mut state);
        return state.into_parts();
    }

    let language = detect_language(file_path);
    if compaction.source_code_filtering_enabled
        && compaction.source_code_filtering != crate::config::SourceFilterLevel::None
        && should_apply_read_source_filtering(text, config)
    {
        state.apply(
            |text| {
                Some(filter_source_code(
                    text,
                    language,
                    compaction.source_code_filtering,
                ))
            },
            &format!("source:{}", compaction.source_code_filtering.as_str()),
        );
    }

    if compaction.smart_truncate.enabled && count_lines(&state.text) > compaction.smart_truncate.max_lines
    {
        let compacted = smart_truncate(&state.text, compaction.smart_truncate.max_lines, language);
        if compacted != state.text {
            state.text = compacted;
            state.techniques.push("smart-truncate".to_owned());
        }
    }

    state.apply_truncation(compaction);
    apply_read_compaction_banner(&mut state);

    state.into_parts()
}

fn apply_read_compaction_banner(state: &mut CompactionState) {
    if !state.techniques.is_empty() && !state.text.starts_with(READ_COMPACTION_BANNER_PREFIX) {
        state.text = format!(
            "{READ_COMPACTION_BANNER_PREFIX} {}]\n{}",
            state.techniques.join(", "),
            state.text
        );
    }
}



#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::anchored_read::ANCHORED_READ_LINE;

    use super::*;

    fn working_dir() -> PathBuf {
        PathBuf::from("/tmp/rtk-compact-tests")
    }

    fn shell_content(body: &str) -> String {
        format!("Process exited with code 0\nOutput:\n{body}")
    }

    /// 生成 astrcode `read` 形态的正文：`{:>6}\t内容`。
    fn read_content(line_count: usize) -> String {
        let lines: Vec<String> = (1..=line_count)
            .map(|number| {
                let body = if number % 2 == 0 {
                    format!("const value{number} = {number};")
                } else {
                    format!("// comment {number}")
                };
                format!("{number:>6}\t{body}")
            })
            .collect();
        lines.join("\n")
    }

    /// 不带行号前缀的普通 read 正文。
    fn plain_read_content(line_count: usize) -> String {
        let lines: Vec<String> = (1..=line_count)
            .map(|number| format!("const value{number} = {number};"))
            .collect();
        lines.join("\n")
    }

    fn default_config() -> Config {
        Config::default()
    }

    fn compact(
        tool: &str,
        input: Value,
        content: &str,
        config: &Config,
    ) -> Option<CompactionOutcome> {
        compact_tool_result(tool, &input, content, &working_dir(), config)
    }

    #[test]
    fn unknown_tools_are_left_alone() {
        assert!(compact("write", json!({}), "some output", &default_config()).is_none());
    }

    #[test]
    fn compaction_disabled_is_a_no_op() {
        let mut config = default_config();
        config.output_compaction.enabled = false;
        assert!(compact(SHELL_TOOL, json!({"command": "git status"}), "x", &config).is_none());
    }

    #[test]
    fn empty_content_is_a_no_op() {
        assert!(compact(SHELL_TOOL, json!({"command": "ls"}), "", &default_config()).is_none());
    }

    #[test]
    fn shell_preamble_is_preserved_around_compacted_output() {
        let content = shell_content("   Compiling foo v0.1.0\n   Compiling bar v0.2.0\n");
        let outcome = compact(
            SHELL_TOOL,
            json!({"command": "cargo build"}),
            &content,
            &default_config(),
        )
        .unwrap();

        assert!(outcome.text.starts_with("Process exited with code 0\nOutput:\n"));
        assert!(outcome.text.contains("[OK] Build successful (2 units compiled)"));
        assert!(outcome.techniques.contains(&"build".to_owned()));
    }

    #[test]
    fn shell_preamble_is_not_counted_in_the_savings_metrics() {
        let content = shell_content("   Compiling foo v0.1.0\n");
        let outcome = compact(
            SHELL_TOOL,
            json!({"command": "cargo build"}),
            &content,
            &default_config(),
        )
        .unwrap();
        assert_eq!(outcome.original_chars, "   Compiling foo v0.1.0\n".chars().count());
    }

    #[test]
    fn shell_without_a_preamble_still_compacts() {
        let outcome = compact(
            SHELL_TOOL,
            json!({"command": "cargo build"}),
            "   Compiling foo v0.1.0\n",
            &default_config(),
        )
        .unwrap();
        assert_eq!(outcome.text, "[OK] Build successful (1 units compiled)");
    }

    #[test]
    fn grep_results_are_grouped_by_file() {
        let content = "src/b.ts:3:const b = 1;\nsrc/a.ts:1:const a = 1;\n";
        let outcome = compact(GREP_TOOL, json!({"pattern": "const"}), content, &default_config())
            .unwrap();
        assert!(outcome.techniques.contains(&"search".to_owned()));
        assert!(outcome.text.starts_with("2 matches in 2 files:"));
    }

    #[test]
    fn read_output_is_exact_when_read_compaction_is_disabled() {
        let mut config = default_config();
        config.output_compaction.source_code_filtering_enabled = true;
        config.output_compaction.source_code_filtering = crate::config::SourceFilterLevel::Aggressive;
        config.output_compaction.smart_truncate.enabled = true;
        config.output_compaction.smart_truncate.max_lines = 40;
        config.output_compaction.truncate.enabled = true;
        config.output_compaction.truncate.max_chars = 500;

        let content = read_content(220);
        assert!(compact(READ_TOOL, json!({"path": "sample.ts"}), &content, &config).is_none());
    }

    #[test]
    fn read_with_an_explicit_range_is_exact() {
        let mut config = default_config();
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.truncate.enabled = true;
        config.output_compaction.truncate.max_chars = 500;
        config.output_compaction.smart_truncate.enabled = true;
        config.output_compaction.smart_truncate.max_lines = 40;

        let content = read_content(220);
        for input in [
            json!({"path": "sample.ts", "offset": 1}),
            json!({"path": "sample.ts", "limit": 200}),
            json!({"path": "sample.ts", "charOffset": 0}),
            json!({"path": "sample.ts", "maxChars": 100}),
        ] {
            assert!(
                compact(READ_TOOL, input.clone(), &content, &config).is_none(),
                "input = {input}"
            );
        }
    }

    #[test]
    fn read_of_a_skill_file_is_exact_when_requested() {
        let mut config = default_config();
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.preserve_exact_skill_reads = true;
        config.output_compaction.smart_truncate.enabled = true;
        config.output_compaction.smart_truncate.max_lines = 40;

        let content = read_content(220);
        let path = working_dir().join(".astrcode/skills/demo/SKILL.md");
        assert!(compact(READ_TOOL, json!({"path": path}), &content, &config).is_none());

        config.output_compaction.preserve_exact_skill_reads = false;
        assert!(compact(READ_TOOL, json!({"path": path}), &content, &config).is_some());
    }

    #[test]
    fn read_compaction_adds_the_banner() {
        let mut config = default_config();
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.source_code_filtering_enabled = true;
        config.output_compaction.source_code_filtering = crate::config::SourceFilterLevel::Minimal;
        config.output_compaction.smart_truncate.enabled = true;
        config.output_compaction.smart_truncate.max_lines = 40;

        let content = read_content(220);
        let outcome = compact(READ_TOOL, json!({"path": "sample.ts"}), &content, &config).unwrap();
        assert!(outcome.text.starts_with(READ_COMPACTION_BANNER_PREFIX));
        assert!(outcome.text.contains("source:minimal"));
        assert!(outcome.lossy);
    }

    /// 80 行以内（含边界）的 read 输出保持精确。
    #[test]
    fn read_output_is_exact_at_the_line_threshold() {
        let mut config = default_config();
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.smart_truncate.enabled = true;
        config.output_compaction.smart_truncate.max_lines = 40;

        assert!(compact(READ_TOOL, json!({"path": "s.ts"}), &read_content(80), &config).is_none());
        assert!(compact(READ_TOOL, json!({"path": "s.ts"}), &read_content(81), &config).is_some());
    }

    #[test]
    fn anchored_read_output_keeps_complete_line_prefixes() {
        let mut config = default_config();
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.source_code_filtering_enabled = true;
        config.output_compaction.source_code_filtering = crate::config::SourceFilterLevel::Minimal;
        config.output_compaction.smart_truncate.enabled = true;
        config.output_compaction.smart_truncate.max_lines = 40;
        config.output_compaction.truncate.enabled = true;
        config.output_compaction.truncate.max_chars = 5_000;

        let content = read_content(120);
        let outcome = compact(READ_TOOL, json!({"path": "sample.ts"}), &content, &config).unwrap();

        assert!(outcome.techniques.contains(&"source:minimal".to_owned()));
        assert!(outcome.text.contains("     2\tconst value2 = 2;"));
        assert!(!outcome.text.contains("// comment"));
        for line in outcome.text.split('\n') {
            if ANCHORED_READ_LINE.is_match(line) {
                assert!(!line.ends_with("..."), "anchor line was cut: {line}");
            }
        }
    }

    #[test]
    fn anchored_read_truncation_uses_the_anchor_safe_marker() {
        let mut config = default_config();
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.smart_truncate.enabled = false;
        config.output_compaction.source_code_filtering_enabled = false;
        config.output_compaction.truncate.enabled = true;
        config.output_compaction.truncate.max_chars = 350;

        let content: String = (1..=120)
            .map(|number| format!("{number:>6}\tconst value{number} = \"{}\";", "x".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");

        let outcome = compact(READ_TOOL, json!({"path": "sample.ts"}), &content, &config).unwrap();
        assert!(outcome.techniques.contains(&"truncate".to_owned()));
        assert!(outcome.text.contains("anchor-safe truncate"));
        for line in outcome.text.split('\n') {
            if ANCHORED_READ_LINE.is_match(line) {
                assert!(!line.ends_with("..."), "anchor line was cut: {line}");
            }
        }
    }

    #[test]
    fn informational_wrapper_lines_survive_anchored_compaction() {
        let mut config = default_config();
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.source_code_filtering_enabled = true;
        config.output_compaction.source_code_filtering = crate::config::SourceFilterLevel::Minimal;
        config.output_compaction.smart_truncate.enabled = true;
        config.output_compaction.smart_truncate.max_lines = 40;
        config.output_compaction.truncate.enabled = true;
        config.output_compaction.truncate.max_chars = 5_000;

        let mut lines = vec!["<file>".to_owned()];
        lines.extend((1..=120).map(|number| {
            let body = if number % 2 == 0 {
                format!("const value{number} = {number};")
            } else {
                format!("// comment {number}")
            };
            format!("{number:>6}\t{body}")
        }));
        lines.push(String::new());
        lines.push("(End of file - 120 total lines)".to_owned());
        lines.push("</file>".to_owned());

        let outcome = compact(
            READ_TOOL,
            json!({"path": "sample.ts"}),
            &lines.join("\n"),
            &config,
        )
        .unwrap();

        assert!(outcome.text.contains("<file>"));
        assert!(outcome.text.contains("</file>"));
        assert!(outcome.text.contains("     2\tconst value2 = 2;"));
    }

    #[test]
    fn a_single_anchor_like_line_does_not_trigger_the_anchored_path() {
        let content = format!("     1\tnot really an anchored read\n{}", plain_read_content(120));
        assert!(!looks_like_anchored_read_output(&content));
    }

    #[test]
    fn plain_read_output_is_not_treated_as_anchored() {
        assert!(!looks_like_anchored_read_output(&plain_read_content(120)));
        assert!(!looks_like_anchored_read_output(""));
        assert!(!looks_like_anchored_read_output("just\nsome\nlines"));
    }

    /// 行号不递增（比如日志里的 `时间\t消息`）时不算锚点式输出。
    #[test]
    fn non_increasing_line_numbers_are_not_anchored() {
        let content = (1..=20)
            .map(|_| "     7\tsame number".to_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!looks_like_anchored_read_output(&content));
    }

    /// 锚点行占比太低时不算锚点式输出。
    #[test]
    fn a_low_anchor_ratio_is_not_anchored() {
        let mut lines = vec!["     1\tanchor".to_owned(), "     2\tanchor".to_owned()];
        for index in 0..40 {
            lines.push(format!("plain line {index}"));
        }
        assert!(!looks_like_anchored_read_output(&lines.join("\n")));
    }

    #[test]
    fn ansi_codes_are_stripped_before_other_techniques() {
        let content = "\x1b[31mgit status output\x1b[0m";
        let outcome = compact(GREP_TOOL, json!({}), content, &default_config()).unwrap();
        assert!(outcome.techniques.contains(&"ansi".to_owned()));
        assert_eq!(outcome.text, "git status output");
    }

    #[test]
    fn hard_truncation_applies_last() {
        let mut config = default_config();
        config.output_compaction.truncate.enabled = true;
        config.output_compaction.truncate.max_chars = 1_000;

        let content = "x".repeat(5_000);
        let outcome = compact(GREP_TOOL, json!({}), &content, &config).unwrap();
        assert!(outcome.techniques.contains(&"truncate".to_owned()));
        assert_eq!(outcome.text.chars().count(), 1_000);
    }

    #[test]
    fn count_lines_matches_the_upstream_definition() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("\n"), 1);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(count_lines("a\nb\n"), 2);
    }

    #[test]
    fn lossy_detection_matches_prefixes_and_exact_names() {
        assert!(has_lossy_compaction(&["build".to_owned()]));
        assert!(has_lossy_compaction(&["source:minimal".to_owned()]));
        assert!(!has_lossy_compaction(&["ansi".to_owned()]));
        assert!(!has_lossy_compaction(&[]));
    }

    #[test]
    fn skill_roots_cover_user_and_ancestor_directories() {
        let roots = skill_roots(Path::new("/work/project"));
        let home = hostpaths::user_home_dir();
        assert!(roots.contains(&home.join(".claude").join("skills")));
        assert!(roots.contains(&home.join(".astrcode").join("skills")));
        assert!(roots.contains(&PathBuf::from("/work/project/.astrcode/skills")));
        assert!(roots.contains(&PathBuf::from("/work/.claude/skills")));
    }

    #[test]
    fn skill_path_matching_handles_relative_paths() {
        let working = working_dir();
        assert!(is_skill_read_path(".astrcode/skills/demo/SKILL.md", &working));
        assert!(!is_skill_read_path("src/main.rs", &working));
        assert!(!is_skill_read_path("", &working));
    }
}
