//! 文件读写：内容嗅探、BOM/行尾保真、原子写。
//!
//! 与原版的差异只有一处：原版走 DSH 的 `fs` 服务（异步 + 沙箱策略），这里直接用
//! `std::fs`。worker 是独立进程，本就拥有完整文件系统访问权；宿主没有提供「按行
//! 保真读写 + 原子替换」的等价能力（`workspace.read` 有 10MiB/1MiB/100k 行上限，
//! 且只返回 `String`，会把有效文件规模压到 ~1MiB）。
//!
//! 路径解析用宿主给的 `working_dir`；planner 仍会声明 `ResourceAccess::read_write_file`，
//! 让宿主的权限系统看得见这次访问。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use astrcode_extension_sdk::hostpaths;

use crate::hashline::{
    error::{EditError, ErrorCode},
    lines::{Ending, detect_ending, strip_bom, to_lf},
};

/// 单文件大小上限。与原版一致。
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// 嗅探二进制/图片时读取的头部字节数。与原版一致。
const SNIFF_BYTES: usize = 8192;

/// 一次读取的结果，连同保真所需的编码事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFile {
    /// BOM 仍在的原始文本（UTF-8 有损解码）。
    pub raw: String,
    /// 原文件的 BOM：`""` 或 `"\u{feff}"`。写回时必须原样带上。
    pub bom: String,
    /// 原文件的行尾风格。
    pub ending: Ending,
    /// 去掉 BOM、换行归一成 LF 之后的内容。
    pub normalized: String,
    /// 归一化后的内容里出现了 U+FFFD（说明原文有非 UTF-8 字节）。
    pub had_utf8_errors: bool,
}

/// 解析一个用户给的路径：相对路径落在 `working_dir` 下。
pub fn resolve_path(working_dir: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        working_dir.join(candidate)
    }
}

/// 读取并嗅探一个文件，返回归一化后的内容。
///
/// `display_path` 只用于错误正文，保持与模型输入的路径一致。
pub fn read_raw_text(path: &Path, display_path: &str) -> Result<RawFile, EditError> {
    let metadata = fs::metadata(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => EditError::plain(format!("cannot read \"{display_path}\": not found")),
        _ => EditError::plain(format!("cannot read \"{display_path}\": {error}")),
    })?;
    if !metadata.is_file() {
        return Err(EditError::plain(format!(
            "cannot read \"{display_path}\": not a regular file"
        )));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(EditError::new(
            ErrorCode::FileTooLarge,
            format!(
                "{display_path} exceeds the {}MB size limit. Hashline editing targets \
                 source-sized files; for very large files use write or a non-line-based approach.",
                MAX_FILE_BYTES / (1024 * 1024)
            ),
        ));
    }

    let bytes = fs::read(path)
        .map_err(|error| EditError::plain(format!("cannot read \"{display_path}\": {error}")))?;

    let truncated = bytes.len() > SNIFF_BYTES;
    let sample = &bytes[..bytes.len().min(SNIFF_BYTES)];
    match sniff(sample, truncated) {
        Sniffed::Image(mime_type) => {
            return Err(EditError::new(
                ErrorCode::Image,
                format!(
                    "{display_path} is a {mime_type} image. Use the built-in read_image tool for \
                     images; hashline anchors address text lines."
                ),
            ));
        },
        Sniffed::Binary(description) => {
            return Err(EditError::new(
                ErrorCode::Binary,
                format!(
                    "{display_path} looks like {description}. Hashline editing targets UTF-8 text \
                     files; use the built-in read/write tools for binary handling."
                ),
            ));
        },
        Sniffed::Text => {},
    }

    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let (bom, text) = strip_bom(&raw);
    let bom = bom.to_owned();
    let ending = detect_ending(text);
    let normalized = to_lf(text);
    let had_utf8_errors = normalized.contains('\u{fffd}');
    Ok(RawFile {
        raw,
        bom,
        ending,
        normalized,
        had_utf8_errors,
    })
}

/// 原子写入。走宿主的 `hostpaths`，与 bundled 扩展写状态文件时同一实现。
pub fn write_text(path: &Path, content: &str) -> io::Result<()> {
    hostpaths::write_file_atomic(path, content)
}

