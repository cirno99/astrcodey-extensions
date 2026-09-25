//! 每个会话的持久状态：锚点快照、served 集合、undo 记录。
//!
//! 三份状态都按**文件路径**（`displayPath`）为键：
//!
//! - `snapshots` —— 内容校验和 → 锚点数组。命中就不必重算 238328 位位图，链式编辑
//!   才够快。上限 256 条，超出按插入顺序淘汰最旧一条。
//! - `served` —— 该文件里**展示给过模型**的锚点。served 守卫靠它拦住凭空编造的地址。
//! - `undo` —— 每个文件最后一次 `replace` 的还原点，重启后仍可回滚。
//!
//! # 与原版的一处架构差异
//!
//! 原版把状态挂在插件实例上，状态文件落在**首个会话的 cwd**；S5R worker 是跨会话
//! 共享的单进程，那样做会让两个会话互相覆盖。这里按 `session_id` 分桶，各自写
//! `~/.astrcode/extension_data/astrcode-hashline-edit/sessions/<sid>/state.json`。

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use astrcode_ext_common::paths::{extension_data_dir, sanitize_component};
use astrcode_extension_sdk::hostpaths;
use serde::{Deserialize, Serialize};

use crate::hashline::{
    HASH_LEN, HASH_SEP,
    error::EditError,
    hash::line_hashes_pure,
    lines::{Ending, split_lines},
    xxh32::content_checksum,
};

/// 状态文件所在子目录。
const SESSIONS_DIR: &str = "sessions";
/// 状态文件名。
const STATE_FILE: &str = "state.json";
/// 快照缓存条数上限（按路径）。
const SNAPSHOT_CAP: usize = 256;
/// 状态文件格式版本。
const STATE_VERSION: u32 = 1;

/// 某个会话的状态文件位置。
pub fn state_path(extension_id: &str, session_id: &str) -> PathBuf {
    extension_data_dir(hostpaths::astrcode_dir(), extension_id)
        .join(SESSIONS_DIR)
        .join(sanitize_component(session_id))
        .join(STATE_FILE)
}

/// 一份文件的锚点快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub checksum: String,
    #[serde(rename = "lineCount")]
    pub line_count: usize,
    pub hashes: Vec<String>,
}

/// 一次 `replace` 的还原点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoRecord {
    /// 编辑前的内容（LF 归一、不含 BOM）。
    pub content: String,
    /// 原文件的 BOM（`""` 或 `"\u{feff}"`）。
    #[serde(default)]
    pub bom: String,
    /// 原文件的行尾，取值为字面量 `"\n"` / `"\r\n"` / `"\r"`。
    #[serde(default = "default_ending_literal")]
    pub ending: String,
    /// 编辑前内容的锚点。
    pub hashes: Vec<String>,
    /// 编辑后写入文件的内容（LF 归一、不含 BOM），用于判断文件是否被外部改过。
    #[serde(rename = "resultContent")]
    pub result_content: String,
}

impl UndoRecord {
    /// 还原点记录的行尾风格。
    pub fn ending_kind(&self) -> Ending {
        Ending::parse(&self.ending)
    }
}

fn default_ending_literal() -> String {
    "\n".to_owned()
}

/// 状态文件的线缆形状。键名沿用原版，便于两边对照。
#[derive(Debug, Serialize, Deserialize)]
struct StateFile {
    version: u32,
    /// 快照的插入顺序，淘汰最旧一条时用。
    #[serde(default)]
    order: Vec<String>,
    #[serde(default)]
    snapshots: BTreeMap<String, Snapshot>,
    #[serde(default)]
    served: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    undo: BTreeMap<String, UndoRecord>,
}

/// 一个会话的可变状态。
#[derive(Debug)]
pub struct SessionState {
    path: PathBuf,
    snapshots: BTreeMap<String, Snapshot>,
    snapshot_order: VecDeque<String>,
    served: BTreeMap<String, BTreeSet<String>>,
    undo: BTreeMap<String, UndoRecord>,
    dirty: bool,
}

