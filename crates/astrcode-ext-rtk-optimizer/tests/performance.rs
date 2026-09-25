//! 压缩管线的微基准。
//!
//! 默认忽略，手动跑：
//!
//! ```sh
//! cargo test -p astrcode-ext-rtk-optimizer --release --test performance -- --ignored --nocapture
//! ```
//!
//! 每项取多次平均，最后打印一行 `SUMMARY` 便于两个二进制交替对照。

use std::{path::PathBuf, time::Instant};

use astrcode_ext_rtk_optimizer::{
    compact::{GREP_TOOL, READ_TOOL, SHELL_TOOL, compact_tool_result},
    config::{Config, SourceFilterLevel},
};
use serde_json::json;

/// 基准输入的行数。
const LINES: usize = 5_000;
/// 每项重复次数。
const ROUNDS: usize = 200;

/// astrcode `read` 输出形态：`{:>6}\t内容`。
fn anchored_read(line_count: usize) -> String {
    let mut out = String::with_capacity(line_count * 48);
    for number in 1..=line_count {
        let body = if number % 7 == 0 {
            format!("    // 注释 {number}：说明这一行在做什么")
        } else if number % 11 == 0 {
            format!("    let value{number} = compute({number});")
        } else {
            format!("    let binding_{number} = {number};")
        };
        out.push_str(&format!("{number:>6}\t{body}\n"));
    }
    out
}

/// 不带行号的普通 read 正文。
fn plain_read(line_count: usize) -> String {
    let mut out = String::with_capacity(line_count * 48);
    for number in 1..=line_count {
        if number % 7 == 0 {
            out.push_str(&format!("// 注释 {number}：说明这一行在做什么\n"));
        } else {
            out.push_str(&format!("let binding_{number} = compute({number});\n"));
        }
    }
    out
}

/// shell 输出形态，带退出状态前导。
fn shell_output(line_count: usize) -> String {
    let mut out = String::from("Process exited with code 0\nOutput:\n");
    for number in 1..=line_count {
        if number % 500 == 0 {
            out.push_str(&format!("warning: unused variable `x{number}`\n"));
        } else {
            out.push_str(&format!("   Compiling crate_{number} v0.1.{number}\n"));
        }
    }
    out.push_str("    Finished `dev` profile [unoptimized] target(s) in 12.34s\n");
    out
}

/// grep 输出形态。
fn grep_output(line_count: usize) -> String {
    let mut out = String::new();
    for number in 1..=line_count {
        out.push_str(&format!(
            "crates/pkg/src/file_{}.rs:{}:    let value = {number};\n",
            number % 40,
            number
        ));
    }
    out
}

fn default_config() -> Config {
    Config::default()
}

/// 把 read 压缩相关的开关全部打开——这是最重的一条路径。
fn everything_on() -> Config {
    let mut config = Config::default();
    let compaction = &mut config.output_compaction;
    compaction.read_compaction.enabled = true;
    compaction.source_code_filtering_enabled = true;
    compaction.source_code_filtering = SourceFilterLevel::Aggressive;
    compaction.smart_truncate.enabled = true;
    compaction.truncate.max_chars = 200_000;
    config
}

fn time(label: &str, rounds: usize, mut body: impl FnMut() -> usize) -> u128 {
    // 预热一次，避免把首次的惰性初始化算进来。
    std::hint::black_box(body());
    let start = Instant::now();
    let mut sink = 0usize;
    for _ in 0..rounds {
        sink = sink.wrapping_add(std::hint::black_box(body()));
    }
    let elapsed = start.elapsed();
    let per_round = elapsed.as_nanos() / rounds as u128;
    println!("{label:<34} {per_round:>10} ns  (sink={sink})");
    per_round
}

