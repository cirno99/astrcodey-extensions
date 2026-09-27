//! 锚点分配与稳定映射。
//!
//! 每行拿到一个 3 字符的字母数字锚点，它就是这一行的地址。分配策略是原版的核心：
//!
//! 1. **内容派生基址** —— `(xxh32(canon(line)) >> 14) % 238328`。同样的内容永远
//!    落到同一个基址，所以哈希对相同文件是确定的。
//! 2. **开放寻址探测** —— 基址被占用时按固定步长（3907，与 238328 互质）线性探测
//!    下一个空位。这保证同一文件内锚点唯一，也让「连续空行」这类同内容行各自拿到
//!    不同锚点。
//! 3. **编辑时的稳定映射** —— [`map_stable_hashes`] 让未被触碰的行保留原锚点，
//!    因此模型可以连续编辑而不必重新读文件。
//!
//! 238328 = 62³ 是 3 字符锚点的容量上限，超过就再也分配不出唯一锚点。
//!
//! # 关于哈希器（实测结论）
//!
//! 这里的查找表都以行内容或锚点为键，每次编辑要对整份文件重建几遍。键空间是用户
//! 自己的源码，不是外部可控的对抗性输入，所以 SipHash 的 HashDoS 防护换不来收益。
//!
//! 实测（5000 行 / 154467 字节，两个二进制交替 6 轮取最小值，见
//! `tests/performance.rs` 与 `tests/hasher_comparison.rs`）：
//!
//! | 项 | std | FxHash | 加速 |
//! |---|---|---|---|
//! | 纯散列 5000 键 | 36.4µs | 8.9µs | 4.11x |
//! | `FxHashMap<&str, Vec<usize>>` 建表+查找 | 297µs | 141µs | 2.12x |
//! | `val_edit` | 638.7µs | 504.2µs | 1.27x |
//! | `map_stable_hashes` | 1579.3µs | 1260.3µs | 1.25x |
//! | 完整 replace 路径 | 2739.2µs | 2201.2µs | 1.24x |
//!
//! 对照组 `canon`（不用哈希表）只差 2%，说明残余噪声远小于上述差异。因此这里采用
//! FxHash。
//!
//! 记一次教训：**先给整个插件换哈希器再各跑一次，是无效对照**。两次跑在不同的机器
//! 状态上，漂移（连 `canon` 都同向偏 10%）会完全盖住 24% 的真实差值，得出相反的
//! 结论。要么在同一个二进制里交替测，要么两个二进制交替跑取最小值。
//!
//! 上表是**换哈希器时**的 A/B，绝对数字此后又被「`canon` 返回 `Cow`」与「查找表用
//! 竞技场」两轮改动压低了一截。同机、同一份输入，改动前后两个二进制交替跑三轮取最小值
//! （`tests/performance.rs`）：
//!
//! | 项 | 改动前 | 改动后 | 加速 |
//! |---|---|---|---|
//! | `canon` 全文件 | 157.1µs | 37.2µs | 4.23x |
//! | `line_hashes_pure` | 510.3µs | 367.1µs | 1.39x |
//! | `val_edit` | 498.1µs | 173.9µs | 2.87x |
//! | `map_stable_hashes` | 1255.5µs | 874.5µs | 1.44x |
//! | 完整 replace 路径 | 2119.8µs | 1431.5µs | 1.48x |
//!
//! 大头是 `canon` 不再逐行 `String` 分配（LF 文件里 `trim_end` 直接给出子切片），
//! 其次是两张按内容分组的查找表改成竞技场分配，省掉每行一次全局分配。
//!
//! # 锚点表示：`Vec<String>` → `Vec<Anchor>`
//!
//! 锚点原来每行一个 3 字符 `String`，也就是每行一次 `malloc`；现在 [`Anchor`] 是
//! `[u8; 3]`，`Vec<Anchor>` 就是 3N 字节的连续缓冲，整份向量只分配一次。
//!
//! 直接判据是**分配次数**，它不受机器状态漂移影响（`tests/allocations.rs` 用计数分配器）：
//!
//! | `line_hashes_pure` | 分配次数 |
//! |---|---|
//! | 1000 行 | 11 |
//! | 20000 行 | 16 |
//!
//! 行数涨 20 倍而分配次数几乎不动；旧表示下这里是两万多次。同机这一项从 367.1µs 降到
//! 158.5µs。
//!
//! 但**端到端编辑路径没有跟着受益**：完整 replace 从 1431.5µs 到 1403.7µs，落在噪声里。
//! 原因有两层：
//!
//! 1. `replace` 走的是 `precomputed_hashes`，根本不调用 `line_hashes_pure`。这一项的收益
//!    落在**首次读入一个文件**上（`read` 工具，以及某个文件的第一次 `replace`）。
//! 2. 编辑路径的大头是按内容分组的哈希表查找与 diff 生成，锚点分配只占其中一小块。
//!
//! 上面这组数字是**跨轮次**比较，不是严格 A/B：「改动前」那一列是上一轮改动时测的。
//! 对照组 `canon`（这次改动碰不到它）同机从 37.2µs 漂到 33.1µs，即本轮机器状态快约 11%，
//! 所以 `replace` 那 2% 的「提升」不能算数。要严格对照，得像换哈希器那次一样把旧表示的
//! 二进制也编出来交替跑。

