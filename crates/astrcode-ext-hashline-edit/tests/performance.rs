//! 热点微基准。
//!
//! 默认不跑（`cargo test` 会忽略），手动执行：
//!
//! ```sh
//! cargo test -p astrcode-ext-hashline-edit --release --test performance -- --ignored --nocapture
//! ```
//!
//! 它的用途不是卡性能门槛，而是给「换哈希器值不值」「该优化哪一段」这类决定
//! 提供实测数字。每次编辑都要对整份文件重建几遍查找表，所以这里按「5000 行文件
//! 上跑一次完整 replace」的口径量。
//!
//! 输出末尾有一行 `SUMMARY`，便于脚本化地做 A/B 对照。
//!
//! 做哈希器 A/B 时会用机械替换把 std 哈希器换成 FxHash，所以本文件里的 import
//! 必须写成单行 `use rustc_hash::FxHashSet;` 这种形状（替换脚本按行匹配），
//! 且样本数据里不能出现长得像本 crate import 的字面量。

use rustc_hash::FxHashSet;
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use astrcode_ext_hashline_edit::hashline::{
    HashRef,
    anchor::val_edit,
    apply::apply_edit,
    diff::gen_diff,
    hash::{Anchor, line_hashes_pure, map_stable_hashes},
    lines::{canon, split_lines},
    request::EditRequest,
};

/// 造一份贴近真实源码的行分布：大量唯一行 + 高频重复行（`}`、空行、`use ...`）。
///
/// 注意：这里的字面量**不能**长得像本 crate 的 `use` 语句，否则做哈希器 A/B 时
/// 会被机械替换脚本一起改掉，两次测量就落在不同的输入上。
fn sample_source(lines: usize) -> String {
    let mut out = String::with_capacity(lines * 40);
    for index in 0..lines {
        match index % 7 {
            0 => out.push_str("}\n"),
            1 => out.push('\n'),
            2 => out.push_str("let mut counts = BTreeMap::new();\n"),
            3 => out.push_str(&format!("    let value_{index} = compute({index});\n")),
            4 => out.push_str("    // 中文注释也要算进来，字节长度和字符长度不同\n"),
            5 => out.push_str(&format!("fn helper_{index}(input: &str) -> Option<usize> {{\n")),
            _ => out.push_str("        Some(input.len())\n"),
        }
    }
    out
}

fn time(label: &str, iterations: u32, mut body: impl FnMut()) -> Duration {
    // 预热一次，避免把首次的页错误算进去。
    body();
    let start = Instant::now();
    for _ in 0..iterations {
        body();
    }
    let elapsed = start.elapsed();
    let per_iteration = elapsed / iterations;
    println!("{label:<36} {per_iteration:>12?}");
    per_iteration
}

#[test]
#[ignore = "微基准，用 --ignored --nocapture 手动运行"]
fn hot_paths() {
    const LINES: usize = 5000;
    const ITERATIONS: u32 = 200;

    let content = sample_source(LINES);
    let hashes = line_hashes_pure(&content).expect("分配锚点失败");
    let removed: FxHashSet<Anchor> = [hashes[LINES / 2]].into_iter().collect();
    let edited = content.replacen("    // 中文注释", "    // 改过的注释", 1);
    let lines = split_lines(&content);
    let edit = EditRequest {
        content_lines: vec!["    // 改过的注释".to_owned()],
        hash_bounds: [
            HashRef {
                hash: hashes[LINES / 2].to_string(),
            },
            HashRef {
                hash: hashes[LINES / 2].to_string(),
            },
        ],
    };

    println!(
        "\n文件：{LINES} 行 / {} 字节，每项 {ITERATIONS} 次取平均\n",
        content.len()
    );

    let canon_time = time("1. canon 全文件（纯分配）", ITERATIONS, || {
        let total: usize = lines.iter().map(|line| canon(line).len()).sum();
        black_box(total);
    });
    let hashes_time = time("2. line_hashes_pure 全文件", ITERATIONS, || {
        black_box(line_hashes_pure(black_box(&content)).expect("分配失败"));
    });
    let val_time = time("3. val_edit 全文件（锚点索引）", ITERATIONS, || {
        black_box(val_edit(&edit, &lines, &hashes).expect("校验失败"));
    });
    let stable_time = time("4. map_stable_hashes 全文件", ITERATIONS, || {
        black_box(
            map_stable_hashes(black_box(&content), &hashes, black_box(&edited), &removed)
                .expect("映射失败"),
        );
    });
    let apply_time = time("5. apply_edit（校验 + 落区间）", ITERATIONS, || {
        black_box(
            apply_edit(&content, &edit, Some(&hashes), Some("sample.rs"), None).expect("编辑失败"),
        );
    });

    // 完整 replace 路径 = apply_edit + 稳定映射 + diff 生成
    let replace_time = time("6. 完整 replace 路径（5 + 4 + diff）", ITERATIONS, || {
        let applied =
            apply_edit(&content, &edit, Some(&hashes), Some("sample.rs"), None).expect("编辑失败");
        let result_hashes =
            map_stable_hashes(&content, &hashes, &applied.content, &removed).expect("映射失败");
        let (diff, _) = gen_diff(
            &content,
            &applied.content,
            1,
            Some(&result_hashes),
            Some(&hashes),
        );
        black_box(diff);
    });

    println!("\n--- 单项 ---");
    for (label, duration) in [
        ("canon", canon_time),
        ("line_hashes_pure", hashes_time),
        ("val_edit", val_time),
        ("map_stable_hashes", stable_time),
        ("apply_edit", apply_time),
    ] {
        println!("{label:<24} {duration:>12?}");
    }

    println!(
        "SUMMARY bytes={} canon={} hashes={} val_edit={} map_stable={} apply={} replace={}",
        content.len(),
        canon_time.as_nanos(),
        hashes_time.as_nanos(),
        val_time.as_nanos(),
        stable_time.as_nanos(),
        apply_time.as_nanos(),
        replace_time.as_nanos(),
    );
}