/// 嗅探结果。
enum Sniffed {
    Text,
    Image(&'static str),
    Binary(String),
}

/// 判断头部字节是什么类型的文件。
fn sniff(sample: &[u8], truncated: bool) -> Sniffed {
    if let Some(encoding) = detect_text_bom(sample) {
        return Sniffed::Binary(format!("{encoding} encoded text"));
    }
    if let Some(mime_type) = detect_image_magic(sample) {
        return Sniffed::Image(mime_type);
    }
    if sample.contains(&0) {
        return Sniffed::Binary("binary file (NUL bytes)".to_owned());
    }
    let candidate = if truncated {
        trim_incomplete_utf8(sample)
    } else {
        sample
    };
    if std::str::from_utf8(candidate).is_err() {
        return Sniffed::Binary("non-UTF-8 binary data".to_owned());
    }
    Sniffed::Text
}

/// 去掉被采样点切断的尾部多字节序列。
///
/// 原版直接对 8192 字节的样本做严格 UTF-8 校验，样本边界恰好切开一个多字节字符时
/// 会把合法 UTF-8 文件误判成二进制。这里只在「整段校验失败」时才尝试去掉末尾至多
/// 3 个字节：能通过就说明失败原因是截断，而不是内容不是 UTF-8。
fn trim_incomplete_utf8(sample: &[u8]) -> &[u8] {
    if std::str::from_utf8(sample).is_ok() {
        return sample;
    }
    for back in 1..=3usize {
        if sample.len() <= back {
            break;
        }
        let candidate = &sample[..sample.len() - back];
        if std::str::from_utf8(candidate).is_ok() {
            return candidate;
        }
    }
    sample
}

/// 文本 BOM 探测（UTF-16/32）。UTF-8 BOM 不算二进制，它由 [`strip_bom`] 处理。
fn detect_text_bom(sample: &[u8]) -> Option<&'static str> {
    if sample.len() >= 4 && sample[0] == 0xff && sample[1] == 0xfe && sample[2] == 0x00 && sample[3] == 0x00
    {
        return Some("UTF-32LE");
    }
    if sample.len() >= 4 && sample[0] == 0x00 && sample[1] == 0x00 && sample[2] == 0xfe && sample[3] == 0xff
    {
        return Some("UTF-32BE");
    }
    if sample.len() >= 2 && sample[0] == 0xff && sample[1] == 0xfe {
        return Some("UTF-16LE");
    }
    if sample.len() >= 2 && sample[0] == 0xfe && sample[1] == 0xff {
        return Some("UTF-16BE");
    }
    None
}

