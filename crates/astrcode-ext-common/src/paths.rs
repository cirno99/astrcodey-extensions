//! 插件自有数据的路径处理。
//!
//! 磁盘扩展拿不到进程内的 `ExtensionPaths`（那是 bundled 扩展的作者面），只能按
//! `astrcode_core::config::defaults` 的同一布局自行拼接路径。因此这里提供两个
//! 共用件：把任意标识符归一化成安全路径分量，以及稳定的 FNV-1a 哈希。

use std::path::{Path, PathBuf};

/// 把任意标识符归一化成单个安全的路径分量。
///
/// 宿主生成的 `session_id` / `tool_call_id` 本就不含路径分隔符，本函数是防御性的：
/// 一旦值被改写（含非法字符、为空、或全是点），就附加内容哈希后缀，既避免 `../`
/// 逃逸，也避免不同原值归一化后撞到同一个文件。
pub fn sanitize_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();

    let degenerate = cleaned.is_empty() || cleaned.chars().all(|character| character == '.');
    if degenerate {
        return format!("_{:016x}", fnv1a(value));
    }
    if cleaned == value {
        return cleaned;
    }
    format!("{cleaned}-{:016x}", fnv1a(value))
}

/// FNV-1a 64 位哈希：只用于生成稳定的路径后缀与匿名引用名，不是安全原语。
///
/// 不用 `DefaultHasher` 是因为它的算法不保证跨 Rust 版本稳定，而这里的哈希值会
/// 落进文件名，必须在重启、升级后仍然指向同一份数据。
pub fn fnv1a(value: &str) -> u64 {
    fnv1a_bytes(value.as_bytes())
}

/// [`fnv1a`] 的字节版本，供调用方直接哈希自己拥有的缓冲（如序列化后的消息字节），
/// 免去为了复用 `&str` 接口再复制一份。
pub fn fnv1a_bytes(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    bytes.iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

/// 按宿主布局拼接插件数据目录：`<base>/extension_data/<extension_id>`。
pub fn extension_data_dir(base: impl AsRef<Path>, extension_id: &str) -> PathBuf {
    base.as_ref()
        .join("extension_data")
        .join(sanitize_component(extension_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_ordinary_identifiers_verbatim() {
        assert_eq!(sanitize_component("s-1_abc.def"), "s-1_abc.def");
    }

    #[test]
    fn disambiguates_values_that_collide_after_cleaning() {
        let slash = sanitize_component("a/b");
        let underscore = sanitize_component("a_b");
        assert_ne!(slash, underscore);
        assert!(slash.starts_with("a_b-"));
        assert_eq!(underscore, "a_b");
    }

    #[test]
    fn handles_degenerate_values() {
        assert!(sanitize_component("").starts_with('_'));
        assert!(sanitize_component("..").starts_with('_'));
        assert!(sanitize_component("a b").starts_with("a_b-"));
    }

    #[test]
    fn neutralizes_path_traversal() {
        // `.` 本身是合法字符，所以结果里可能残留 `..`；关键是整串只有一个路径分量，
        // 没有分隔符就无法向上穿越。
        let escaped = sanitize_component("../../escape");
        assert!(!escaped.contains('/'));
        assert!(!escaped.contains('\\'));
        assert_eq!(std::path::Path::new(&escaped).components().count(), 1);
    }

    #[test]
    fn fnv1a_is_stable_for_a_fixed_input() {
        // 落进文件名的哈希必须在重启后仍然一致，因此这里把取值钉死。
        assert_eq!(fnv1a("call-1"), 0x9582_b899_5d76_b867);
    }

    #[test]
    fn the_byte_variant_matches_the_string_variant() {
        assert_eq!(fnv1a_bytes(b"call-1"), fnv1a("call-1"));
        assert_eq!(fnv1a_bytes(b""), fnv1a(""));
    }

    #[test]
    fn extension_data_dir_follows_the_host_layout() {
        assert_eq!(
            extension_data_dir("/home/u/.astrcode", "astrcode-hashline-edit"),
            PathBuf::from("/home/u/.astrcode/extension_data/astrcode-hashline-edit")
        );
    }
}
