//! 稳定 GUID 与牌组 id 派生。
//!
//! Anki 以 `notes.guid` 为合并键：同 guid 的笔记再导入时是**更新**，不同 guid 是**新建**。
//! 因此 GUID 必须跨多次生成保持稳定——这是「agent 反复跑同一个 vault，牌组不发散」的前提。
//!
//! 算法对齐 genanki 的 `guid_for`：SHA-256 前 8 字节按大端转成整数，再用 Anki 的
//! base91 表编码。输入由调用方决定：本扩展用「牌组名 + 卡片稳定 id」（vault 相对路径
//! # 标题），而不是 genanki 默认的字段内容，让 GUID 不随卡面文案微调而漂移。

use sha2::{Digest, Sha256};

/// Anki 内部使用的 base91 字符表（顺序有讲究，逐字对齐，不要重排）。
const BASE91_TABLE: &[u8; 91] = br#"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$%&()*+,-./:;<=>?@[]^_`{|}~"#;

/// 用各部分拼出稳定 GUID。空输入与全空部分是调用方的责任（校验层已拦下空 front）。
pub fn guid_for(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hasher.update(b"\x1f");
        }
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();

    // 取前 8 字节大端成 u64，再按 base91 展开；与 genanki 的逐字节累加等价。
    let mut hash_int: u64 = 0;
    for byte in &digest[..8] {
        hash_int = (hash_int << 8) | u64::from(*byte);
    }

    if hash_int == 0 {
        return String::from("a");
    }

    let mut reversed = Vec::with_capacity(11);
    let mut remaining = hash_int;
    while remaining > 0 {
        reversed.push(BASE91_TABLE[(remaining % 91) as usize]);
        remaining /= 91;
    }
    reversed.reverse();
    String::from_utf8(reversed).expect("base91 表全是 ASCII")
}

/// 由牌组名派生稳定的 deck id（i64，非零）。
///
/// deck id 稳定同样是导入合并的条件：同 id 同名 → 导入进既有牌组；随机 id 会让
/// Anki 每次导入都开新牌组。取 SHA-256 前 8 字节清掉符号位，零值折叠成 1（1 是
/// Anki 的 Default deck，躲开它）。
pub fn stable_deck_id(name: &str) -> i64 {
    let digest = Sha256::digest(name.as_bytes());
    let mut raw: u64 = 0;
    for byte in &digest[..8] {
        raw = (raw << 8) | u64::from(*byte);
    }
    let id = (raw & 0x7fff_ffff_ffff_ffff) as i64;
    if id == 0 { 1 } else { id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_matches_the_genanki_reference_vector() {
        // 金标准：用 Python 的 genanki 同款算法实算 guid_for("a", "b") = "OQo#R66FBz"
        // （sha256("a\x1fb") 前 8 字节 f04cdced9736a69d → base91）。钉死以防实现漂移。
        assert_eq!(guid_for(&["a", "b"]), "OQo#R66FBz");
    }

    #[test]
    fn guid_is_deterministic_and_order_sensitive() {
        assert_eq!(guid_for(&["Rust", "id:1"]), guid_for(&["Rust", "id:1"]));
        assert_ne!(guid_for(&["Rust", "id:1"]), guid_for(&["id:1", "Rust"]));
    }

    #[test]
    fn deck_id_is_stable_nonzero_and_nondefault() {
        let first = stable_deck_id("Rust::Ownership");
        assert_eq!(first, stable_deck_id("Rust::Ownership"));
        assert_ne!(first, 0);
        assert_ne!(first, 1);
    }

    #[test]
    fn deck_ids_of_different_names_differ() {
        assert_ne!(
            stable_deck_id("Rust::Ownership"),
            stable_deck_id("Rust::Lifetimes")
        );
    }
}
