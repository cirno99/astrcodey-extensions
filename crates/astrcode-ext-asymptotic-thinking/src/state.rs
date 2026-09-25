//! 会话状态与开关的持久化层。
//!
//! 上游用一张 SQLite 全局库（`~/.pi/agent/extension-global.db`）存两张表：
//! `asymptotic_thinking_session_state`（append-only 的状态快照）与
//! `asymptotic_thinking_toggle`（按会话的开关）。移植版换成**按会话一份 JSON 文件**，
//! 状态与开关同放一处：
//!
//! ```text
//! ~/.astrcode/extension_data/astrcode-asymptotic-thinking/sessions/<sid>/state.json
//! ```
//!
//! 不用宿主的 `astrcode.session.state`：它的单值上限是 1 MiB，且每次读写都要走一次
//! IPC，而 `before_provider_request` 每个 LLM 请求都会触发。
//!
//! # 为什么按会话分桶
//!
//! S5R worker 是**跨会话共享的单进程**，把状态挂在「插件实例」上会让两个会话互相覆盖。
//! 因此用 [`StoreRegistry`] 按 `session_id` 分桶，各自读写自己的文件。

use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use astrcode_ext_common::paths::{extension_data_dir, sanitize_component};
use astrcode_extension_sdk::hostpaths;
use serde::{Deserialize, Serialize};

use crate::types::SessionState;

/// 状态文件所在子目录。
const SESSIONS_DIR: &str = "sessions";
/// 状态文件名。
const STATE_FILE: &str = "state.json";
/// 状态文件格式版本。版本不匹配时整份丢弃、按默认值重来。
const STATE_VERSION: u32 = 1;

/// 某个会话的状态文件位置。
pub fn state_path(extension_id: &str, session_id: &str) -> PathBuf {
    extension_data_dir(hostpaths::astrcode_dir(), extension_id)
        .join(SESSIONS_DIR)
        .join(sanitize_component(session_id))
        .join(STATE_FILE)
}

/// 状态文件的线缆形状。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    version: u32,
    /// `/asymptotic-toggle` 的开关。缺省启用（与上游 `isEnabled` 无记录时返回 true 一致）。
    #[serde(default = "default_true")]
    enabled: bool,
    /// 状态机状态。
    #[serde(default)]
    machine: SessionState,
    /// 上一次 `TurnEnd` 时没有流转状态，警告一直挂到模型真的流转为止。
    #[serde(default)]
    violation_pending: bool,
}

fn default_true() -> bool {
    true
}

/// 只读快照：读路径（每个 LLM 请求）用它在锁外组装注入文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub enabled: bool,
    pub machine: SessionState,
    pub violation_pending: bool,
}

/// 一个会话的可变状态。
#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    enabled: bool,
    machine: SessionState,
    violation_pending: bool,
    /// 本 turn 内是否调用过 `transition`。只在内存里维护，不落盘。
    transition_called: bool,
    dirty: bool,
}

impl SessionStore {
    /// 从磁盘加载。文件不存在、损坏或版本不符时以默认状态开始——
    /// 状态机状态是引导用的，损坏不该让插件整体不可用。
    pub fn load(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            enabled: true,
            machine: SessionState::default(),
            violation_pending: false,
            transition_called: false,
            dirty: false,
        };

        let Ok(raw) = std::fs::read(&store.path) else {
            return store;
        };
        // 直接吃掉刚读进来的缓冲，不再复制一份（与 hashline-edit 的会话账本同一手法）。
        let Ok(file) = astrcode_ext_common::json::parse_owned::<StateFile>(raw) else {
            return store;
        };
        if file.version != STATE_VERSION {
            return store;
        }

        store.enabled = file.enabled;
        store.machine = file.machine;
        store.violation_pending = file.violation_pending;
        store
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 只读快照。
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            enabled: self.enabled,
            machine: self.machine.clone(),
            violation_pending: self.violation_pending,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.dirty = true;
        }
    }

    pub fn machine(&self) -> &SessionState {
        &self.machine
    }

    pub fn machine_mut(&mut self) -> &mut SessionState {
        self.dirty = true;
        &mut self.machine
    }

    pub fn violation_pending(&self) -> bool {
        self.violation_pending
    }

    pub fn set_violation_pending(&mut self, pending: bool) {
        if self.violation_pending != pending {
            self.violation_pending = pending;
            self.dirty = true;
        }
    }

    /// 记下「本 turn 调用过 transition」。
    pub fn mark_transition_called(&mut self) {
        self.transition_called = true;
    }

    /// 读取并清空「本 turn 是否调用过 transition」。
    pub fn take_transition_called(&mut self) -> bool {
        std::mem::take(&mut self.transition_called)
    }

    /// 把状态写回磁盘。无改动时是空操作。
    pub fn save(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let file = StateFile {
            version: STATE_VERSION,
            enabled: self.enabled,
            machine: self.machine.clone(),
            violation_pending: self.violation_pending,
        };
        let payload = astrcode_ext_common::json::to_string(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        match hostpaths::write_file_atomic(&self.path, &payload) {
            Ok(()) => Ok(()),
            Err(error) => {
                // 写失败时保留脏标记，下一次变更还会再试。
                self.dirty = true;
                Err(error)
            },
        }
    }
}

/// 按会话隔离的状态注册表。同一个 worker 进程服务所有会话，状态必须按 `session_id` 分桶。
#[derive(Debug, Clone, Default)]
pub struct StoreRegistry {
    sessions: Arc<Mutex<BTreeMap<String, Arc<Mutex<SessionStore>>>>>,
}

