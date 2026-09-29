//! 钩子与命令共享的运行期状态。
//!
//! 磁盘扩展的钩子回调拿不到宿主的 UI 通道，所有可观测状态都收敛在这里，由 `/sleep status`
//! 统一展示。配置本身落盘在 [`ConfigStore`]，这里是内存缓存 + 热重载 + 运行期表 + 统计。
//!
//! # 会话开关为什么不放在这里
//!
//! 开关必须跨扩展重载存活——无人值守场景下扩展进程会被宿主重载，而你正是指望它替你盯着。
//! 因此开关写宿主的 `session_state`（见 [`Toggles`]），本模块只在内存里缓存它。
//! 反过来，**运行期计数不持久化**：进程重启后从零开始计数是安全的（最坏情况是多跑几次
//! 续跑），而写盘会让每次续跑多一次 IO。

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use astrcode_extension_worker::worker_prelude::*;

use astrcode_ext_common::config::ConfigStore;

use crate::{config::Config, plan::StopReason};

/// 会话运行期表的容量上限。
///
/// 扩展进程可能长期服务很多会话，而续跑状态只是启发式计数、丢了无害，因此到顶就按插入
/// 顺序淘汰最旧的会话，避免无界增长。
const MAX_TRACKED_SESSIONS: usize = 256;

/// 一个会话的续跑运行期状态。
#[derive(Debug, Default, Clone)]
pub struct SessionRuntime {
    /// 本次人工 turn 内已经续跑了几次。
    pub continuations: u32,
    /// 自上次续跑以来产生的工具调用数。
    pub tool_calls: u32,
    /// 连续多少次续跑都没有产生工具调用。
    pub idle_streak: u32,
    /// 最近一次停止续跑的原因；续跑重新开始时清除。
    pub stop_reason: Option<StopReason>,
    /// 连续多少次续跑都「没有新内容」（没有工具调用，且回复与上一次重复或为空）。
    pub no_progress_streak: u32,
    /// 本次人工 turn 内已成功投递的失败重试次数。
    pub retries: u32,
    /// 上一步回复的内容指纹，用于判定「与上一次重复」。
    pub last_assistant: Option<u64>,
    /// 持久事件日志的读取游标。只判新事件——历史错误不该触发重试。
    pub event_cursor: Option<String>,
    /// 最近一次被识别的失败摘要，供 `/sleep status` 展示。
    pub last_failure: Option<String>,
}

/// [`SessionRuntime`] 的只读快照，供判定与展示使用。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Progress {
    pub continuations: u32,
    pub tool_calls: u32,
    pub idle_streak: u32,
    pub no_progress_streak: u32,
    pub retries: u32,
    pub stop_reason: Option<StopReason>,
}

impl SessionRuntime {
    /// 取只读快照。判定与展示都只吃快照，锁的持有范围因此收敛在这一处。
    fn snapshot(&self) -> Progress {
        Progress {
            continuations: self.continuations,
            tool_calls: self.tool_calls,
            idle_streak: self.idle_streak,
            no_progress_streak: self.no_progress_streak,
            retries: self.retries,
            stop_reason: self.stop_reason.clone(),
        }
    }

    /// 人工接手：预算、空转链、复读链一起归零。
    ///
    /// 游标与「最近一次失败」是**观测**状态，跨人工输入保留：它们记的是宿主日志读到哪儿、
    /// 上次坏成什么样，和预算无关，清掉只会让下一次 turn_end 白白丢一次判定。
    fn reset_for_prompt(&mut self) {
        self.continuations = 0;
        self.tool_calls = 0;
        self.idle_streak = 0;
        self.no_progress_streak = 0;
        self.retries = 0;
        self.last_assistant = None;
        self.stop_reason = None;
    }
}

/// 按插入顺序淘汰的会话运行期表。
#[derive(Debug, Default)]
struct Sessions {
    runtimes: HashMap<String, SessionRuntime>,
    order: VecDeque<String>,
}

impl Sessions {
    /// 取或建一个会话的运行期状态。
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