impl SessionState {
    /// 从磁盘加载。文件不存在或损坏时以空状态开始——状态只是缓存与撤销历史，
    /// 损坏不应该让编辑功能整体不可用（与原版一致）。
    pub fn load(path: PathBuf) -> Self {
        let mut state = Self {
            path,
            snapshots: BTreeMap::new(),
            snapshot_order: VecDeque::new(),
            served: BTreeMap::new(),
            undo: BTreeMap::new(),
            dirty: false,
        };

        let Ok(raw) = std::fs::read(&state.path) else {
            return state;
        };
        // 交给 simd-json 原地解析：它要可变输入，直接吃掉刚读进来的缓冲，不再复制一份。
        let Ok(file) = astrcode_ext_common::json::parse_owned::<StateFile>(raw) else {
            return state;
        };
        if file.version != STATE_VERSION {
            return state;
        }

        state.snapshot_order = if file.order.is_empty() {
            file.snapshots.keys().cloned().collect()
        } else {
            file.order
                .into_iter()
                .filter(|key| file.snapshots.contains_key(key))
                .collect()
        };
        state.snapshots = file.snapshots;
        state.served = file.served;
        state.undo = file.undo;
        state
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 取某个文件的锚点：命中快照缓存就直接用，否则重算并写回缓存。
    pub fn hashes_for(&mut self, key: &str, content: &str) -> Result<Vec<String>, EditError> {
        let checksum = content_checksum(content);
        let line_count = split_lines(content).len();
        if let Some(snapshot) = self.snapshots.get(key)
            && snapshot.checksum == checksum
            && snapshot.line_count == line_count
        {
            return Ok(snapshot.hashes.clone());
        }
        let hashes = line_hashes_pure(content)?;
        self.put_snapshot(key, content, hashes.clone());
        Ok(hashes)
    }

    /// 覆盖某个文件的锚点快照。
    pub fn put_snapshot(&mut self, key: &str, content: &str, hashes: Vec<String>) {
        if !self.snapshots.contains_key(key) {
            self.snapshot_order.push_back(key.to_owned());
        }
        self.snapshots.insert(
            key.to_owned(),
            Snapshot {
                checksum: content_checksum(content),
                line_count: split_lines(content).len(),
                hashes,
            },
        );
        while self.snapshot_order.len() > SNAPSHOT_CAP {
            if let Some(oldest) = self.snapshot_order.pop_front() {
                self.snapshots.remove(&oldest);
            }
        }
        self.dirty = true;
    }

    /// 某个文件已经展示给模型的锚点集合。
    pub fn served_set(&self, key: &str) -> Option<&BTreeSet<String>> {
        self.served.get(key)
    }

    /// 记下「这些锚点已经展示给模型了」。
    pub fn record_served(&mut self, key: &str, hashes: &[String]) {
        if hashes.is_empty() {
            return;
        }
        let entry = self.served.entry(key.to_owned()).or_default();
        let before = entry.len();
        entry.extend(hashes.iter().cloned());
        if entry.len() != before {
            self.dirty = true;
        }
    }

    /// 从 diff 正文里抽出已经展示过的锚点。
    ///
    /// diff 的上下文行（` `）与新增行（`+`）都带锚点，它们就是模型「见过」的行；
    /// 删除行（`-`）的锚点已失效，不计入。
    pub fn record_served_from_diff(&mut self, key: &str, diff: &str) {
        let hashes = served_hashes_in_diff(diff);
        self.record_served(key, &hashes);
    }

    /// 某个文件的还原点。
    pub fn undo_record(&self, key: &str) -> Option<&UndoRecord> {
        self.undo.get(key)
    }

    /// 覆盖某个文件的还原点，返回被替换掉的上一条（写入失败时用于回滚）。
    pub fn set_undo(&mut self, key: &str, record: UndoRecord) -> Option<UndoRecord> {
        self.dirty = true;
        self.undo.insert(key.to_owned(), record)
    }

    /// 丢弃某个文件的还原点。
    pub fn clear_undo(&mut self, key: &str) -> Option<UndoRecord> {
        let removed = self.undo.remove(key);
        if removed.is_some() {
            self.dirty = true;
        }
        removed
    }

    /// 把状态写回磁盘。无改动时是空操作。
    pub fn save(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let file = StateFile {
            version: STATE_VERSION,
            order: self.snapshot_order.iter().cloned().collect(),
            snapshots: self.snapshots.clone(),
            served: self.served.clone(),
            undo: self.undo.clone(),
        };
        let payload = astrcode_ext_common::json::to_string(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        match hostpaths::write_file_atomic(&self.path, &payload) {
            Ok(()) => Ok(()),
            Err(error) => {
                // 写失败时保留脏标记，下一次工具调用还会再试。
                self.dirty = true;
                Err(error)
            },
        }
    }
}

/// 从 diff 正文里抽出 `+`/` ` 行的锚点。
fn served_hashes_in_diff(diff: &str) -> Vec<String> {
    let prefix_len = HASH_LEN + HASH_SEP.len_utf8();
    diff.split('\n')
        .filter_map(|line| {
            let rest = match line.as_bytes().first() {
                Some(b'+') | Some(b' ') => &line[1..],
                _ => return None,
            };
            if rest.len() < prefix_len {
                return None;
            }
            if !rest.as_bytes()[..HASH_LEN].iter().all(u8::is_ascii_alphanumeric) {
                return None;
            }
            rest[HASH_LEN..]
                .starts_with(HASH_SEP)
                .then(|| rest[..HASH_LEN].to_owned())
        })
        .collect()
}

/// 按会话隔离的状态注册表。
///
/// 同一个 worker 进程服务所有会话，因此不能把状态放在「插件实例」这一层。
#[derive(Debug, Clone, Default)]
pub struct StateRegistry {
    sessions: Arc<Mutex<BTreeMap<String, Arc<Mutex<SessionState>>>>>,
}

impl StateRegistry {
    /// 取某个会话的状态，第一次访问时从磁盘加载。
    pub fn session(&self, extension_id: &str, session_id: &str) -> Arc<Mutex<SessionState>> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(sessions.entry(session_id.to_owned()).or_insert_with(|| {
            Arc::new(Mutex::new(SessionState::load(state_path(
                extension_id,
                session_id,
            ))))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashline::hash::line_hashes_pure;

    fn temp_state(name: &str) -> SessionState {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-hashline-state-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        SessionState::load(dir.join(STATE_FILE))
    }

    #[test]
    fn hashes_for_caches_by_checksum() {
        let mut state = temp_state("cache");
        let content = "one\ntwo\n";
        let first = state.hashes_for("a.txt", content).expect("计算失败");
        assert!(state.snapshots.contains_key("a.txt"));
        let second = state.hashes_for("a.txt", content).expect("计算失败");
        assert_eq!(first, second);

        // 内容变了就重算
        let changed = state.hashes_for("a.txt", "one\nTWO\n").expect("计算失败");
        assert_ne!(first, changed);
    }

    #[test]
    fn snapshots_are_evicted_in_insertion_order_past_the_cap() {
        let mut state = temp_state("evict");
        for index in 0..SNAPSHOT_CAP + 5 {
            state.put_snapshot(&format!("file{index}.txt"), "x\n", vec!["aB3".into()]);
        }
        assert_eq!(state.snapshots.len(), SNAPSHOT_CAP);
        assert!(!state.snapshots.contains_key("file0.txt"));
        assert!(state.snapshots.contains_key(&format!("file{}.txt", SNAPSHOT_CAP + 4)));
        // 顺序表也同步收缩，不会无界增长
        assert_eq!(state.snapshot_order.len(), SNAPSHOT_CAP);
    }

    #[test]
    fn re_putting_an_existing_key_does_not_reset_its_age() {
        let mut state = temp_state("age");
        state.put_snapshot("first.txt", "x\n", vec!["aB3".into()]);
        for index in 0..SNAPSHOT_CAP - 1 {
            state.put_snapshot(&format!("f{index}.txt"), "x\n", vec!["aB3".into()]);
        }
        // 再写一次最早的键：原版 Map.set 不改变已有键的位置，这里保持一致
        state.put_snapshot("first.txt", "y\n", vec!["cD4".into()]);
        assert_eq!(state.snapshot_order.front().map(String::as_str), Some("first.txt"));
    }

    #[test]
    fn record_served_accumulates_and_dedupes() {
        let mut state = temp_state("served");
        state.record_served("a.txt", &["aB3".into(), "cD4".into()]);
        state.record_served("a.txt", &["cD4".into(), "eF5".into()]);
        let served = state.served_set("a.txt").expect("应当有记录");
        assert_eq!(served.len(), 3);
        assert!(served.contains("eF5"));
        // 空输入不改动任何东西
        state.record_served("b.txt", &[]);
        assert!(state.served_set("b.txt").is_none());
    }

    #[test]
    fn served_hashes_in_diff_skips_removed_rows() {
        let diff = format!(
            " {}{HASH_SEP}context\n-{}{HASH_SEP}gone\n+{}{HASH_SEP}added\n ...",
            "aB3", "cD4", "eF5"
        );
        assert_eq!(served_hashes_in_diff(&diff), ["aB3", "eF5"]);
    }

    #[test]
    fn served_hashes_in_diff_ignores_unmarked_or_malformed_rows() {
        assert!(served_hashes_in_diff("plain line").is_empty());
        assert!(served_hashes_in_diff("+ab│short").is_empty());
        assert!(served_hashes_in_diff("+aB3-no-separator").is_empty());
        assert!(served_hashes_in_diff("").is_empty());
    }

    #[test]
    fn state_round_trips_through_disk() {
        let mut state = temp_state("round-trip");
        let content = "one\ntwo\n";
        let hashes = state.hashes_for("a.txt", content).expect("计算失败");
        state.record_served("a.txt", &hashes);
        state.set_undo(
            "a.txt",
            UndoRecord {
                content: content.to_owned(),
                bom: "\u{feff}".to_owned(),
                ending: "\r\n".to_owned(),
                hashes: hashes.clone(),
                result_content: "one\nTWO\n".to_owned(),
            },
        );
        state.save().expect("写入失败");

        let reloaded = SessionState::load(state.path().clone());
        assert_eq!(reloaded.snapshots.get("a.txt").map(|s| &s.hashes), Some(&hashes));
        assert_eq!(reloaded.served_set("a.txt").map(BTreeSet::len), Some(hashes.len()));
        let undo = reloaded.undo_record("a.txt").expect("还原点应当被持久化");
        assert_eq!(undo.ending_kind(), Ending::Crlf);
        assert_eq!(undo.bom, "\u{feff}");
        let _ = std::fs::remove_dir_all(state.path().parent().expect("有父目录"));
    }

    #[test]
    fn a_corrupt_state_file_starts_from_empty_instead_of_failing() {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-hashline-corrupt-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let path = dir.join(STATE_FILE);
        std::fs::write(&path, b"{not json").expect("写入失败");
        let mut state = SessionState::load(path);
        assert!(state.hashes_for("a.txt", "x\n").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_state_file_from_a_future_version_is_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-hashline-version-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let path = dir.join(STATE_FILE);
        std::fs::write(
            &path,
            br#"{"version":99,"snapshots":{"a.txt":{"checksum":"x","lineCount":1,"hashes":["aB3"]}}}"#,
        )
        .expect("写入失败");
        let state = SessionState::load(path);
        assert!(state.snapshots.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_a_no_op_without_changes() {
        let mut state = temp_state("no-op");
        state.save().expect("保存失败");
        assert!(!state.path().exists());
        state.hashes_for("a.txt", "x\n").expect("计算失败");
        state.save().expect("保存失败");
        assert!(state.path().exists());
        let _ = std::fs::remove_dir_all(state.path().parent().expect("有父目录"));
    }

    #[test]
    fn registry_returns_the_same_state_per_session() {
        let registry = StateRegistry::default();
        let first = registry.session("ext", "s-1");
        let second = registry.session("ext", "s-1");
        assert!(Arc::ptr_eq(&first, &second));
        let other = registry.session("ext", "s-2");
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn put_snapshot_keeps_the_hashes_aligned_with_the_content() {
        let mut state = temp_state("align");
        let content = "a\nb\nc\n";
        let hashes = line_hashes_pure(content).expect("分配失败");
        state.put_snapshot("a.txt", content, hashes.clone());
        assert_eq!(state.snapshots["a.txt"].line_count, 3);
        assert_eq!(state.hashes_for("a.txt", content).expect("计算失败"), hashes);
    }
}