use std::{borrow::Cow, fmt};

use astrcode_ext_common::arena::with_scratch;
use bumpalo::Bump;
use bumpalo::collections::Vec as ArenaVec;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error as DeError, Visitor},
};

use super::{
    error::{EditError, ErrorCode},
    lines::{canon, split_lines},
    xxh32::xxh32_str,
};

/// 锚点长度。
pub const HASH_LEN: usize = 3;
/// 锚点与行内容之间的分隔符（U+2502），与原版一致。
pub const HASH_SEP: char = '│';
/// 锚点字母表：大写、小写、数字。
const ALPH: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const ALPH_SIZE: usize = 62;
/// 3 字符锚点的容量。
pub const HASH_SPACE: usize = 238_328;
/// 线性探测步长。与 [`HASH_SPACE`] 互质，因此能遍历整个空间。
const HASH_PROBE_STRIDE: usize = ALPH_SIZE * ALPH_SIZE + ALPH_SIZE + 1;
/// 位图需要的 u32 个数。
const BITSET_WORDS: usize = HASH_SPACE.div_ceil(32);

/// 一个 3 字符锚点 —— 行的地址。
///
/// 表示成 `[u8; 3]` 而不是 `String`，是为了让整份文件的锚点向量退化成**一次**分配：
/// `Vec<Anchor>` 就是 3N 字节的连续缓冲，而 `Vec<String>` 是 N 次小分配。编辑路径上
/// 每行一次 malloc 正是常驻内存高水位的来源。
///
/// 布局与 `Box<[u8]>` 完全相同，但保留了 `hashes[i]` 索引（返回 `&Anchor`），调用方
/// 只需换类型，不必改索引写法。
///
/// 磁盘形状仍是「3 字符字符串的数组」（见下面的手写 serde）：derive 会把 `[u8; 3]`
/// 写成数字数组 `[97,66,51]`，那是破坏格式。`Ord` 按字节序导出，与 `String` 的字节序
/// 一致，所以以锚点为键的 `BTreeSet`（`served`）迭代顺序不变。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Anchor([u8; HASH_LEN]);

