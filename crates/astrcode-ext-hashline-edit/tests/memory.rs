//! 内存驻留诊断。
//!
//! 默认不跑（`cargo test` 会忽略），手动执行：
//!
//! ```sh
//! cargo test -p astrcode-ext-hashline-edit --release --test memory -- --ignored --nocapture
//! ```
//!
//! 扩展是长驻进程，分配画像很极端：每行文本一个 3 字符锚点，一次编辑就是几倍行数的
//! 小分配，散落在十几条线程上。实测下来，这些分配释放后会不会归还操作系统**主要不取决
//! 于分配器**：
//!
//! - glibc 会在大块释放后动态抬高 trim / mmap 阈值，历史峰值被永久钉在 RSS 上；
//! - jemalloc 按 dirty decay 主动 purge，能把峰值还回去一部分，但稳态与 glibc 几乎打平
//!   （108.4 MB vs 109.4 MB），峰值却是 2 倍（218.1 MB vs 109.4 MB）、基线是 3 倍
//!   （13.8 MB vs 4.8 MB）。换分配器不划算，所以本 crate 仍然用 glibc。
//!
//! 真正的杠杆是**减少分配量本身**：少分配一次，两种分配器就都少留一份。本测试因此也是
//! 那些改动的验收工具（锚点向量共享、状态借用序列化、竞技场保留上限）。
//!
//! # 为什么要用常驻线程
//!
//! 分配器的归还是**按线程**推进的。如果测试每轮都新起一批线程、做完就退，那些线程的
//! 缓存会立刻变成无人触碰的孤岛，永远等不到归还——这会得出「分配器更差」的错误结论。
//! 线上不是这样：tokio 的 worker 线程与 `spawn_blocking` 线程都活得比单次调用久。
//!
//! 因此本测试用**常驻线程**跑两个阶段：先来一轮热点调用制造峰值，再在静置窗口里持续做
//! 真实的小调用（复刻「扩展空闲时也偶有小事件」），分别打印峰值与静置后的 RSS。前者衡量
//! 工作集，后者才是常驻进程真正的代价。做 A/B 时对照 `SUMMARY` 行即可。
//!
//! # 记一次基线（锚点扁平化之后）
//!
//! 20000 行 / 623782 字节，本机实测：
//!
//! | 口径 | 峰值 | 静置后 |
//! |---|---|---|
//! | 编辑路径（16 常驻线程 × 3 轮） | 130688 KiB | 105960 KiB |
//! | 状态层（6 个 20000 行文件的快照 + 还原点） | 130688 KiB | 130688 KiB |
//!
//! 编辑路径静置后能还回约 24 MB；状态层一点不还——后者的常驻量就是那些快照与还原点本身
//! （`state.json` 9654 KiB），不是分配器攥着不放的碎片。
//!
//! 这组数字是**改动后**的，没有改动前的对照（锚点扁平化之前没在这里留记录），所以它只能
//! 当后续 A/B 的基线，不能拿来断言这次改动降了多少。锚点分配的直接判据在
//! `tests/allocations.rs`：那条不受机器漂移影响。

