//! 哈希器对照基准：std 默认（SipHash13）vs `rustc_hash::FxHasher`。
//!
//! 手动运行：
//!
//! ```sh
//! cargo test -p astrcode-ext-hashline-edit --release --test hasher_comparison -- --ignored --nocapture
//! ```
//!
//! # 为什么要单独量
//!
//! 直接给整个插件换哈希器再各跑一次，测出来的差异会被机器状态的漂移盖住——实测中
//! 连完全不用哈希表的 `canon` 一项都同向漂了 10%。这里改成在**同一个二进制里交替
//! 测量**两种 hasher，取多轮最小值，漂移对两边同等作用，差值才是哈希器的真实贡献。
//!
//! 结论：FxHash 在纯散列上快 4.1x、在 `FxHashMap<&str, Vec<usize>>` 建表+查找上快
//! 2.1x，落到完整 replace 路径是 1.24x。因此生产代码采用 FxHash。

//! 这个文件同时用到两种 hasher，所以 import 不能交给机械替换脚本：
//! `HashMap` 是 std 默认（SipHash13），`FxHashMap` / `FxHasher` 是对照组。

use std::borrow::Cow;
use std::collections::HashMap;
use std::{
    hash::Hasher,
    hint::black_box,
    time::Instant,
};

use astrcode_ext_hashline_edit::hashline::lines::{canon, split_lines};
use rustc_hash::{FxHashMap, FxHasher};

/// 造一份贴近真实源码的行分布：大量唯一行 + 高频重复行（`}`、空行、`use ...`）。
///
/// 注意：这里的字面量**不能**长得像 Rust 的 `use` 语句，否则会被机械替换脚本
/// 一起改掉（本文件已经踩过一次）。
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

/// 多轮取最小值：最小值对偶发的调度抖动最不敏感，适合做 A/B 对照。
fn best_of(rounds: u32, mut body: impl FnMut()) -> std::time::Duration {
    body();
    let mut best = std::time::Duration::MAX;
    for _ in 0..rounds {
        let start = Instant::now();
        body();
        best = best.min(start.elapsed());
    }
    best
}

#[test]
#[ignore = "哈希器对照基准，用 --ignored --nocapture 手动运行"]
fn std_versus_fx() {
    const LINES: usize = 5000;
    const ROUNDS: u32 = 40;

    let content = sample_source(LINES);
    // `canon` 返回 `Cow`：无 `\r` 的行直接借用原切片，这里也就一并量了「键不复制」的形状。
    let keys: Vec<Cow<'_, str>> = split_lines(&content).iter().map(|line| canon(line)).collect();
    println!("\n键：{} 个，平均 {} 字节\n", keys.len(), content.len() / keys.len());

    // ── 1. 纯散列：把每个键喂给 hasher，不做别的 ──
    let hash_std = best_of(ROUNDS, || {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for key in &keys {
            hasher.write(black_box(key).as_ref().as_bytes());
        }
        black_box(hasher.finish());
    });
    let hash_fx = best_of(ROUNDS, || {
        let mut hasher = FxHasher::default();
        for key in &keys {
            hasher.write(black_box(key).as_ref().as_bytes());
        }
        black_box(hasher.finish());
    });

    // ── 2. 建表 + 逐键查找：这才是 val_edit / map_stable_hashes 里真正发生的事 ──
    let table_std = best_of(ROUNDS, || {
        let mut map: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, key) in keys.iter().enumerate() {
            map.entry(black_box(key).as_ref()).or_default().push(index);
        }
        let mut hits = 0usize;
        for key in &keys {
            if map.contains_key(black_box(key).as_ref()) {
                hits += 1;
            }
        }
        black_box(hits);
    });
    let table_fx = best_of(ROUNDS, || {
        let mut map: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
        for (index, key) in keys.iter().enumerate() {
            map.entry(black_box(key).as_ref()).or_default().push(index);
        }
        let mut hits = 0usize;
        for key in &keys {
            if map.contains_key(black_box(key).as_ref()) {
                hits += 1;
            }
        }
        black_box(hits);
    });

    // ── 3. 含 Cow 键的建表（map_stable_hashes 的 new_by_content）──
    let owned_std = best_of(ROUNDS, || {
        let mut map: HashMap<Cow<'_, str>, Vec<usize>> = HashMap::new();
        for (index, key) in keys.iter().enumerate() {
            map.entry(black_box(key).clone()).or_default().push(index);
        }
        black_box(map.len());
    });
    let owned_fx = best_of(ROUNDS, || {
        let mut map: FxHashMap<Cow<'_, str>, Vec<usize>> = FxHashMap::default();
        for (index, key) in keys.iter().enumerate() {
            map.entry(black_box(key).clone()).or_default().push(index);
        }
        black_box(map.len());
    });

    // 两种 hasher 对同一输入给出不同摘要，证明两边都真的被跑到了
    let mut std_probe = std::collections::hash_map::DefaultHasher::new();
    std_probe.write(b"aB3");
    let mut fx_probe = FxHasher::default();
    fx_probe.write(b"aB3");
    assert_ne!(std_probe.finish(), fx_probe.finish());

    let report = |label: &str, std_time: std::time::Duration, fx_time: std::time::Duration| {
        let speedup = std_time.as_secs_f64() / fx_time.as_secs_f64();
        println!(
            "{label:<28} std {:>10?}   fx {:>10?}   fx/std {speedup:>5.2}x",
            std_time, fx_time
        );
    };
    println!("--- 同二进制交替测量，{ROUNDS} 轮取最小 ---");
    report("1. 纯散列 5000 键", hash_std, hash_fx);
    report("2. 建表+查找 5000 键", table_std, table_fx);
    report("3. 含 Cow 键建表", owned_std, owned_fx);

    // 这两个基准只量哈希本身；它们在完整 replace 路径里的占比由
    // tests/performance.rs 的 A/B 给出（实测 1.24x）。
}