impl AsRef<str> for Anchor {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Anchor {
    /// 从字面锚点构造；长度或字母表不符时 panic。
    ///
    /// 是 `const fn`，所以测试里把锚点写错会在编译期就报错。
    pub const fn new(text: &str) -> Self {
        let bytes = text.as_bytes();
        assert!(bytes.len() == HASH_LEN, "锚点必须是 3 个字符");
        let mut out = [0u8; HASH_LEN];
        let mut index = 0;
        while index < HASH_LEN {
            let byte = bytes[index];
            assert!(byte.is_ascii_alphanumeric(), "锚点只能是 ASCII 字母数字");
            out[index] = byte;
            index += 1;
        }
        Self(out)
    }

    /// 校验并构造；不是合法锚点时返回 `None`。
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != HASH_LEN || !text.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return None;
        }
        let mut out = [0u8; HASH_LEN];
        out.copy_from_slice(text.as_bytes());
        Some(Self(out))
    }

    /// 零拷贝的字符串视图。锚点恒为 ASCII，UTF-8 校验不会失败。
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("锚点恒为 ASCII")
    }

    /// 62 进制解码出的序号。
    pub fn index(&self) -> usize {
        let mut index = 0usize;
        for byte in self.0 {
            let position = ALPH
                .iter()
                .position(|&candidate| candidate == byte)
                .expect("锚点字符必在字母表内");
            index = index * ALPH_SIZE + position;
        }
        index
    }
}

impl fmt::Display for Anchor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Debug for Anchor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Anchor({})", self.as_str())
    }
}

impl Serialize for Anchor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Anchor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AnchorVisitor;

        impl<'a> Visitor<'a> for AnchorVisitor {
            type Value = Anchor;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("一个 3 字符的哈希锚点")
            }

            fn visit_str<E: DeError>(self, value: &str) -> Result<Anchor, E> {
                Anchor::parse(value).ok_or_else(|| E::custom("锚点必须是 3 个 ASCII 字母数字字符"))
            }
        }

        deserializer.deserialize_str(AnchorVisitor)
    }
}

/// 判断一个字符串是不是合法的裸锚点。
pub fn is_hash(text: &str) -> bool {
    Anchor::parse(text).is_some()
}

/// 把序号渲染成 3 字符锚点（大端 62 进制）。
pub fn idx_to_hash(mut index: usize) -> Anchor {
    let mut out = [0u8; HASH_LEN];
    for slot in out.iter_mut().rev() {
        *slot = ALPH[index % ALPH_SIZE];
        index /= ALPH_SIZE;
    }
    Anchor(out)
}

/// [`idx_to_hash`] 的逆运算；不是合法锚点时返回 `None`。
pub fn hash_to_index(hash: &str) -> Option<usize> {
    Anchor::parse(hash).map(|anchor| anchor.index())
}

/// 内容派生的基址。
fn base_index(canonical: &str) -> usize {
    (xxh32_str(canonical, 0) >> 14) as usize % HASH_SPACE
}

/// 已占用锚点的位图。
struct BitSet {
    words: Vec<u32>,
}

impl BitSet {
    fn new() -> Self {
        Self {
            words: vec![0; BITSET_WORDS],
        }
    }

    fn get(&self, index: usize) -> bool {
        self.words[index >> 5] >> (index & 31) & 1 != 0
    }

    fn set(&mut self, index: usize) {
        self.words[index >> 5] |= 1 << (index & 31);
    }

    /// 从 `start` 起按探测步长找第一个空位。整个空间都占满时返回 `None`。
    fn next_zero(&self, start: usize) -> Option<usize> {
        let mut index = start % HASH_SPACE;
        for _ in 0..HASH_SPACE {
            if !self.get(index) {
                return Some(index);
            }
            index += HASH_PROBE_STRIDE;
            if index >= HASH_SPACE {
                index -= HASH_SPACE;
            }
        }
        None
    }
}

/// 分配器：位图加一个探测提示位。
///
/// `hint` 让连续分配不必每次从 0 开始扫描；`occupy_hash` 在置位的同时把提示位推到
/// 该锚点之后，与 `occupy`（只置位）区分开，因为原版就是这样。
struct Assigner {
    used: BitSet,
    hint: usize,
}