    /// 只读取一个会话；未知会话返回 `None`，**不插入**，因此读路径不会挤掉别的会话。
    fn get(&self, session_id: &str) -> Option<&SessionRuntime> {
        self.runtimes.get(session_id)
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
    /// 自动续跑的总次数。
    pub continuations: AtomicU64,
    /// 被自动应答的提问调用数。
    pub answers: AtomicU64,
    /// 因空转熔断而停止的次数。
    pub idle_stops: AtomicU64,
    /// 因复读熔断而停止的次数。
    pub no_progress_stops: AtomicU64,
    /// 失败后自动重试并成功投递的次数。
    pub retries: AtomicU64,
}

impl Stats {
    fn snapshot(&self) -> StatsSnapshot {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        StatsSnapshot {
            continuations: read(&self.continuations),
            answers: read(&self.answers),
            idle_stops: read(&self.idle_stops),
            no_progress_stops: read(&self.no_progress_stops),
            retries: read(&self.retries),
        }
    }
}

/// [`Stats`] 的只读快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    pub continuations: u64,
    pub answers: u64,
    pub idle_stops: u64,
    pub no_progress_stops: u64,
    pub retries: u64,
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

    /// 从磁盘重新加载配置（`/sleep` 与 `session_start` 使用）。
    pub fn reload(&self) -> Option<String> {
        let loaded = self.store.load();
        match self.config.write() {
            Ok(mut current) => *current = loaded.config,
            Err(poisoned) => *poisoned.into_inner() = loaded.config,
        }
        loaded.warning
    }