#[test]
#[ignore = "微基准，手动运行"]
fn compaction_pipeline() {
    let working_dir = PathBuf::from("/tmp/rtk-bench");
    let read_anchored = anchored_read(LINES);
    let read_plain = plain_read(LINES);
    let shell = shell_output(LINES);
    let grep = grep_output(LINES);
    println!(
        "\n输入：{} 行；read(锚点)={} 字节 read(普通)={} 字节 shell={} 字节 grep={} 字节",
        LINES,
        read_anchored.len(),
        read_plain.len(),
        shell.len(),
        grep.len()
    );

    let default = default_config();
    let full = everything_on();

    let read_input = json!({ "path": "crates/pkg/src/lib.rs" });
    let shell_input = json!({ "command": "cargo build" });
    let grep_input = json!({ "pattern": "value" });

    let mut summary = format!("bytes={}", read_anchored.len());
    let mut record = |name: &str, value: u128| {
        summary.push_str(&format!(" {name}={value}"));
    };

    record(
        "read_default",
        time("read 默认配置（readCompaction 关）", ROUNDS, || {
            compact_tool_result(READ_TOOL, &read_input, &read_anchored, &working_dir, &default)
                .map_or(0, |outcome| outcome.text.len())
        }),
    );
    record(
        "read_all_on",
        time("read 全开（锚点路径）", ROUNDS, || {
            compact_tool_result(READ_TOOL, &read_input, &read_anchored, &working_dir, &full)
                .map_or(0, |outcome| outcome.text.len())
        }),
    );
    record(
        "read_plain_all_on",
        time("read 全开（普通路径）", ROUNDS, || {
            compact_tool_result(READ_TOOL, &read_input, &read_plain, &working_dir, &full)
                .map_or(0, |outcome| outcome.text.len())
        }),
    );
    record(
        "shell_default",
        time("shell 默认配置", ROUNDS, || {
            compact_tool_result(SHELL_TOOL, &shell_input, &shell, &working_dir, &default)
                .map_or(0, |outcome| outcome.text.len())
        }),
    );
    record(
        "shell_nomatch",
        time("shell 非构建命令（无技术命中）", ROUNDS, || {
            let input = json!({ "command": "ls -la" });
            compact_tool_result(SHELL_TOOL, &input, &shell, &working_dir, &default)
                .map_or(0, |outcome| outcome.text.len())
        }),
    );

    record(
        "grep_default",
        time("grep 默认配置", ROUNDS, || {
            compact_tool_result(GREP_TOOL, &grep_input, &grep, &working_dir, &default)
                .map_or(0, |outcome| outcome.text.len())
        }),
    );

    println!("\n--- 单项技术 ---");
    use std::sync::LazyLock;

    use astrcode_ext_rtk_optimizer::techniques::{
        aggregate_test_output, compact_git_output, detect_language, filter_build_output,
        filter_source_code, group_search_results, smart_truncate, strip_ansi_fast, truncate,
    };
    use regex::Regex;
    let language = detect_language("crates/pkg/src/lib.rs");

    // 与 search.rs 里那条同形的正则，单独量一遍，用来把「正则」与「分组+分配」拆开。
    static SEARCH_LINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(.+?):([0-9]+)?:(.+)$").expect("search line is valid")
    });
    record(
        "t_search_regex_only",
        time("SEARCH_LINE 正则（仅匹配）", ROUNDS, || {
            let mut matched = 0usize;
            for line in grep.split('\n') {
                if !line.trim().is_empty() && SEARCH_LINE.captures(line).is_some() {
                    matched += 1;
                }
            }
            matched
        }),
    );
    record(
        "t_group_search",
        time("group_search_results", ROUNDS, || {
            group_search_results(&grep, 50).map_or(0, |text| text.len())
        }),
    );
    record(
        "t_source_aggressive",
        time("filter_source_code(aggressive)", ROUNDS, || {
            filter_source_code(&read_plain, language, SourceFilterLevel::Aggressive).len()
        }),
    );
    record(
        "t_source_minimal",
        time("filter_source_code(minimal)", ROUNDS, || {
            filter_source_code(&read_plain, language, SourceFilterLevel::Minimal).len()
        }),
    );
    record(
        "t_smart_truncate",
        time("smart_truncate(220 行)", ROUNDS, || {
            smart_truncate(&read_plain, 220, language).len()
        }),
    );
    record(
        "t_truncate",
        time("truncate(12000 字符)", ROUNDS, || {
            truncate(&read_plain, 12_000).len()
        }),
    );
    record(
        "t_strip_ansi",
        time("strip_ansi_fast(无 ESC)", ROUNDS, || {
            strip_ansi_fast(&read_plain).len()
        }),
    );
    record(
        "t_build_filter",
        time("filter_build_output", ROUNDS, || {
            filter_build_output(&shell, Some("cargo build")).map_or(0, |text| text.len())
        }),
    );
    record(
        "t_test_agg",
        time("aggregate_test_output", ROUNDS, || {
            aggregate_test_output(&shell, Some("cargo test")).map_or(0, |text| text.len())
        }),
    );
    record(
        "t_git_compact",
        time("compact_git_output", ROUNDS, || {
            compact_git_output(&shell, Some("git status")).map_or(0, |text| text.len())
        }),
    );

    println!("\nSUMMARY {summary}");
}