impl Assigner {
    fn new() -> Self {
        Self {
            used: BitSet::new(),
            hint: 0,
        }
    }

    /// 只占位，不推进探测提示。
    fn occupy(&mut self, index: usize) {
        self.used.set(index);
    }

    /// 占位并推进探测提示（用于「原样保留」的锚点）。
    fn occupy_hash(&mut self, hash: Anchor) {
        let index = hash.index();
        self.used.set(index);
        if index + HASH_PROBE_STRIDE > self.hint {
            self.hint = index + HASH_PROBE_STRIDE;
        }
    }

    /// 为 `base` 分配一个唯一锚点：基址空闲就直接用，否则探测下一个空位。
    fn assign(&mut self, base: usize) -> Result<Anchor, EditError> {
        if !self.used.get(base) {
            self.used.set(base);
            self.hint = base + HASH_PROBE_STRIDE;
            return Ok(idx_to_hash(base));
        }
        let next = self.used.next_zero(self.hint).ok_or_else(anchor_space_exhausted)?;
        self.used.set(next);
        self.hint = next + HASH_PROBE_STRIDE;
        Ok(idx_to_hash(next))
    }
}

/// 锚点空间耗尽时的错误。100MB / 238328 行上限里的后半句。
fn anchor_space_exhausted() -> EditError {
    EditError::new(
        ErrorCode::FileTooLarge,
        format!(
            "Cannot allocate a unique hash anchor: the file exceeds the {HASH_SPACE}-line limit \
             for {HASH_LEN}-char hashline anchors. For very large files use write or a \
             non-line-based approach."
        ),
    )
}

/// 为整个文件重新分配锚点。相同内容永远得到相同结果。
pub fn line_hashes_pure(content: &str) -> Result<Vec<Anchor>, EditError> {
    let lines = split_lines(content);
    let mut assigner = Assigner::new();
    let mut hashes = Vec::with_capacity(lines.len());
    for line in lines {
        let base = base_index(&canon(line));
        hashes.push(assigner.assign(base)?);
    }
    Ok(hashes)
}

/// 编辑后重算锚点，尽量让未被触碰的行保留原锚点。
///
/// `removed_hashes` 是被替换掉的那些行的锚点。映射分四步，顺序不能换：
///
/// 1. 幸存行按内容就近复用原锚点（这样「改一行」不会动其他行的地址）。
/// 2. 新内容里与被删行同内容的行，继承被删掉的锚点（这样「原地改写」也不会换地址）。
/// 3. 剩下的行重新按内容派生基址分配。
pub fn map_stable_hashes(
    old_content: &str,
    old_hashes: &[Anchor],
    new_content: &str,
    removed_hashes: &FxHashSet<Anchor>,
) -> Result<Vec<Anchor>, EditError> {
    // 下面两张查找表都是调用内部的临时量，返回前整体复位。
    with_scratch(|bump| {
        map_stable_hashes_in(bump, old_content, old_hashes, new_content, removed_hashes)
    })
}

