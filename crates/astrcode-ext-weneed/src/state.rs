//! 钩子与命令共享的运行期状态。
//!
//! 磁盘扩展的钩子回调拿不到宿主的 UI 通道，所有可观测状态都收敛在这里，由 `/weneed status`
//! 统一展示。配置本身落盘在 [`ConfigStore`]，这里是内存缓存 + 热重载 + 统计。

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use astrcode_extension_worker::worker_prelude::ErrorPayload;

use astrcode_ext_common::config::ConfigStore;

use crate::{
    config::{Config, ReminderMode},
    drift::Drift,
    toggle::{SessionSwitch, Toggles},
};

/// 会话运行期表的容量上限。
///
/// 扩展进程可能长期服务很多会话，而漂移观测只是启发式状态、丢了无害，因此到顶就按插入
/// 顺序淘汰最旧的会话，避免无界增长。
const MAX_TRACKED_SESSIONS: usize = 256;

/// 一个会话的运行期观测。
#[derive(Debug, Default, Clone, Copy)]
struct SessionRuntime {
    /// 本会话已经历的 provider 请求数。
    requests: u64,
    /// 最近一次可判定的推理漂移结论；`None` 表示还没有过观测。
    last_drift: Option<bool>,
}

/// 按插入顺序淘汰的会话运行期表。
#[derive(Debug, Default)]
struct Sessions {
    runtimes: HashMap<String, SessionRuntime>,
    order: VecDeque<String>,
}

impl Sessions {
    fn entry(&mut self, session_id: &str) -> &mut SessionRuntime {
        if !self.runtimes.contains_key(session_id) {
            self.order.push_back(session_id.to_owned());
            self.runtimes
                .insert(session_id.to_owned(), SessionRuntime::default());
            self.evict();
        }
        // 新会话刚插入、老会话本就存在，两条路径都必然命中；用 `or_default`
        // 兜底而不是 `expect`，避免在长驻进程里留下 panic 路径。
        self.runtimes.entry(session_id.to_owned()).or_default()
    }

    fn evict(&mut self) {
        while self.order.len() > MAX_TRACKED_SESSIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.runtimes.remove(&oldest);
            }
        }
    }
}

/// 运行期统计。
#[derive(Debug, Default)]
pub struct Stats {
    /// 完整规范被注入进 system prompt 的轮数。
    pub injections: AtomicU64,
    /// 追加贴尾提醒的请求数。
    pub reminders: AtomicU64,
    /// 观测到推理漂移的次数。
    pub drifts: AtomicU64,
    /// 被工具守卫拦下的调用数。
    pub blocks: AtomicU64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        StatsSnapshot {
            injections: read(&self.injections),
            reminders: read(&self.reminders),
            drifts: read(&self.drifts),
            blocks: read(&self.blocks),
        }
    }
}

/// [`Stats`] 的只读快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    pub injections: u64,
    pub reminders: u64,
    pub drifts: u64,
    pub blocks: u64,
}

/// 钩子与命令共享的状态。
pub struct SharedState {
    store: ConfigStore,
    config: RwLock<Config>,
    toggles: Toggles,
    stats: Stats,
    sessions: Mutex<Sessions>,
}

impl SharedState {
    /// 从磁盘加载配置并构造状态。
    pub fn load(store: ConfigStore) -> (Arc<Self>, Option<String>) {
        let loaded = store.load();
        (
            Arc::new(Self {
                store,
                config: RwLock::new(loaded.config),
                toggles: Toggles::new(),
                stats: Stats::default(),
                sessions: Mutex::new(Sessions::default()),
            }),
            loaded.warning,
        )
    }