use std::{
    hint::black_box,
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

use astrcode_ext_common::arena::with_scratch;
use astrcode_ext_hashline_edit::hashline::{
    HashRef,
    anchor::val_edit,
    apply::apply_edit,
    diff::gen_diff,
    hash::{Anchor, line_hashes_pure, map_stable_hashes},
    request::EditRequest,
};
use astrcode_ext_hashline_edit::state::{SessionState, UndoRecord};
use rustc_hash::FxHashSet;

/// 造一份贴近真实源码的行分布：大量唯一行 + 高频重复行。
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

/// 常驻集大小（KiB）。扩展跑在 Linux 上，`/proc/self/statm` 的第二列就是 RSS 页数。
fn rss_kib() -> usize {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("读取 statm 失败");
    let resident: usize = statm
        .split_whitespace()
        .nth(1)
        .expect("statm 缺第二列")
        .parse()
        .expect("statm 第二列不是数字");
    resident * 4
}

/// 跑一次完整的 replace 热点：校验 → 落区间 → 稳定映射 → diff。
fn replace_hot_path(
    content: &str,
    hashes: &[Anchor],
    edit: &EditRequest,
    removed: &FxHashSet<Anchor>,
) {
    let applied = apply_edit(content, edit, Some(hashes), Some("sample.rs"), None).expect("编辑失败");
    let result_hashes =
        map_stable_hashes(content, hashes, &applied.content, removed).expect("映射失败");
    let (diff, _) = gen_diff(
        content,
        &applied.content,
        3,
        Some(&result_hashes),
        Some(hashes),
    );
    black_box(diff);
}

/// 一次调用量级的工作，复刻一次 `replace` 工具调用。
fn one_call(content: &str, hashes: &[Anchor], removed: &FxHashSet<Anchor>, line: usize) {
    let lines = astrcode_ext_hashline_edit::hashline::lines::split_lines(content);
    let edit = EditRequest {
        content_lines: vec!["    // 改过的注释".to_owned()],
        hash_bounds: [
            HashRef {
                hash: hashes[line].to_string(),
            },
            HashRef {
                hash: hashes[line].to_string(),
            },
        ],
    };
    let _ = val_edit(&edit, &lines, hashes).expect("校验失败");
    replace_hot_path(content, hashes, &edit, removed);
}

/// 静置窗口里的轻量工作，用来驱动各 arena 的 decay。
///
/// 必须是**真实的调用量级**：只分配 4 KiB 小对象的话，jemalloc 会全程走线程缓存
/// （tcache），根本不碰 arena，decay 也就永远不会推进——那样测出来的是假阴性。
fn idle_tick() {
    let small = sample_source(200);
    let hashes = line_hashes_pure(&small).expect("分配锚点失败");
    let removed: FxHashSet<Anchor> = [hashes[100]].into_iter().collect();
    one_call(&small, &hashes, &removed, 100);
    black_box(small.len());
}

#[test]
#[ignore = "内存诊断，用 --ignored --nocapture 手动运行"]
fn resident_memory_after_multi_threaded_edits() {
    const LINES: usize = 20_000;
    const THREADS: usize = 16;
    const ROUNDS: usize = 3;
    /// 峰值之后留给分配器归还内存的观察窗口。jemalloc 的 dirty decay 默认 10s。
    const SETTLE: Duration = Duration::from_secs(16);
    const SAMPLE_EVERY: Duration = Duration::from_secs(4);

    let content = Arc::new(sample_source(LINES));
    let hashes = Arc::new(line_hashes_pure(&content).expect("分配锚点失败"));
    let removed: Arc<FxHashSet<Anchor>> = Arc::new([hashes[LINES / 2]].into_iter().collect());

    println!(
        "\n文件：{LINES} 行 / {} 字节，{THREADS} 常驻线程 × {ROUNDS} 轮\n",
        content.len()
    );
    println!("{:>10}  {:>10}", "阶段", "RSS(KiB)");
    println!("{:>10}  {:>10}", "起始", rss_kib());

    let rounds_done = Arc::new(Barrier::new(THREADS + 1));
    let settle_done = Arc::new(Barrier::new(THREADS + 1));

    let start = Instant::now();
    let mut handles = Vec::with_capacity(THREADS);
    for _ in 0..THREADS {
        let content = Arc::clone(&content);
        let hashes = Arc::clone(&hashes);
        let removed = Arc::clone(&removed);
        let rounds_done = Arc::clone(&rounds_done);
        let settle_done = Arc::clone(&settle_done);
        handles.push(std::thread::spawn(move || {
            // 阶段一：制造峰值。线程只起一次，arena 因此会被后续的静置阶段继续复用。
            for round in 0..ROUNDS {
                for call in 0..8 {
                    let line = (LINES / 2 + round * 97 + call) % LINES;
                    one_call(&content, &hashes, &removed, line);
                }
            }
            rounds_done.wait();

            // 阶段二：保持线程存活，用轻量分配驱动本 arena 的 decay。
            let settle_start = Instant::now();
            while settle_start.elapsed() < SETTLE {
                idle_tick();
                std::thread::sleep(Duration::from_millis(50));
            }
            settle_done.wait();

            // 线程本地竞技场刻意保留底层 chunk 复用，它的峰值因此一直挂在线程上。
            // 这里从线程内部取，量到的正是该线程那份。
            with_scratch(|bump| bump.allocated_bytes())
        }));
    }

    rounds_done.wait();
    let peak = rss_kib();
    println!("{:>10}  {:>10}", "峰值", peak);

    let settle_start = Instant::now();
    while settle_start.elapsed() < SETTLE {
        std::thread::sleep(SAMPLE_EVERY);
        println!(
            "{:>9}s  {:>10}",
            settle_start.elapsed().as_secs(),
            rss_kib()
        );
    }

    settle_done.wait();
    let scratch: Vec<usize> = handles
        .into_iter()
        .map(|handle| handle.join().expect("线程 panic"))
        .collect();
    let settled = rss_kib();
    let elapsed = start.elapsed();

    println!("\n--- 单项 ---");
    println!("峰值 RSS            {peak:>10} KiB");
    println!("静置后 RSS          {settled:>10} KiB");
    println!(
        "竞技场保留合计      {:>10} KiB（每线程最大 {} KiB）",
        scratch.iter().sum::<usize>() / 1024,
        scratch.iter().copied().max().unwrap_or(0) / 1024
    );
    println!("总耗时              {elapsed:>12?}");
    println!(
        "SUMMARY lines={LINES} threads={THREADS} rounds={ROUNDS} peak_kib={peak} \
         settled_kib={settled} elapsed_ms={}",
        elapsed.as_millis()
    );
}

/// 复刻一次 `replace` 对状态层的调用序列。
///
/// 这条路径才是线上真正跑的东西：`hashes_for` 命中快照缓存、`save()` 把整个状态落盘。
/// 前者曾经每次深拷贝整份锚点向量，后者曾经每次深拷贝整张状态表——大会话下就是十几 MB
/// 的瞬时分配，每次都把 RSS 高水位往上顶一次。
fn state_tick(state: &mut SessionState, key: &str, content: &str, hashes: &Arc<Vec<Anchor>>) {
    let fetched = state.hashes_for(key, content).expect("取锚点失败");
    state.record_served(key, &fetched);
    state.set_undo(
        key,
        UndoRecord {
            content: content.to_owned(),
            bom: String::new(),
            ending: "\n".to_owned(),
            hashes: Arc::clone(&fetched),
            result_content: content.to_owned(),
        },
    );
    let _ = state.save();
    state.put_snapshot(key, content, Arc::clone(hashes));
    let _ = state.save();
}

/// 状态层的驻留诊断：`hashes_for` 缓存命中 + `save()` 落盘的反复开销。
#[test]
#[ignore = "内存诊断，用 --ignored --nocapture 手动运行"]
fn resident_memory_through_the_state_layer() {
    const LINES: usize = 20_000;
    const KEYS: usize = 6;
    const ROUNDS: usize = 8;
    /// 状态层静置后不再有活动，看分配器会不会把峰值还回去。
    const SETTLE: Duration = Duration::from_secs(12);

    let content = sample_source(LINES);
    let hashes = Arc::new(line_hashes_pure(&content).expect("分配锚点失败"));

    let dir = std::env::temp_dir().join(format!("astrcode-hashline-mem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    let mut state = SessionState::load(dir.join("state.json"));

    // 先把状态撑到「大会话」的量级：多个大文件的快照 + 还原点（每个还原点两份全文）。
    for index in 0..KEYS {
        state_tick(&mut state, &format!("f{index}.rs"), &content, &hashes);
    }
    let state_bytes = std::fs::metadata(dir.join("state.json")).map_or(0, |meta| meta.len());

    println!(
        "\n状态：{KEYS} 个大文件（各 {LINES} 行）的快照 + 还原点，state.json {} KiB\n",
        state_bytes / 1024
    );
    println!("{:>10}  {:>10}", "阶段", "RSS(KiB)");
    println!("{:>10}  {:>10}", "起始", rss_kib());

    let start = Instant::now();
    for round in 0..ROUNDS {
        for index in 0..KEYS {
            state_tick(&mut state, &format!("f{index}.rs"), &content, &hashes);
        }
        if round % 2 == 1 {
            println!("{:>10}  {:>10}", format!("轮 {}", round + 1), rss_kib());
        }
    }
    let peak = rss_kib();

    let settle_start = Instant::now();
    while settle_start.elapsed() < SETTLE {
        std::thread::sleep(Duration::from_secs(4));
        println!("{:>9}s  {:>10}", settle_start.elapsed().as_secs(), rss_kib());
    }
    let settled = rss_kib();
    let elapsed = start.elapsed();

    println!("\n--- 单项 ---");
    println!("峰值 RSS            {peak:>10} KiB");
    println!("静置后 RSS          {settled:>10} KiB");
    println!("state.json          {:>10} KiB", state_bytes / 1024);
    println!("总耗时              {elapsed:>12?}");
    println!(
        "SUMMARY-STATE lines={LINES} keys={KEYS} rounds={ROUNDS} peak_kib={peak} \
         settled_kib={settled} elapsed_ms={}",
        elapsed.as_millis()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