fn map_stable_hashes_in(
    bump: &Bump,
    old_content: &str,
    old_hashes: &[Anchor],
    new_content: &str,
    removed_hashes: &FxHashSet<Anchor>,
) -> Result<Vec<Anchor>, EditError> {
    let old_lines = split_lines(old_content);
    let new_lines = split_lines(new_content);
    let mut new_hashes: Vec<Option<Anchor>> = vec![None; new_lines.len()];
    let mut assigner = Assigner::new();

    let mut old_hash_index: FxHashMap<&str, usize> = FxHashMap::default();
    for (index, hash) in old_hashes.iter().enumerate() {
        old_hash_index.insert(hash.as_str(), index);
        assigner.occupy(hash.index());
    }

    let removed_indexes: FxHashSet<usize> = removed_hashes
        .iter()
        .filter_map(|hash| old_hash_index.get(hash.as_str()).copied())
        .collect();
    // 只有 0/1 个被删行时不必排序；否则后面需要按行号升序处理。
    let span_start = removed_indexes.iter().min().copied();
    let span_end = removed_indexes.iter().max().copied();
    // 没有删除时 spanEnd 视为 -1，于是所有幸存行都走 `index + shift` 分支（shift 为 0）。
    let span_end_signed = span_end.map_or(-1isize, |end| end as isize);
    let shift_after_span = match (span_start, span_end) {
        (Some(start), Some(end)) => {
            let span_len = (end - start + 1) as isize;
            let replacement_len = new_lines.len() as isize - old_lines.len() as isize + span_len;
            replacement_len - span_len
        },
        _ => 0,
    };

    let mut survivors: ArenaVec<usize> = ArenaVec::new_in(bump);
    let mut removed_entries: ArenaVec<usize> = ArenaVec::new_in(bump);
    for index in 0..old_lines.len() {
        if removed_indexes.contains(&index) {
            removed_entries.push(index);
        } else {
            survivors.push(index);
        }
    }
    // 被删行必须按行号升序参与后续的锚点继承排队。
    removed_entries.as_mut_slice().sort_unstable();

    let mut new_by_content: FxHashMap<Cow<'_, str>, ArenaVec<'_, usize>> = FxHashMap::default();
    for (index, line) in new_lines.iter().enumerate() {
        new_by_content
            .entry(canon(line))
            .or_insert_with(|| ArenaVec::new_in(bump))
            .push(index);
    }

    for &index in &survivors {
        let key = canon(old_lines[index]);
        let Some(candidates) = new_by_content.get_mut(key.as_ref()) else {
            continue;
        };
        if candidates.is_empty() {
            continue;
        }
        let target = if index as isize > span_end_signed {
            index as isize + shift_after_span
        } else {
            index as isize
        };
        let Some(position) = nearest_new(candidates, target) else {
            continue;
        };
        let new_index = candidates.remove(position);
        new_hashes[new_index] = Some(old_hashes[index].clone());
        assigner.occupy_hash(old_hashes[index]);
    }

    // 被删行的锚点按内容排队，供新内容里同内容的行继承。
    let mut removed_by_content: FxHashMap<Cow<'_, str>, (Vec<Anchor>, usize)> =
        FxHashMap::default();
    for &index in &removed_entries {
        let entry = removed_by_content
            .entry(canon(old_lines[index]))
            .or_insert_with(|| (Vec::new(), 0));
        entry.0.push(old_hashes[index].clone());
    }

    for index in 0..new_lines.len() {
        if new_hashes[index].is_some() {
            continue;
        }
        let key = canon(new_lines[index]);
        let Some((hashes, position)) = removed_by_content.get_mut(key.as_ref()) else {
            continue;
        };
        if *position >= hashes.len() {
            continue;
        }
        new_hashes[index] = Some(hashes[*position].clone());
        *position += 1;
    }

    for index in 0..new_lines.len() {
        if new_hashes[index].is_some() {
            continue;
        }
        let base = base_index(&canon(new_lines[index]));
        new_hashes[index] = Some(assigner.assign(base)?);
    }

    Ok(new_hashes
        .into_iter()
        .map(|hash| hash.expect("每个位置都已被上面三步填满"))
        .collect())
}

