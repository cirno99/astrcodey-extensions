//! 被 offload 的工具输出原文落盘与读取。
//!
//! # 目录布局
//!
//! ```text
//! ~/.astrcode/extension_data/astrcode-context-offload/
//!   sessions/<session>/offload/<ref>.txt
//! ```
//!
//! S5R 插件运行在独立进程里，拿不到进程内 `ExtensionPaths`（那是 bundled 扩展的
//! 作者面），因此按 `astrcode_core::config::defaults::extension_data_dir` 的同一
//! 布局自行拼接。`session` 与 `ref` 都经过 [`sanitize_component`]，被改写过的值
//! 会附加内容哈希后缀，既不会逃出该目录，也不会因归一化而互相覆盖。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use astrcode_ext_common::paths::sanitize_component;
use astrcode_extension_sdk::hostpaths;

const SESSIONS_DIR: &str = "sessions";
const OFFLOAD_DIR: &str = "offload";
const CONTENT_SUFFIX: &str = ".txt";

/// 单个 offload 条目的元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffloadEntry {
    /// 占位符里展示给模型的引用名。
    pub reference: String,
    /// 原文字节数。
    pub bytes: u64,
}

/// 一个会话的 offload 原文仓库。
#[derive(Debug, Clone)]
pub struct OffloadStore {
    dir: PathBuf,
}

impl OffloadStore {
    /// 定位某个会话的仓库目录（宿主数据目录下）。
    pub fn for_session(extension_id: &str, session_id: &str) -> Self {
        Self::under(hostpaths::astrcode_dir(), extension_id, session_id)
    }

    /// 在显式根目录下定位会话仓库；测试与工具函数用它避开真实用户目录。
    pub fn under(base: impl AsRef<Path>, extension_id: &str, session_id: &str) -> Self {
        let dir = base
            .as_ref()
            .join("extension_data")
            .join(sanitize_component(extension_id))
            .join(SESSIONS_DIR)
            .join(sanitize_component(session_id))
            .join(OFFLOAD_DIR);
        Self { dir }
    }

    /// 直接指定仓库目录。
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { dir: root.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 写入原文；同名条目直接覆盖（ref 由 tool_call_id 派生，本身唯一）。
    pub fn write(&self, reference: &str, content: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        hostpaths::write_file_atomic(&self.path_for(reference), content)
    }

    /// 读取原文；条目不存在时返回 `None`，与「读失败」区分开。
    pub fn read(&self, reference: &str) -> io::Result<Option<String>> {
        match fs::read_to_string(self.path_for(reference)) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// 列出全部条目，按引用名排序；目录不存在时返回空列表。
    pub fn entries(&self) -> io::Result<Vec<OffloadEntry>> {
        let dir = match fs::read_dir(&self.dir) {
            Ok(dir) => dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };

        let mut entries = Vec::new();
        for entry in dir {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(reference) = name.strip_suffix(CONTENT_SUFFIX) else {
                continue;
            };
            entries.push(OffloadEntry {
                reference: reference.to_string(),
                bytes: entry.metadata()?.len(),
            });
        }
        entries.sort_by(|left, right| left.reference.cmp(&right.reference));
        Ok(entries)
    }

    fn path_for(&self, reference: &str) -> PathBuf {
        self.dir
            .join(format!("{}{CONTENT_SUFFIX}", sanitize_component(reference)))
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn temp_store(name: &str) -> OffloadStore {
        let root = std::env::temp_dir().join(format!(
            "astrcode-offload-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        OffloadStore::at(root)
    }

    #[test]
    fn round_trips_content_by_reference() {
        let store = temp_store("round-trip");
        store.write("call-1", "full output").unwrap();
        assert_eq!(
            store.read("call-1").unwrap().as_deref(),
            Some("full output")
        );
        let _ = fs::remove_dir_all(store.dir());
    }

    #[test]
    fn missing_reference_is_none_not_an_error() {
        let store = temp_store("missing");
        assert_eq!(store.read("never-written").unwrap(), None);
    }

    #[test]
    fn entries_report_references_and_sizes() {
        let store = temp_store("entries");
        store.write("call-a", "12345").unwrap();
        store.write("call-b", "abc").unwrap();
        assert_eq!(
            store.entries().unwrap(),
            vec![
                OffloadEntry {
                    reference: "call-a".into(),
                    bytes: 5
                },
                OffloadEntry {
                    reference: "call-b".into(),
                    bytes: 3
                },
            ]
        );
        let _ = fs::remove_dir_all(store.dir());
    }

    #[test]
    fn entries_on_missing_directory_is_empty() {
        let store = temp_store("entries-missing");
        assert!(store.entries().unwrap().is_empty());
    }

    #[test]
    fn path_traversal_is_neutralized() {
        let store = temp_store("traversal");
        store.write("../../escape", "payload").unwrap();
        let written = store.path_for("../../escape");
        assert!(
            written.starts_with(store.dir()),
            "sanitized path escaped the store: {written:?}"
        );
        assert_eq!(
            store.read("../../escape").unwrap().as_deref(),
            Some("payload")
        );
        let _ = fs::remove_dir_all(store.dir());
    }

}
