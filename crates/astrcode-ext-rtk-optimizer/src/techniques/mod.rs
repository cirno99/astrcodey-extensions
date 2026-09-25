//! 输出压缩技术栈。
//!
//! 每个子模块对应上游 `pi-rtk-optimizer/src/techniques/` 下的一个文件，语义逐条对齐。
//!
//! # 与上游正则的差异
//!
//! 上游跑在 JavaScript 上，`\d` / `\w` 只覆盖 ASCII。Rust 的 `regex` 默认开启 Unicode，
//! `\d` 会匹配任何 Unicode 十进制数字、`\w` 会匹配任何 Unicode 词字符，直接照抄会让
//! 行为在非 ASCII 输入上分叉。因此本 crate 的 pattern 一律把 `\d` 写成 `[0-9]`、
//! `\w` 写成 `[A-Za-z0-9_]`，与 JS 语义对齐。
//!
//! # 长度口径
//!
//! JS 的 `String.length` 是 UTF-16 码元数，`slice` 也按码元切。本 crate 一律按
//! Unicode 标量（`char`）计数与切分——对 BMP 内的字符两者一致，只有非 BMP 字符
//! （如 emoji）会有差异。

pub mod ansi;
pub mod build;
pub mod command;
pub mod git;
pub mod linter;
pub mod path;
pub mod search;
pub mod source;
pub mod test;
pub mod truncate;

pub use ansi::strip_ansi_fast;
pub use build::filter_build_output;
pub use git::compact_git_output;
pub use linter::aggregate_linter_output;
pub use search::group_search_results;
pub use source::{detect_language, filter_source_code, smart_truncate};
pub use test::aggregate_test_output;
pub use truncate::truncate;