    /// 本会话是否允许续跑：全局开关是硬闸门，会话开关只能进一步关闭。
    pub async fn session_enabled(&self, session_id: &str) -> bool {
        let global = self.config().enabled;
        if !global {
            return false;
        }
        self.toggles.switch(session_id).await.allows(global)
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

    /// 读一个会话的运行期快照。未知会话返回默认值。
    pub fn progress(&self, session_id: &str) -> Progress {
        let Ok(sessions) = self.sessions.lock() else {
            return Progress::default();
        };
        sessions
            .get(session_id)
            .map_or_else(Progress::default, SessionRuntime::snapshot)
    }

    /// 记一次工具调用：它是「模型还在干活」的唯一硬信号。
    pub fn note_tool_call(&self, session_id: &str) {
        self.update(session_id, |runtime| {
            runtime.tool_calls = runtime.tool_calls.saturating_add(1);
        });
    }

    /// 结算一次续跑：计数加一、工具调用归零（下一次续跑重新统计本步的活动）。
    pub fn note_continuation(&self, session_id: &str) {
        self.update(session_id, |runtime| {
            runtime.continuations = runtime.continuations.saturating_add(1);
            runtime.tool_calls = 0;
            runtime.stop_reason = None;
        });
        self.stats.continuations.fetch_add(1, Ordering::Relaxed);
    }

    /// 结算本步活动进空转链与复读链，并返回结算后的快照。
    ///
    /// 「本步有没有产生工具调用」是空转判定的**唯一**输入，「回复有没有新内容」是复读判定
    /// 的唯一输入；两者都在这里结算，判定方（[`crate::plan::decide`]）就不必再关心它们。
    /// 工具调用计数同时归零——它的语义是「自上次结算以来的活动」。
    ///
    /// `assistant_text` 是刚结束那一步的可见回复。失败重试不走这里：那时没有「一步跑完」，
    /// 上一步的指纹保持不动。
    pub fn settle_step(&self, session_id: &str, assistant_text: &str) -> Progress {
        let Ok(mut sessions) = self.sessions.lock() else {
            return Progress::default();
        };
        let runtime = sessions.entry(session_id);
        runtime.idle_streak = if runtime.tool_calls == 0 {
            runtime.idle_streak.saturating_add(1)
        } else {
            0
        };
        if crate::plan::is_no_progress(runtime.last_assistant, assistant_text, runtime.tool_calls) {
            runtime.no_progress_streak = runtime.no_progress_streak.saturating_add(1);
        } else {
            runtime.no_progress_streak = 0;
        }
        runtime.last_assistant = Some(crate::plan::text_fingerprint(assistant_text));
        runtime.tool_calls = 0;
        runtime.snapshot()
    }

    /// 人工接手：预算与空转链一起归零。
    ///
    /// 上限的语义是「单次人工 turn 的预算」，所以人插过一次话就重新开始计数——这正是
    /// 「到顶只停止续跑、开关保持开启」能自洽的原因。
    pub fn reset_budget(&self, session_id: &str) {
        self.update(session_id, SessionRuntime::reset_for_prompt);
    }

    /// 记一次失败重试的投递。
    pub fn note_retry(&self, session_id: &str) {
        self.update(session_id, |runtime| {
            runtime.retries = runtime.retries.saturating_add(1);
        });
        self.stats.retries.fetch_add(1, Ordering::Relaxed);
    }

    /// 记下最近一次被识别的失败摘要。
    pub fn note_failure(&self, session_id: &str, summary: &str) {
        self.update(session_id, |runtime| {
            runtime.last_failure = Some(summary.to_owned());
        });
    }

    /// 最近一次被识别的失败摘要，供 `/sleep status` 展示。
    pub fn last_failure(&self, session_id: &str) -> Option<String> {
        let sessions = self.sessions.lock().ok()?;
        sessions.get(session_id)?.last_failure.clone()
    }

    /// 读持久事件日志的游标。`None` 表示还没建立过——首次读只能从头扫，见 [`crate::hook`]。
    pub fn event_cursor(&self, session_id: &str) -> Option<String> {
        let Ok(sessions) = self.sessions.lock() else {
            return None;
        };
        sessions
            .get(session_id)
            .and_then(|runtime| runtime.event_cursor.clone())
    }

    /// 推进持久事件日志的游标。
    pub fn set_event_cursor(&self, session_id: &str, cursor: Option<String>) {
        self.update(session_id, |runtime| {
            runtime.event_cursor = cursor;
        });
    }

    /// 记下停止续跑的原因；续跑重新开始时由 [`Self::note_continuation`] 清除。
    pub fn record_stop(&self, session_id: &str, reason: StopReason) {
        match reason {
            StopReason::Idle { .. } => {
                self.stats.idle_stops.fetch_add(1, Ordering::Relaxed);
            }
            StopReason::NoProgress { .. } => {
                self.stats.no_progress_stops.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        self.update(session_id, |runtime| {
            runtime.stop_reason = Some(reason);
        });
    }

    pub fn count_answer(&self) {
        self.stats.answers.fetch_add(1, Ordering::Relaxed);
    }

    /// 测试专用：直接写会话开关的内存缓存，绕开宿主往返。
    #[cfg(test)]
    pub(crate) fn cache_session_switch_for_tests(&self, session_id: &str, switch: SessionSwitch) {
        self.toggles.cache_for_tests(session_id, switch);
    }

    /// 在运行期表上做一次读改。锁中毒说明此前有 panic 穿过临界区：退化为不做任何事，
    /// 而不是在长驻进程里再 panic 一次。
    fn update(&self, session_id: &str, f: impl FnOnce(&mut SessionRuntime)) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        f(sessions.entry(session_id));
    }
}

// ─── 每会话开关 ─────────────────────────────────────────────────────────

/// `session_state` 中的键名。键只允许 ASCII 字母数字与 `-`、`_`、`.`。
const STATE_KEY: &str = "sleep-continue";
const STATE_ON: &str = "on";
const STATE_OFF: &str = "off";

/// 会话级开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSwitch {
    /// 从未设置过：跟随全局配置。
    #[default]
    Unset,
    /// `/sleep on`：开启本会话（覆盖此前写入的关闭）。
    On,
    /// `/sleep off`：关闭本会话。
    Off,
}

impl SessionSwitch {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// 中文展示名，用于 `/sleep status`。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unset => "未设置（跟随全局）",
            Self::On => "开",
            Self::Off => "关",
        }
    }

    /// 会话是否允许续跑。
    ///
    /// 全局开关是硬闸门（默认开，供「一次关掉所有会话」用）；**真正的开关在会话上**，
    /// 必须显式 `/sleep on` 才生效——续跑会持续消耗额度，不能靠默认值替人做决定。
    /// 因此 `Unset` 与 `Off` 在判定上等价，区别只在 `/sleep status` 的展示（是否开过）。
    pub const fn allows(self, global_enabled: bool) -> bool {
        global_enabled && matches!(self, Self::On)
    }

    fn parse(value: Option<&str>) -> Self {
        match value {
            Some(STATE_ON) => Self::On,
            Some(STATE_OFF) => Self::Off,
            _ => Self::Unset,
        }
    }
}

