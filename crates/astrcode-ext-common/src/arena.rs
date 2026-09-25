//! bumpalo 竞技场：为单次命令 / 钩子调用提供临时内存。
//!
//! 扩展是长驻进程。一次调用里如果要做「扫描 → 分类 → 汇总 → 渲染」多趟处理，
//! 每趟都用 `String` / `Vec` 会产生大量短命堆分配，反复进出全局分配器。
//!
//! [`Scratch`] 复用一个 [`Bump`]：一次调用结束时 [`Scratch::finish`] 只把分配
//! 指针拨回起点，底层 chunk 全部保留，后续调用几乎不再向全局分配器申请内存。
//!
//! **生命周期约束**：竞技场里的数据在 `finish()` 后立即失效，因此只用于调用
//! 内部的中间量；最终交给宿主的字符串仍以普通 `String` 返回。
//!
//! **跨 await 约束**：[`Bump`] 不是 `Sync`，`&Bump` 因而不是 `Send`。worker 的
//! handler future 必须是 `Send`，所以竞技场句柄不能跨 `.await` 持有——先完成
//! 全部异步取数，再在同步区块里用竞技场做汇总与渲染。
//!
//! **怎么用**：自己持有 [`Scratch`] 时走 [`Scratch::scope`]；不关心句柄归属的调用点
//! 直接走 [`with_scratch`]，它借用线程本地实例。

use std::cell::RefCell;

use bumpalo::Bump;
use bumpalo::collections::String as ArenaString;

/// 可复用的竞技场句柄。
#[derive(Debug)]
pub(crate) struct Scratch {
    arena: Bump,
    resets: u64,
    peak_reserved: usize,
}

impl Scratch {
    /// 创建空竞技场（首次分配时按需扩块）。
    pub(crate) fn new() -> Self {
        Self::with_capacity(0)
    }

    /// 创建预置容量的竞技场，适合已知单次处理量级的场景。
    pub(crate) fn with_capacity(bytes: usize) -> Self {
        Self {
            arena: Bump::with_capacity(bytes),
            resets: 0,
            peak_reserved: 0,
        }
    }

    /// 底层竞技场，传给本模块的分配辅助函数使用。
    #[cfg(test)]
    pub(crate) fn arena(&self) -> &Bump {
        &self.arena
    }

    /// 底层 chunk 当前占用的字节数（含为复用保留的空闲块）。
    ///
    /// 该值在 [`Scratch::finish`] 之后不会归零，因为 chunk 被刻意保留复用。
    #[cfg(test)]
    pub(crate) fn reserved_bytes(&self) -> usize {
        self.arena.allocated_bytes()
    }

    /// 历史峰值 chunk 占用，用于判断预置容量是否合理。
    #[cfg(test)]
    pub(crate) fn peak_reserved_bytes(&self) -> usize {
        self.peak_reserved
    }

    /// 已完成（被复位）的调用次数。
    #[cfg(test)]
    pub(crate) fn resets(&self) -> u64 {
        self.resets
    }

    /// 在竞技场里跑一段同步逻辑，结束后立即复位。
    ///
    /// 回调拿到的是 `&Bump`，其生命周期与返回类型 `R` 无关（`R` 不能借用竞技场），
    /// 因此「临时量不会漏到调用之外」由类型系统保证，不需要靠约定。
    pub(crate) fn scope<R>(&mut self, body: impl FnOnce(&Bump) -> R) -> R {
        let out = body(&self.arena);
        self.finish();
        out
    }

    /// 结束一次调用：释放全部临时分配，保留底层 chunk 供下次复用。
    pub(crate) fn finish(&mut self) {
        self.peak_reserved = self.peak_reserved.max(self.arena.allocated_bytes());
        self.arena.reset();
        self.resets += 1;
    }
}

impl Default for Scratch {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// 线程本地的可复用竞技场。
    ///
    /// 扩展进程常驻，线程也常驻，因此底层 chunk 只会在首次调用时向全局分配器申请，
    /// 之后一直复用。`Bump` 不是 `Sync`，只能线程本地持有。
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::with_capacity(DEFAULT_SCRATCH_BYTES));
}

