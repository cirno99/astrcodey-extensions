//! 分配计数诊断：锚点分配是不是「整块一次」。
//!
//! 默认跑（这条断言很快），但也可以单独看数字：
//!
//! ```sh
//! cargo test -p astrcode-ext-hashline-edit --test allocations -- --nocapture
//! ```
//!
//! 时间基准（`tests/performance.rs`）量的是「快了多少」，受机器状态漂移影响很大；这里量的是
//! **分配次数**，它不受漂移影响，正好是锚点表示改动的直接判据：`Vec<String>` 是每行一次
//! `malloc`，`Vec<Anchor>` 是整块一次。
//!
//! 这条测试在旧表示下必然失败——两万行文件会数出两万多次分配。

use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
};

use astrcode_ext_hashline_edit::hashline::hash::{Anchor, line_hashes_pure};

/// 只累加次数、转发给系统分配器的分配器。
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// 数一次调用里发生的分配次数。
fn allocations(body: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    body();
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

/// 造一份贴近真实源码的行分布：大量唯一行 + 高频重复行。
fn sample_source(lines: usize) -> String {
    let mut out = String::with_capacity(lines * 40);
    for index in 0..lines {
        match index % 7 {
            0 => out.push_str("}\n"),
            1 => out.push_str("    // 中文注释\n"),
            2 => out.push_str(&format!("    let value_{index} = compute({index});\n")),
            3 => out.push_str("\n"),
            4 => out.push_str("    return Ok(());\n"),
            5 => out.push_str(&format!("    // 第 {index} 行的说明\n")),
            _ => out.push_str("        self.inner.flush()?;\n"),
        }
    }
    out
}

/// 锚点向量是扁平的 3 字节缓冲，不是每行一个堆指针。
#[test]
fn an_anchor_is_three_bytes_with_no_heap_pointer() {
    assert_eq!(std::mem::size_of::<Anchor>(), 3);
    // `Option<Anchor>` 也没有 niche 可用，因此是 3 字节 + 判别位。
    assert_eq!(std::mem::size_of::<Option<Anchor>>(), 4);
}

/// 行数涨 20 倍，分配次数不跟着涨 —— 这就是「每行一次 malloc 变成整块一次」。
#[test]
fn anchor_allocation_does_not_scale_with_line_count() {
    let small = sample_source(1_000);
    let large = sample_source(20_000);

    // 预热：先把线程本地竞技场的块建好，把一次性开销排除在计数之外。
    black_box(line_hashes_pure(&small).expect("分配失败"));

    let small_allocations = allocations(|| {
        black_box(line_hashes_pure(&small).expect("分配失败"));
    });
    let large_allocations = allocations(|| {
        black_box(line_hashes_pure(&large).expect("分配失败"));
    });

    println!(
        "line_hashes_pure：1000 行 {small_allocations} 次分配，20000 行 {large_allocations} 次分配"
    );

    assert!(
        large_allocations <= small_allocations + 8,
        "分配次数随行数增长了：{small_allocations} -> {large_allocations}"
    );
    // 旧表示下这里是两万多次（每行一个 String）。
    assert!(
        large_allocations < 200,
        "锚点分配退化成了每行一次：{large_allocations}"
    );
}