/// 各会话的开关。
#[derive(Default)]
struct Toggles {
    cache: Mutex<HashMap<String, SessionSwitch>>,
}

impl Toggles {
    fn new() -> Self {
        Self::default()
    }

    /// 读取会话开关。首次访问该会话时从 `session_state` 载入并缓存。
    ///
    /// 载入失败回落 [`SessionSwitch::Unset`] 而非报错：读不到开关状态不该阻断本轮判定，
    /// 而 `Unset` 跟着全局走，是最不意外的回落。
    async fn switch(&self, session_id: &str) -> SessionSwitch {
        if let Some(cached) = self.cached(session_id) {
            return cached;
        }
        let loaded = self.load().await.unwrap_or_default();
        self.cache_value(session_id, loaded);
        loaded
    }

    /// 写入会话开关并持久化。
    ///
    /// 内存缓存先更新，因此即使持久化失败，本次会话内的开关也立即生效；
    /// 错误仍然上抛，让调用方把持久化失败如实告诉用户。
    async fn set(&self, session_id: &str, switch: SessionSwitch) -> Result<(), ErrorPayload> {
        self.cache_value(session_id, switch);
        HostClient::session_state()
            .write(HostSessionStateWriteRequest {
                key: STATE_KEY.to_owned(),
                // `Unset` 写空串而不是提前返回：读路径把「文件缺失」与「空串」都解析成
                // `Unset`，写空串因此等价于清除，且让宿主的落盘状态与内存缓存保持一致。
                content: match switch {
                    SessionSwitch::Unset => String::new(),
                    SessionSwitch::On => STATE_ON.to_owned(),
                    SessionSwitch::Off => STATE_OFF.to_owned(),
                },
            })
            .await
    }

    /// 测试专用：直接写内存缓存，绕开宿主往返。
    #[cfg(test)]
    fn cache_for_tests(&self, session_id: &str, switch: SessionSwitch) {
        self.cache_value(session_id, switch);
    }

    /// 读缓存。锁中毒说明此前有 panic 穿过临界区，此时退化为「无缓存」而非再次 panic。
    fn cached(&self, session_id: &str) -> Option<SessionSwitch> {
        self.cache.lock().ok()?.get(session_id).copied()
    }