/// 线程本地竞技场的初始容量：覆盖「几千行文本的多趟扫描」这一常见量级。
const DEFAULT_SCRATCH_BYTES: usize = 64 * 1024;

/// 借用线程本地竞技场跑一段同步逻辑，回调返回后整体复位。
///
/// 这是给「同一个线程上反复发生的调用」用的入口：调用方不必自己持有 [`Scratch`]。
/// 回调内**不能** `.await`（[`Bump`] 不是 `Send`），也不能把借用传出回调。
pub fn with_scratch<R>(body: impl FnOnce(&Bump) -> R) -> R {
    SCRATCH.with(|cell| cell.borrow_mut().scope(body))
}

/// 用分隔符把若干片段连接成一个竞技场字符串。
pub fn join<'a>(arena: &'a Bump, parts: &[&str], sep: &str) -> ArenaString<'a> {
    let total: usize =
        parts.iter().map(|p| p.len()).sum::<usize>() + sep.len() * parts.len().saturating_sub(1);
    let mut out = ArenaString::with_capacity_in(total, arena);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        out.push_str(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_keeps_chunk_for_reuse() {
        let mut scratch = Scratch::with_capacity(4096);
        let bulk = "x".repeat(512);
        let _ = join(scratch.arena(), &[&bulk, &bulk, &bulk], " · ");
        let after_first = scratch.reserved_bytes();
        assert!(after_first > 0);

        scratch.finish();
        assert_eq!(scratch.resets(), 1);
        assert!(scratch.peak_reserved_bytes() >= after_first);

        // 复位后底层 chunk 保留，同量级再分配不应触发新的块增长。
        let _ = join(scratch.arena(), &[&bulk, &bulk, &bulk], " · ");
        assert!(scratch.reserved_bytes() <= after_first);
        scratch.finish();
        assert_eq!(scratch.resets(), 2);
    }

    #[test]
    fn scope_resets_the_arena_after_the_call() {
        let mut scratch = Scratch::with_capacity(4096);
        let bulk = "x".repeat(512);
        let first = scratch.scope(|arena| {
            let _ = join(arena, &[&bulk, &bulk, &bulk], " · ");
            arena.allocated_bytes()
        });
        assert!(first > 0);
        assert_eq!(scratch.resets(), 1);

        // 复位保留底层 chunk，同量级再分配不触发新的块增长。
        let second = scratch.scope(|arena| {
            let _ = join(arena, &[&bulk, &bulk, &bulk], " · ");
            arena.allocated_bytes()
        });
        assert_eq!(scratch.resets(), 2);
        assert!(second <= first);
    }

    #[test]
    fn with_scratch_reuses_one_arena_across_calls() {
        let bulk = "y".repeat(4096);
        let first = with_scratch(|arena| {
            let _ = join(arena, &[&bulk], "");
            arena.allocated_bytes()
        });
        let second = with_scratch(|arena| {
            let _ = join(arena, &[&bulk], "");
            arena.allocated_bytes()
        });
        assert!(first > 0);
        // 两次的已占用字节数相同，说明第二次没有把上一次的分配留在场上。
        assert_eq!(first, second);
    }

    #[test]
    fn join_inserts_separator_between_parts_only() {
        let scratch = Scratch::with_capacity(64);
        let got = join(scratch.arena(), &["a", "b", "c"], " -> ");
        assert_eq!(got.as_str(), "a -> b -> c");
    }

    #[test]
    fn join_with_no_parts_is_empty() {
        let scratch = Scratch::with_capacity(16);
        let got = join(scratch.arena(), &[], " -> ");
        assert_eq!(got.as_str(), "");
    }

    #[test]
    fn join_with_single_part_has_no_separator() {
        let scratch = Scratch::with_capacity(16);
        let got = join(scratch.arena(), &["only"], " -> ");
        assert_eq!(got.as_str(), "only");
    }
}