impl StoreRegistry {
    /// 取某个会话的状态，第一次访问时从磁盘加载。
    ///
    /// 首次加载是同步文件读（状态文件不到 1 KiB），与 hashline-edit 的会话账本一致。
    pub fn session(&self, extension_id: &str, session_id: &str) -> Arc<Mutex<SessionStore>> {
        let mut sessions = lock(&self.sessions);
        Arc::clone(sessions.entry(session_id.to_owned()).or_insert_with(|| {
            Arc::new(Mutex::new(SessionStore::load(state_path(
                extension_id,
                session_id,
            ))))
        }))
    }
}

/// 取锁。锁中毒说明此前有 panic 穿过临界区，此时继续用内部值——
/// 状态只是引导用的缓存，不值得让整个扩展停摆。
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 在阻塞线程池上独占访问会话状态，并在闭包返回后按需落盘。
///
/// 闭包内可以放心做文件 I/O：`std::sync::MutexGuard` 不是 `Send`，不能跨 `.await` 持有，
/// 所以所有「改状态 + 写盘」都收敛到这一次 `spawn_blocking` 里。
pub async fn mutate<T, F>(
    store: Arc<Mutex<SessionStore>>,
    f: F,
) -> Result<T, tokio::task::JoinError>
where
    F: FnOnce(&mut SessionStore) -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut guard = lock(&store);
        let value = f(&mut guard);
        // 写盘失败不阻断本轮：脏标记已经留着，下一次变更会重试。
        let _ = guard.save();
        value
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Difficulty, ThinkingState};

    fn temp_store(name: &str) -> SessionStore {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-asymptotic-state-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        SessionStore::load(dir.join(STATE_FILE))
    }

    #[test]
    fn a_missing_file_starts_from_defaults() {
        let store = temp_store("missing");
        assert!(store.enabled());
        assert_eq!(store.machine().state, ThinkingState::START);
        assert!(!store.violation_pending());
    }

    /// 状态文件里只存了部分字段时，缺省值必须与「无记录」一致（开关默认启用）。
    #[test]
    fn a_partial_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-asymptotic-partial-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let path = dir.join(STATE_FILE);
        std::fs::write(&path, br#"{"version":1}"#).expect("写入失败");

        let store = SessionStore::load(path);
        assert!(store.enabled());
        assert_eq!(store.machine().state, ThinkingState::START);
    }

    #[test]
    fn a_corrupt_or_version_mismatched_file_is_discarded() {
        for (name, body) in [
            ("corrupt", "{not json".as_bytes()),
            ("version", br#"{"version":99,"enabled":false}"#.as_slice()),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "astrcode-asymptotic-{}-{}",
                name,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("创建临时目录失败");
            let path = dir.join(STATE_FILE);
            std::fs::write(&path, body).expect("写入失败");

            let store = SessionStore::load(path);
            assert!(store.enabled(), "{name} 应回落到默认启用");
            assert_eq!(store.machine().state, ThinkingState::START);
        }
    }

    #[test]
    fn state_round_trips_through_the_disk() {
        let mut store = temp_store("round-trip");
        store.set_enabled(false);
        store.machine_mut().difficulty = Some(Difficulty::HARD);
        store.machine_mut().state = ThinkingState::DESIGN;
        store.machine_mut().visited = vec![ThinkingState::DEEP_UNDERSTAND];
        store.set_violation_pending(true);
        store.save().expect("写盘失败");

        let reloaded = SessionStore::load(store.path().clone());
        assert!(!reloaded.enabled());
        assert_eq!(reloaded.machine().state, ThinkingState::DESIGN);
        assert_eq!(reloaded.machine().difficulty, Some(Difficulty::HARD));
        assert_eq!(reloaded.machine().visited, vec![ThinkingState::DEEP_UNDERSTAND]);
        assert!(reloaded.violation_pending());
    }

    #[test]
    fn save_is_a_noop_without_changes() {
        let mut store = temp_store("noop");
        assert!(!store.dirty);
        // 无改动时 save 直接返回，不会创建文件。
        store.save().expect("写盘失败");
        assert!(!store.path().exists());
    }

    #[test]
    fn transition_called_is_read_and_cleared_once() {
        let mut store = temp_store("transition-called");
        assert!(!store.take_transition_called());
        store.mark_transition_called();
        assert!(store.take_transition_called());
        assert!(!store.take_transition_called());
    }

    #[test]
    fn the_registry_buckets_by_session() {
        let registry = StoreRegistry::default();
        let first = registry.session("astrcode-asymptotic-thinking", "s-1");
        let same = registry.session("astrcode-asymptotic-thinking", "s-1");
        let other = registry.session("astrcode-asymptotic-thinking", "s-2");
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other));

        lock(&first).set_enabled(false);
        assert!(!lock(&first).enabled());
        assert!(lock(&other).enabled());
    }

    /// 会话 id 里出现路径分隔符时不能逃出插件数据目录。
    #[test]
    fn session_paths_stay_inside_the_plugin_directory() {
        let path = state_path("astrcode-asymptotic-thinking", "../../escape");
        let text = path.to_string_lossy().to_string();
        assert!(text.contains("astrcode-asymptotic-thinking/sessions/"));
        // `.` 是合法路径字符，所以结果里可能残留 `..`；关键是整串只有一个路径分量，
        // 没有分隔符就无法向上穿越。
        let component = path
            .parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .expect("会话目录名");
        assert!(!component.contains('/'));
        assert!(!component.contains('\\'));
    }
}