/// 图片魔数探测。
fn detect_image_magic(sample: &[u8]) -> Option<&'static str> {
    let len = sample.len();
    if len >= 3 && sample[0] == 0xff && sample[1] == 0xd8 && sample[2] == 0xff {
        return Some("image/jpeg");
    }
    if len >= 8
        && sample[0] == 0x89
        && sample[1] == 0x50
        && sample[2] == 0x4e
        && sample[3] == 0x47
        && sample[4] == 0x0d
        && sample[5] == 0x0a
        && sample[6] == 0x1a
        && sample[7] == 0x0a
    {
        return Some("image/png");
    }
    if len >= 6
        && sample[0] == 0x47
        && sample[1] == 0x49
        && sample[2] == 0x46
        && sample[3] == 0x38
        && (sample[4] == 0x37 || sample[4] == 0x39)
        && sample[5] == 0x61
    {
        return Some("image/gif");
    }
    if len >= 2 && sample[0] == 0x42 && sample[1] == 0x4d {
        return Some("image/bmp");
    }
    if len >= 12
        && sample[0] == 0x52
        && sample[1] == 0x49
        && sample[2] == 0x46
        && sample[3] == 0x46
        && sample[8] == 0x57
        && sample[9] == 0x45
        && sample[10] == 0x42
        && sample[11] == 0x50
    {
        return Some("image/webp");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-hashline-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("创建临时目录失败");
        dir
    }

    #[test]
    fn resolve_path_keeps_absolute_paths_and_joins_relative_ones() {
        let base = Path::new("/workspace");
        assert_eq!(resolve_path(base, "src/lib.rs"), Path::new("/workspace/src/lib.rs"));
        assert_eq!(resolve_path(base, "/etc/hosts"), Path::new("/etc/hosts"));
    }

    #[test]
    fn reads_lf_content_and_preserves_ending() {
        let dir = temp_dir("lf");
        let path = dir.join("a.txt");
        fs::write(&path, "one\ntwo\n").expect("写入失败");
        let file = read_raw_text(&path, "a.txt").expect("读取失败");
        assert_eq!(file.normalized, "one\ntwo\n");
        assert_eq!(file.ending, Ending::Lf);
        assert!(!file.had_utf8_errors);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_crlf_and_bom_and_normalizes_both() {
        let dir = temp_dir("crlf-bom");
        let path = dir.join("a.txt");
        std::fs::write(&path, "\u{feff}one\r\ntwo\r\n").expect("写入失败");
        let file = read_raw_text(&path, "a.txt").expect("读取失败");
        assert_eq!(file.ending, Ending::Crlf);
        assert_eq!(file.normalized, "one\ntwo\n");
        assert_eq!(file.bom, "\u{feff}");
        assert!(file.raw.starts_with('\u{feff}'));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_without_a_bom_reports_an_empty_marker() {
        let dir = temp_dir("no-bom");
        let path = dir.join("a.txt");
        fs::write(&path, "one\n").expect("写入失败");
        let file = read_raw_text(&path, "a.txt").expect("读取失败");
        assert_eq!(file.bom, "");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_a_missing_file_without_an_error_marker() {
        let error = read_raw_text(Path::new("/definitely/not/here"), "nope.txt")
            .expect_err("应当报文件不存在");
        assert_eq!(error.render(), "cannot read \"nope.txt\": not found");
    }

    #[test]
    fn rejects_a_directory() {
        let dir = temp_dir("dir");
        let error = read_raw_text(&dir, "somedir").expect_err("应当报不是普通文件");
        assert!(error.render().contains("not a regular file"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_images_and_binary_and_utf16() {
        let dir = temp_dir("sniff");

        let png = dir.join("a.png");
        fs::write(&png, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00]).expect("写入失败");
        let error = read_raw_text(&png, "a.png").expect_err("应当拒绝图片");
        assert_eq!(error.code(), ErrorCode::Image);
        assert!(error.render().contains("image/png"));

        let binary = dir.join("a.bin");
        fs::write(&binary, [0x00, 0x01, 0x02]).expect("写入失败");
        let error = read_raw_text(&binary, "a.bin").expect_err("应当拒绝二进制");
        assert_eq!(error.code(), ErrorCode::Binary);
        assert!(error.render().contains("NUL bytes"));

        let utf16 = dir.join("a.txt");
        fs::write(&utf16, [0xff, 0xfe, 0x61, 0x00]).expect("写入失败");
        let error = read_raw_text(&utf16, "a.txt").expect_err("应当拒绝 UTF-16");
        assert!(error.render().contains("UTF-16LE encoded text"));

        let _ = fs::remove_dir_all(&dir);
    }

    /// 采样点切开多字节字符时不能把合法 UTF-8 文件误判成二进制。
    #[test]
    fn a_multibyte_char_split_by_the_sample_boundary_is_not_binary() {
        let dir = temp_dir("boundary");
        let path = dir.join("a.txt");
        // 8191 个 ASCII + 一个 3 字节汉字，采样正好切在汉字中间
        let content = format!("{}\n错\n", "x".repeat(SNIFF_BYTES - 2));
        fs::write(&path, &content).expect("写入失败");
        let file = read_raw_text(&path, "a.txt").expect("合法 UTF-8 不应被拒绝");
        assert!(file.normalized.contains("错"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_with_a_genuine_invalid_byte_is_still_binary() {
        let dir = temp_dir("invalid");
        let path = dir.join("a.bin");
        let mut bytes = b"hello ".to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b" world");
        fs::write(&path, &bytes).expect("写入失败");
        let error = read_raw_text(&path, "a.bin").expect_err("应当拒绝非 UTF-8");
        assert!(error.render().contains("non-UTF-8 binary data"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_text_replaces_content_atomically() {
        let dir = temp_dir("write");
        let path = dir.join("a.txt");
        write_text(&path, "first").expect("写入失败");
        write_text(&path, "second").expect("写入失败");
        assert_eq!(fs::read_to_string(&path).expect("读取失败"), "second");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sniff_detects_every_supported_image_magic() {
        assert_eq!(detect_image_magic(&[0xff, 0xd8, 0xff]), Some("image/jpeg"));
        assert_eq!(detect_image_magic(b"GIF89a"), Some("image/gif"));
        assert_eq!(detect_image_magic(b"BM"), Some("image/bmp"));
        assert_eq!(
            detect_image_magic(b"RIFF\x00\x00\x00\x00WEBP"),
            Some("image/webp")
        );
        assert_eq!(detect_image_magic(b"plain text"), None);
    }
}
