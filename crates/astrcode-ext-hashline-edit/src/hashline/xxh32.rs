//! xxHash32 的纯 Rust 实现。
//!
//! 原版 Pi 用 `xxhash-wasm`，DSH 移植版换成纯 JS，这里再换成纯 Rust。三者输出
//! 必须逐位一致：哈希值就是锚点地址，换实现等于换掉整套地址，跨版本的状态文件
//! 与提示词里的锚点都会失效。
//!
//! 参考向量见本文件的单元测试（`""`、`"a"`、`"abc"`）。

const P1: u32 = 2_654_435_761;
const P2: u32 = 2_246_822_519;
const P3: u32 = 3_266_489_917;
const P4: u32 = 668_265_263;
const P5: u32 = 374_761_393;

/// 内容校验和里第二个流用的种子。取自黄金比例常数，与原版一致。
const CHECKSUM_SEED: u32 = 0x9e37_79b1;

/// 计算 `input` 在给定种子下的 xxHash32。
pub fn xxh32(input: &[u8], seed: u32) -> u32 {
    let length = input.len();
    let mut index = 0usize;

    let mut hash = if length >= 16 {
        let mut v1 = seed.wrapping_add(P1).wrapping_add(P2);
        let mut v2 = seed.wrapping_add(P2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(P1);
        let last_block = length - 16;
        while index <= last_block {
            v1 = round(v1, read_u32(input, index));
            index += 4;
            v2 = round(v2, read_u32(input, index));
            index += 4;
            v3 = round(v3, read_u32(input, index));
            index += 4;
            v4 = round(v4, read_u32(input, index));
            index += 4;
        }
        v1.rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18))
    } else {
        seed.wrapping_add(P5)
    };

    hash = hash.wrapping_add(length as u32);
    while index + 4 <= length {
        hash = hash.wrapping_add(read_u32(input, index).wrapping_mul(P3));
        hash = hash.rotate_left(17).wrapping_mul(P4);
        index += 4;
    }
    while index < length {
        hash = hash.wrapping_add(u32::from(input[index]).wrapping_mul(P5));
        hash = hash.rotate_left(11).wrapping_mul(P1);
        index += 1;
    }

    hash ^= hash >> 15;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(P3);
    hash ^= hash >> 16;
    hash
}

/// 计算字符串的 xxHash32。
pub fn xxh32_str(input: &str, seed: u32) -> u32 {
    xxh32(input.as_bytes(), seed)
}

/// 64 位量级的内容校验和：两个不同种子的 xxh32 拼起来。
///
/// 只用于判断「快照缓存是否仍然对应当前文件内容」，不做安全用途。
pub fn content_checksum(content: &str) -> String {
    format!(
        "{}-{}",
        base36(xxh32_str(content, 0)),
        base36(xxh32_str(content, CHECKSUM_SEED))
    )
}

/// 按小端序读 4 字节。越界时按 0 补齐——调用点都保证 `index + 4 <= length`。
fn read_u32(input: &[u8], index: usize) -> u32 {
    u32::from_le_bytes([
        input[index],
        input[index + 1],
        input[index + 2],
        input[index + 3],
    ])
}

/// xxHash32 的轮函数。
fn round(accumulator: u32, input: u32) -> u32 {
    accumulator
        .wrapping_add(input.wrapping_mul(P2))
        .rotate_left(13)
        .wrapping_mul(P1)
}

/// 与 JS `Number.prototype.toString(36)` 一致的 36 进制渲染。
fn base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = Vec::with_capacity(7);
    while value > 0 {
        digits.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    digits.reverse();
    String::from_utf8(digits).expect("DIGITS 是 ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_vectors() {
        assert_eq!(xxh32(b"", 0), 0x02cc_5d05);
        assert_eq!(xxh32(b"a", 0), 0x550d_7456);
        assert_eq!(xxh32(b"abc", 0), 0x32d1_53ff);
    }

    /// 金标准：期望值由原版纯核心（`probe.js` 跑 `src/host.js`）实测得出。
    #[test]
    fn matches_the_original_implementation() {
        let cases: &[(&str, [u32; 3])] = &[
            ("", [46_947_589, 3_586_027_192, 917_998_311]),
            ("a", [1_426_945_110, 1_273_873_115, 2_652_255_204]),
            ("abc", [852_579_327, 21_474_302, 2_712_565_513]),
            ("hello world", [3_468_387_874, 4_225_033_588, 80_230_959]),
            ("错", [1_196_778_531, 523_209_961, 498_775_036]),
        ];
        for (input, expected) in cases {
            assert_eq!(
                [xxh32_str(input, 0), xxh32_str(input, 42), xxh32_str(input, CHECKSUM_SEED)],
                *expected,
                "输入 {input:?}"
            );
        }
        let long = "x".repeat(64);
        assert_eq!(
            [
                xxh32_str(&long, 0),
                xxh32_str(&long, 42),
                xxh32_str(&long, CHECKSUM_SEED)
            ],
            [4_129_382_658, 313_052_046, 3_124_815_223]
        );
    }

    #[test]
    fn seed_changes_the_digest() {
        assert_ne!(xxh32_str("same input", 0), xxh32_str("same input", 1));
    }

    #[test]
    fn is_deterministic_across_lengths() {
        for length in [0usize, 1, 4, 15, 16, 17, 31, 64, 1000] {
            let input = "x".repeat(length);
            assert_eq!(xxh32_str(&input, 7), xxh32_str(&input, 7));
        }
    }

    /// 16 字节块与尾部的边界必须覆盖到：长度 16 恰好走一次块循环，15 走纯尾部。
    #[test]
    fn block_and_tail_paths_are_both_exercised() {
        let fifteen = "0123456789abcde";
        let sixteen = "0123456789abcdef";
        assert_ne!(xxh32_str(fifteen, 0), xxh32_str(sixteen, 0));
        assert_eq!(xxh32_str(sixteen, 0), xxh32(b"0123456789abcdef", 0));
    }

    #[test]
    fn content_checksum_is_stable_and_content_sensitive() {
        let a = content_checksum("line one\nline two\n");
        let b = content_checksum("line one\nline two\n");
        let c = content_checksum("line one\nline three\n");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.contains('-'));
    }

    #[test]
    fn base36_matches_javascript_to_string_36() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        assert_eq!(base36(u32::MAX), "1z141z3");
    }
}