    fn cache_value(&self, session_id: &str, switch: SessionSwitch) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(session_id.to_owned(), switch);
        }
    }

    async fn load(&self) -> Result<SessionSwitch, ErrorPayload> {
        let output = HostClient::session_state()
            .read(HostSessionStateReadRequest {
                key: STATE_KEY.to_owned(),
            })
            .await?;
        Ok(SessionSwitch::parse(output.content.as_deref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> (Arc<SharedState>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-sleep-continue-state-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (state, warning) = SharedState::load(crate::config::store_at(dir.join("config.json")));
        assert!(warning.is_none());
        (state, dir)
    }

    /// 状态键必须落在宿主的键名校验内：非空、≤128 字节、仅 ASCII 字母数字与 `-_.`。
    #[test]
    fn the_state_key_passes_host_validation() {
        assert!(!STATE_KEY.is_empty());
        assert!(STATE_KEY.len() <= 128);
        assert!(
            STATE_KEY
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "键名含宿主会拒绝的字符：{STATE_KEY}"
        );
    }

    #[test]
    fn switch_round_trips_through_its_wire_names() {
        for switch in [SessionSwitch::Unset, SessionSwitch::On, SessionSwitch::Off] {
            assert_eq!(SessionSwitch::parse(Some(switch.as_str())), switch);
        }
        assert_eq!(SessionSwitch::parse(None), SessionSwitch::Unset);
        assert_eq!(SessionSwitch::parse(Some("whatever")), SessionSwitch::Unset);
    }

    /// 缓存命中时不应触碰宿主；没有 host api 作用域也能返回缓存值。
    #[tokio::test]
    async fn a_cached_session_does_not_need_the_host() {
        let (state, dir) = state("cached");
        state.cache_session_switch_for_tests("s-1", SessionSwitch::Off);
        assert_eq!(state.session_switch("s-1").await, SessionSwitch::Off);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 只有显式的 `On` 才允许续跑：默认值不能替人打开一个会持续烧额度的循环。
    #[test]
    fn only_an_explicit_on_enables_the_session() {
        assert!(SessionSwitch::On.allows(true));
        assert!(!SessionSwitch::Unset.allows(true));
        assert!(!SessionSwitch::Off.allows(true));
        // 全局关是硬闸门：会话开关不能反向打开。
        assert!(!SessionSwitch::On.allows(false));
    }

    /// 没配过的会话不允许续跑：这是「默认不替人决定」的兜底。
    #[tokio::test]
    async fn a_fresh_session_is_disabled() {
        let (state, dir) = state("fresh");
        state.cache_session_switch_for_tests("s-1", SessionSwitch::Unset);
        assert!(!state.session_enabled("s-1").await);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn continuations_count_up_and_reset_the_tool_counter() {
        let (state, dir) = state("continuations");
        state.note_tool_call("s-1");
        state.note_continuation("s-1");
        let progress = state.progress("s-1");
        assert_eq!(progress.continuations, 1);
        // 工具调用归零：下一次续跑重新统计本步的活动。
        assert_eq!(progress.tool_calls, 0);

        state.note_continuation("s-1");
        assert_eq!(state.progress("s-1").continuations, 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 空转链由「本步有没有产生工具调用」驱动：没有就累加，有就断开。
    ///
    /// 这是空转熔断的全部输入，因此它必须在结算处就被钉死，而不是留给判定方去猜。
    #[test]
    fn settle_step_drives_the_idle_streak_from_tool_activity() {
        let (state, dir) = state("settle");

        // 空跑两次：没有任何工具调用。
        assert_eq!(state.settle_step("s-1", "第一次").idle_streak, 1);
        assert_eq!(state.settle_step("s-1", "第二次").idle_streak, 2);

        // 干了一次活：链条断开。
        state.note_tool_call("s-1");
        assert_eq!(state.settle_step("s-1", "第三次").idle_streak, 0);

        // 结算把工具调用计数清零，同一次活动不会被重复计入。
        assert_eq!(state.progress("s-1").tool_calls, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resetting_the_budget_clears_everything() {
        let (state, dir) = state("reset");
        state.note_continuation("s-1");
        state.settle_step("s-1", "一步");
        state.record_stop("s-1", StopReason::CapReached { count: 1, max: 1 });

        state.reset_budget("s-1");
        assert_eq!(state.progress("s-1"), Progress::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stop_reason_is_cleared_by_the_next_continuation() {
        let (state, dir) = state("stop-reason");
        state.record_stop(
            "s-1",
            StopReason::Idle {
                streak: 3,
                limit: 3,
            },
        );
        assert!(state.progress("s-1").stop_reason.is_some());

        state.note_continuation("s-1");
        assert!(state.progress("s-1").stop_reason.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reading_an_unknown_session_does_not_create_it() {
        let (state, dir) = state("unknown");
        assert_eq!(state.progress("never-seen"), Progress::default());
        let sessions = state.sessions.lock().unwrap();
        assert!(!sessions.runtimes.contains_key("never-seen"));
        drop(sessions);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sessions_are_evicted_in_insertion_order() {
        let (state, dir) = state("evict");
        for index in 0..(MAX_TRACKED_SESSIONS + 8) {
            state.note_tool_call(&format!("s-{index}"));
        }
        let sessions = state.sessions.lock().unwrap();
        assert_eq!(sessions.runtimes.len(), MAX_TRACKED_SESSIONS);
        assert!(!sessions.runtimes.contains_key("s-0"));
        drop(sessions);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stats_only_count_idle_stops() {
        let (state, dir) = state("stats");
        state.record_stop("s-1", StopReason::CapReached { count: 1, max: 1 });
        state.record_stop(
            "s-1",
            StopReason::Idle {
                streak: 3,
                limit: 3,
            },
        );
        assert_eq!(state.stats().idle_stops, 1);

        state.note_continuation("s-1");
        state.count_answer();
        let stats = state.stats();
        assert_eq!(stats.continuations, 1);
        assert_eq!(stats.answers, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