/// 在升序候选里找离 `target` 最近的一个，返回其下标。
///
/// 距离相同时取左侧（与原版一致，保证结果确定）。
fn nearest_new(candidates: &[usize], target: isize) -> Option<usize> {
    let lower_bound = candidates.partition_point(|&candidate| (candidate as isize) < target);
    let left = lower_bound as isize - 1;
    let right = lower_bound;
    if left >= 0
        && (right >= candidates.len()
            || target - candidates[left as usize] as isize
                <= candidates[right] as isize - target)
    {
        return Some(left as usize);
    }
    if right < candidates.len() {
        return Some(right);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes(content: &str) -> Vec<Anchor> {
        line_hashes_pure(content).expect("分配锚点失败")
    }

    /// 把字面锚点列表转成锚点向量，让金标准用例里的期望值保持可读。
    fn anchors(texts: &[&str]) -> Vec<Anchor> {
        texts.iter().map(|text| Anchor::new(text)).collect()
    }

    #[test]
    fn anchors_are_three_alphanumeric_chars_and_unique() {
        let content = "alpha\nbeta\ngamma\ndelta\n";
        let anchors = hashes(content);
        assert_eq!(anchors.len(), 4);
        for anchor in &anchors {
            assert!(is_hash(anchor.as_str()), "{anchor} 不是合法锚点");
        }
        let unique: FxHashSet<&Anchor> = anchors.iter().collect();
        assert_eq!(unique.len(), anchors.len());
    }

    #[test]
    fn assignment_is_deterministic_for_identical_content() {
        let content = "alpha\nbeta\ngamma\ndelta\n";
        assert_eq!(hashes(content), hashes(content));
    }

    #[test]
    fn blank_line_runs_still_get_unique_anchors() {
        let content = "a\n\n\n\n\nb\n";
        let anchors = hashes(content);
        let unique: FxHashSet<&Anchor> = anchors.iter().collect();
        assert_eq!(unique.len(), anchors.len());
        assert_eq!(anchors.len(), 6);
    }

    #[test]
    fn large_file_stays_unique() {
        let content: String = (0..5000)
            .map(|index| format!("line number {index} with some content\n"))
            .collect();
        let anchors = hashes(&content);
        assert_eq!(anchors.len(), 5000);
        let unique: FxHashSet<&Anchor> = anchors.iter().collect();
        assert_eq!(unique.len(), 5000);
    }

    #[test]
    fn idx_to_hash_and_hash_to_index_round_trip() {
        for index in [0usize, 1, 61, 62, 3843, HASH_SPACE - 1] {
            let hash = idx_to_hash(index);
            assert_eq!(hash.index(), index);
            assert_eq!(hash_to_index(hash.as_str()), Some(index));
        }
        assert_eq!(hash_to_index("aB"), None);
        assert_eq!(hash_to_index("aB3x"), None);
        assert_eq!(hash_to_index("a│3"), None);
    }

    #[test]
    fn is_hash_rejects_wrong_shapes() {
        assert!(is_hash("aB3"));
        assert!(!is_hash("aB"));
        assert!(!is_hash("aB34"));
        assert!(!is_hash("a│3"));
        assert!(!is_hash(""));
    }

    #[test]
    fn map_stable_hashes_keeps_untouched_anchors() {
        let old_content = "one\ntwo\nthree\nfour\nfive\n";
        let old_hashes = hashes(old_content);
        let new_content = "one\ntwo\nTHREE\nfour\nfive\n";
        let removed: FxHashSet<Anchor> = [old_hashes[2]].into_iter().collect();
        let new_hashes =
            map_stable_hashes(old_content, &old_hashes, new_content, &removed).expect("映射失败");

        assert_eq!(new_hashes[0], old_hashes[0]);
        assert_eq!(new_hashes[1], old_hashes[1]);
        assert_eq!(new_hashes[3], old_hashes[3]);
        assert_eq!(new_hashes[4], old_hashes[4]);
        let unique: FxHashSet<&Anchor> = new_hashes.iter().collect();
        assert_eq!(unique.len(), new_hashes.len());
    }

    #[test]
    fn map_stable_hashes_handles_insertion() {
        let old_content = "a\nb\nc\n";
        let old_hashes = hashes(old_content);
        let new_content = "a\nb\nB2\nc\n";
        let removed: FxHashSet<Anchor> = [old_hashes[1]].into_iter().collect();
        let new_hashes =
            map_stable_hashes(old_content, &old_hashes, new_content, &removed).expect("映射失败");

        assert_eq!(new_hashes.len(), 4);
        assert_eq!(new_hashes[0], old_hashes[0]);
        assert_eq!(new_hashes[3], old_hashes[2]);
        let unique: FxHashSet<&Anchor> = new_hashes.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    /// 删掉一行、又在原位插入同内容的行：锚点应当被继承而不是重新分配。
    #[test]
    fn map_stable_hashes_reuses_removed_anchors_for_same_content() {
        let old_content = "a\nb\nc\n";
        let old_hashes = hashes(old_content);
        let removed: FxHashSet<Anchor> = [old_hashes[1]].into_iter().collect();
        let new_hashes =
            map_stable_hashes(old_content, &old_hashes, "a\nb\nc\n", &removed).expect("映射失败");
        assert_eq!(new_hashes, old_hashes);
    }

    #[test]
    fn map_stable_hashes_handles_pure_insertion_without_removals() {
        let old_content = "a\nb\n";
        let old_hashes = hashes(old_content);
        let new_hashes =
            map_stable_hashes(old_content, &old_hashes, "a\nX\nb\n", &FxHashSet::default())
                .expect("映射失败");
        assert_eq!(new_hashes.len(), 3);
        assert_eq!(new_hashes[0], old_hashes[0]);
        assert_eq!(new_hashes[2], old_hashes[1]);
    }

    /// 金标准：期望值由原版纯核心（`probe.js` 跑 `src/host.js`）实测得出。
    /// 锚点就是地址，跨实现差一位就等于换掉整套地址。
    #[test]
    fn matches_the_original_implementation() {
        assert_eq!(
            hashes("function hello() {\n  console.log(\"world\");\n}\n\n// end\n"),
            anchors(&["EuR", "AZx", "AU6", "AuN", "BNk"])
        );
        assert_eq!(hashes(""), anchors(&["AuN"]));
        assert_eq!(
            hashes("a\n\n\n\n\nb\n"),
            anchors(&["Wot", "AuN", "BvO", "CwP", "DxQ", "rKa"])
        );
        assert_eq!(hashes("错\n误\n"), anchors(&["TAJ", "A9H"]));
        assert_eq!(
            hashes(&format!("{}\n{}\n", "x".repeat(300), "y".repeat(300))),
            anchors(&["hXb", "TSA"])
        );
    }

    /// 稳定映射的金标准，同样取自原版实测。
    #[test]
    fn stable_mapping_matches_the_original_implementation() {
        let old_content = "one\ntwo\nthree\nfour\nfive\n";
        let old_hashes = hashes(old_content);
        let removed: FxHashSet<Anchor> = [old_hashes[2]].into_iter().collect();
        assert_eq!(
            map_stable_hashes(
                old_content,
                &old_hashes,
                "one\ntwo\nTHREE\nfour\nfive\n",
                &removed
            )
            .expect("映射失败"),
            anchors(&["hvX", "n4z", "BHz", "N4i", "LKH"])
        );

        let old_content = "a\nb\nc\n";
        let old_hashes = hashes(old_content);
        assert_eq!(old_hashes, anchors(&["Wot", "rKa", "BkM"]));
        let removed: FxHashSet<Anchor> = [old_hashes[1]].into_iter().collect();
        assert_eq!(
            map_stable_hashes(old_content, &old_hashes, "a\nb\nB2\nc\n", &removed)
                .expect("映射失败"),
            anchors(&["Wot", "rKa", "Bo2", "BkM"])
        );
    }

    #[test]
    fn nearest_new_prefers_the_left_candidate_on_ties() {
        assert_eq!(nearest_new(&[1, 3], 2), Some(0));
        assert_eq!(nearest_new(&[1, 3], 3), Some(1));
        assert_eq!(nearest_new(&[5], 0), Some(0));
        assert_eq!(nearest_new(&[], 0), None);
    }
}