    /// 当前生效的全局配置。
    pub fn config(&self) -> Config {
        match self.config.read() {
            Ok(config) => config.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn store_path(&self) -> PathBuf {
        self.store.path().to_path_buf()
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    /// 归一化后写回磁盘并更新内存缓存。内存先更新，因此落盘失败也不会让本次会话的配置
    /// 回退；错误如实上抛给调用方。
    pub fn set_config(&self, next: Config) -> Result<(), String> {
        let normalized = next
            .normalized()
            .map_err(|error| format!("Failed to normalize config: {error}"))?;
        match self.config.write() {
            Ok(mut current) => *current = normalized.clone(),
            Err(poisoned) => *poisoned.into_inner() = normalized.clone(),
        }
        self.store
            .save(&normalized)
            .map_err(|error| format!("Failed to save {}: {error}", self.store.path().display()))
    }

    /// 从磁盘重新加载配置（`/weneed` 与 `session_start` 使用）。
    pub fn reload(&self) -> Option<String> {
        let loaded = self.store.load();
        match self.config.write() {
            Ok(mut current) => *current = loaded.config,
            Err(poisoned) => *poisoned.into_inner() = loaded.config,
        }
        loaded.warning
    }

    /// 本会话是否允许注入：全局开关是硬闸门，会话开关只能进一步关闭。
    pub async fn session_enabled(&self, session_id: &str) -> bool {
        let global = self.config().enabled;
        if !global {
            return false;
        }
        self.session_switch(session_id).await.allows(global)
    }

    /// 本会话的开关状态（不判定全局）。
    pub async fn session_switch(&self, session_id: &str) -> SessionSwitch {
        self.toggles.switch(session_id).await
    }

    pub async fn set_session_switch(
        &self,
        session_id: &str,
        switch: SessionSwitch,
    ) -> Result<(), ErrorPayload> {
        self.toggles.set(session_id, switch).await
    }

    /// 测试专用：直接写会话开关的内存缓存，绕开宿主往返。
    #[cfg(test)]
    pub(crate) fn cache_session_switch_for_tests(&self, session_id: &str, switch: SessionSwitch) {
        self.toggles.cache_for_tests(session_id, switch);
    }

    /// 记一次 provider 请求，并判定这一轮是否该贴提醒。
    ///
    /// `OnDrift` 的两条触发条件：
    ///
    /// - **每会话首个请求**：此时还没有任何推理可供观测，先贴一次引导；
    /// - **观测到漂移**：模型在后续请求里被拉回风格。
    pub fn should_remind(&self, session_id: &str, mode: ReminderMode) -> bool {
        if mode == ReminderMode::Off {
            return false;
        }
        // 锁中毒说明此前有 panic 穿过临界区：退化为「不贴提醒」而不是再 panic 一次。
        let Ok(mut sessions) = self.sessions.lock() else {
            return false;
        };
        let runtime = sessions.entry(session_id);
        runtime.requests = runtime.requests.saturating_add(1);
        match mode {
            ReminderMode::Off => false,
            ReminderMode::Always => true,
            ReminderMode::OnDrift => runtime.requests == 1 || runtime.last_drift == Some(true),
        }
    }

    /// 记录一次推理观测。
    ///
    /// [`Drift::NoObservation`] 不覆盖已有结论：provider 这一轮没回推理通道，不代表上一轮
    /// 的结论失效，也不该让提醒重新变成「每轮都贴」。
    pub fn note_observation(&self, session_id: &str, drift: Drift) {
        let drifted = match drift {
            Drift::NoObservation => return,
            Drift::OnStyle => false,
            Drift::OffStyle => true,
        };
        if drifted {
            self.stats.drifts.fetch_add(1, Ordering::Relaxed);
        }
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        sessions.entry(session_id).last_drift = Some(drifted);
    }

    pub fn count_injection(&self) {
        self.stats.injections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_reminder(&self) {
        self.stats.reminders.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_block(&self) {
        self.stats.blocks.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> (Arc<SharedState>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-weneed-state-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (state, warning) = SharedState::load(crate::config::store_at(dir.join("config.json")));
        assert!(warning.is_none());
        (state, dir)
    }

    #[test]
    fn on_drift_reminds_on_the_first_request_then_stays_quiet() {
        let (state, dir) = state("first-request");
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        // 没有任何观测 → 后续请求不再贴，避免对不回推理通道的 provider 刷屏。
        assert!(!state.should_remind("s1", ReminderMode::OnDrift));
        assert!(!state.should_remind("s1", ReminderMode::OnDrift));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_observed_drift_rearms_the_reminder() {
        let (state, dir) = state("drift-rearms");
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        state.note_observation("s1", Drift::OffStyle);
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        assert_eq!(state.stats().drifts, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn compliance_disarms_the_reminder() {
        let (state, dir) = state("compliant");
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        state.note_observation("s1", Drift::OffStyle);
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        state.note_observation("s1", Drift::OnStyle);
        assert!(!state.should_remind("s1", ReminderMode::OnDrift));
        assert_eq!(state.stats().drifts, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// provider 不回推理通道时，上一轮的漂移结论必须保留，否则提醒会退化成每轮都贴。
    #[test]
    fn a_missing_observation_does_not_erase_the_previous_verdict() {
        let (state, dir) = state("no-observation");
        state.note_observation("s1", Drift::OffStyle);
        state.note_observation("s1", Drift::NoObservation);
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn always_reminds_on_every_request() {
        let (state, dir) = state("always");
        for _ in 0..3 {
            assert!(state.should_remind("s1", ReminderMode::Always));
        }
        assert_eq!(state.stats().drifts, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn off_never_reminds() {
        let (state, dir) = state("off");
        state.note_observation("s1", Drift::OffStyle);
        assert!(!state.should_remind("s1", ReminderMode::Off));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sessions_are_observed_independently() {
        let (state, dir) = state("per-session");
        assert!(state.should_remind("s1", ReminderMode::OnDrift));
        assert!(state.should_remind("s2", ReminderMode::OnDrift));
        state.note_observation("s1", Drift::OnStyle);
        assert!(!state.should_remind("s1", ReminderMode::OnDrift));
        assert!(!state.should_remind("s2", ReminderMode::OnDrift));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 全局关是硬闸门：`/weneed on` 也打不开，而且判定根本不会走到宿主往返。
    #[tokio::test]
    async fn a_globally_disabled_plugin_stays_off() {
        let (state, dir) = state("global-off");
        let mut config = state.config();
        config.enabled = false;
        state.set_config(config).unwrap();

        state.cache_session_switch_for_tests("s1", SessionSwitch::On);
        assert!(!state.session_enabled("s1").await);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_session_can_turn_itself_off() {
        let (state, dir) = state("session-off");
        assert!(state.session_enabled("s1").await);
        state.cache_session_switch_for_tests("s1", SessionSwitch::Off);
        assert!(!state.session_enabled("s1").await);
        // 另一个会话不受影响。
        assert!(state.session_enabled("s2").await);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_session_table_evicts_the_oldest_entry() {
        let mut sessions = Sessions::default();
        for index in 0..(MAX_TRACKED_SESSIONS + 5) {
            sessions.entry(&format!("s{index}"));
        }
        assert_eq!(sessions.runtimes.len(), MAX_TRACKED_SESSIONS);
        assert!(!sessions.runtimes.contains_key("s0"));
        assert!(
            sessions
                .runtimes
                .contains_key(&format!("s{}", MAX_TRACKED_SESSIONS + 4))
        );
    }
}
