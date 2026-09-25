//! 每会话的前缀观测状态。
//!
//! 观测器只保存**上一次请求的逐消息指纹**，不保存消息正文；比较因此与正文体量无关，
//! 只与消息条数成正比。
//!
//! # 为什么状态在内存里
//!
//! 前缀对比天然只需要「上一次请求」这一个事实，跨进程重启没有任何可比较的对象：
//! 重启后的第一次请求必然是「首次观测」。落盘只会留下过期的对比对象，因此不落盘。
//! 扩展重载（`/reload`）同样清空观测，`/cache-doctor` 的输出里会如实说明。

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

use astrcode_extension_sdk::llm::LlmMessage;

use crate::prefix::{self, MessageDigest, PrefixReport};

/// 观测状态里保留的会话数上限。
///
/// 子 agent 会为每次委派新建会话，长驻进程必须给观测表设上限，否则条目只增不减。
/// 超出时按插入顺序淘汰最早的会话——被淘汰的会话下次请求会重新从「首次观测」开始，
/// 这只影响诊断的连续性，不影响正确性。
const MAX_TRACKED_SESSIONS: usize = 64;

/// 一次前缀断点的现场。
///
/// 只记录断点两侧消息的**结构摘要**（标签与字节数），不记录正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakRecord {
    /// 断点位置：第 `index` 条消息起与上一次不同。
    pub index: usize,
    /// 上一次请求在该位置的消息；本次请求更短时可能不存在。
    pub previous: Option<MessageDigest>,
    /// 本次请求在该位置的消息；断点落在本次末尾之外时不存在。
    pub current: Option<MessageDigest>,
    /// 本次请求比上一次短（历史被裁剪）。
    pub truncated: bool,
    /// 断点发生在第几次请求上（从 1 起）。
    pub at_request: u64,
}

/// 一次观测的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchOutcome {
    /// 本会话的第几次请求（从 1 起）。
    pub request_index: u64,
    /// 与上一次请求的对比；首次观测时为 `None`。
    pub report: Option<PrefixReport>,
    /// 本次是否检测到前缀断点。
    pub is_break: bool,
}

/// 可渲染的会话观测快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionSnapshot {
    /// 已观测的请求数。
    pub requests: u64,
    /// 前缀完整保留的请求数。
    pub preserved: u64,
    /// 检测到前缀断点的请求数。
    pub broken: u64,
    /// 最近一次断点。
    pub last_break: Option<BreakRecord>,
    /// 最近若干次请求的前缀对比，按时间先后排列。
    pub recent: Vec<PrefixReport>,
}

impl SessionSnapshot {
    /// 前缀保留率；还没有对比样本时返回 `None`（而不是 100%，避免误导）。
    pub fn preserved_rate(&self) -> Option<f64> {
        let compared = self.preserved + self.broken;
        (compared > 0).then(|| self.preserved as f64 / compared as f64)
    }

    /// 已观测的请求数是否为 0。
    pub fn is_empty(&self) -> bool {
        self.requests == 0
    }
}

#[derive(Debug, Default)]
struct SessionWatch {
    previous: Vec<MessageDigest>,
    requests: u64,
    preserved: u64,
    broken: u64,
    last_break: Option<BreakRecord>,
    recent: VecDeque<PrefixReport>,
}

/// 全部会话的观测状态。
#[derive(Debug, Default)]
pub struct WatchRegistry {
    sessions: Mutex<SessionTable>,
}

#[derive(Debug, Default)]
struct SessionTable {
    order: VecDeque<String>,
    entries: HashMap<String, SessionWatch>,
}

impl WatchRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 观测一次 provider 请求。
    ///
    /// `history` 是保留在快照里的最近对比条数上限；超出部分按时间顺序丢弃。
    pub fn observe(
        &self,
        session_id: &str,
        messages: &[LlmMessage],
        history: usize,
    ) -> WatchOutcome {
        let current = prefix::digest_messages(messages);
        let Ok(mut table) = self.sessions.lock() else {
            // 锁中毒说明此前有 panic 穿过临界区。观测是纯诊断，退化为「不记录」即可，
            // 绝不因为观测失败影响 provider 请求本身。
            return WatchOutcome {
                request_index: 0,
                report: None,
                is_break: false,
            };
        };

        table.touch(session_id);
        let watch = table.entries.entry(session_id.to_owned()).or_default();
        watch.requests = watch.requests.saturating_add(1);

        let report = if watch.previous.is_empty() && watch.requests == 1 {
            None
        } else {
            Some(prefix::compare(&watch.previous, &current))
        };

        if let Some(report) = report {
            if report.preserved() {
                watch.preserved = watch.preserved.saturating_add(1);
            } else {
                watch.broken = watch.broken.saturating_add(1);
                watch.last_break = Some(BreakRecord {
                    index: report.common,
                    previous: watch.previous.get(report.common).copied(),
                    current: current.get(report.common).copied(),
                    truncated: report.truncated(),
                    at_request: watch.requests,
                });
            }
            watch.recent.push_back(report);
            while watch.recent.len() > history.max(1) {
                watch.recent.pop_front();
            }
        }

        watch.previous = current;

        WatchOutcome {
            request_index: watch.requests,
            report,
            is_break: report.is_some_and(|report| !report.preserved()),
        }
    }

    /// 读取某个会话的观测快照；未观测过该会话时返回 `None`。
    pub fn snapshot(&self, session_id: &str) -> Option<SessionSnapshot> {
        let table = self.sessions.lock().ok()?;
        let watch = table.entries.get(session_id)?;
        Some(SessionSnapshot {
            requests: watch.requests,
            preserved: watch.preserved,
            broken: watch.broken,
            last_break: watch.last_break,
            recent: watch.recent.iter().copied().collect(),
        })
    }

    /// 清空某个会话的计数与最近对比。
    ///
    /// 刻意保留 `previous` 指纹：清空计数不该顺带把「上一次请求」也忘掉，否则下一次
    /// 请求会被误报成首次观测，前缀连续性断在这里。
    pub fn reset(&self, session_id: &str) {
        let Ok(mut table) = self.sessions.lock() else {
            return;
        };
        if let Some(watch) = table.entries.get_mut(session_id) {
            watch.requests = 0;
            watch.preserved = 0;
            watch.broken = 0;
            watch.last_break = None;
            watch.recent.clear();
        }
    }

    /// 丢弃某个会话的全部观测状态（含上一次请求指纹）。
    pub fn forget(&self, session_id: &str) {
        let Ok(mut table) = self.sessions.lock() else {
            return;
        };
        table.entries.remove(session_id);
        table.order.retain(|id| id != session_id);
    }

    /// 当前跟踪的会话数，用于测试与诊断。
    pub fn tracked_sessions(&self) -> usize {
        self.sessions
            .lock()
            .map(|table| table.entries.len())
            .unwrap_or(0)
    }
}

impl SessionTable {
    fn touch(&mut self, session_id: &str) {
        if self.entries.contains_key(session_id) {
            return;
        }
        self.order.push_back(session_id.to_owned());
        while self.order.len() > MAX_TRACKED_SESSIONS {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use astrcode_extension_sdk::llm::LlmMessage;

    use super::*;

    const HISTORY: usize = 20;

    fn registry() -> WatchRegistry {
        WatchRegistry::new()
    }

    #[test]
    fn the_first_request_has_nothing_to_compare_against() {
        let watch = registry();
        let outcome = watch.observe("s1", &[LlmMessage::user("hi")], HISTORY);
        assert_eq!(outcome.request_index, 1);
        assert_eq!(outcome.report, None);
        assert!(!outcome.is_break);

        let snapshot = watch.snapshot("s1").expect("应已记录");
        assert_eq!(snapshot.requests, 1);
        assert!(snapshot.recent.is_empty());
        assert_eq!(snapshot.preserved_rate(), None);
    }

    #[test]
    fn appended_history_is_counted_as_preserved() {
        let watch = registry();
        watch.observe("s1", &[LlmMessage::user("one")], HISTORY);
        let outcome = watch.observe(
            "s1",
            &[LlmMessage::user("one"), LlmMessage::assistant("two")],
            HISTORY,
        );

        assert!(outcome.report.expect("应产生对比").preserved());
        assert!(!outcome.is_break);

        let snapshot = watch.snapshot("s1").unwrap();
        assert_eq!(snapshot.requests, 2);
        assert_eq!(snapshot.preserved, 1);
        assert_eq!(snapshot.broken, 0);
        assert_eq!(snapshot.last_break, None);
        assert_eq!(snapshot.preserved_rate(), Some(1.0));
    }

    #[test]
    fn a_rewritten_history_is_recorded_as_a_break() {
        let watch = registry();
        watch.observe(
            "s1",
            &[LlmMessage::user("one"), LlmMessage::assistant("two")],
            HISTORY,
        );
        let outcome = watch.observe(
            "s1",
            &[LlmMessage::user("one"), LlmMessage::assistant("TWO")],
            HISTORY,
        );

        assert!(outcome.is_break);
        let snapshot = watch.snapshot("s1").unwrap();
        assert_eq!(snapshot.broken, 1);
        assert_eq!(snapshot.preserved_rate(), Some(0.0));

        let last = snapshot.last_break.expect("应记录断点");
        assert_eq!(last.index, 1);
        assert_eq!(last.at_request, 2);
        assert!(!last.truncated);
        assert_eq!(
            last.previous.map(|digest| digest.label),
            Some(crate::prefix::Label::AssistantText)
        );
        assert_eq!(
            last.current.map(|digest| digest.label),
            Some(crate::prefix::Label::AssistantText)
        );
    }

    #[test]
    fn truncation_marks_the_break_and_keeps_no_current_digest() {
        let watch = registry();
        watch.observe(
            "s1",
            &[
                LlmMessage::user("one"),
                LlmMessage::assistant("two"),
                LlmMessage::user("three"),
            ],
            HISTORY,
        );
        watch.observe("s1", &[LlmMessage::user("one")], HISTORY);

        let last = watch.snapshot("s1").unwrap().last_break.unwrap();
        assert!(last.truncated);
        assert_eq!(last.index, 1);
        assert!(last.previous.is_some());
        assert_eq!(last.current, None, "断点落在本次长度之外");
    }

    #[test]
    fn sessions_are_tracked_separately() {
        let watch = registry();
        watch.observe("s1", &[LlmMessage::user("one")], HISTORY);
        watch.observe("s2", &[LlmMessage::user("other")], HISTORY);
        watch.observe("s2", &[LlmMessage::user("other"), LlmMessage::user("next")], HISTORY);

        assert_eq!(watch.snapshot("s1").unwrap().requests, 1);
        assert_eq!(watch.snapshot("s2").unwrap().requests, 2);
        assert_eq!(watch.snapshot("s2").unwrap().preserved, 1);
        assert!(watch.snapshot("never-seen").is_none());
    }

    #[test]
    fn reset_keeps_the_comparison_baseline() {
        let watch = registry();
        watch.observe("s1", &[LlmMessage::user("one")], HISTORY);
        watch.observe("s1", &[LlmMessage::user("one"), LlmMessage::assistant("two")], HISTORY);
        watch.reset("s1");

        let snapshot = watch.snapshot("s1").unwrap();
        assert_eq!(snapshot.requests, 0);
        assert_eq!(snapshot.broken, 0);
        assert!(snapshot.recent.is_empty());

        // 清空计数后紧接着的请求仍应与「上一次请求」比较，而不是被当成首次观测。
        let outcome = watch.observe(
            "s1",
            &[
                LlmMessage::user("one"),
                LlmMessage::assistant("two"),
                LlmMessage::user("three"),
            ],
            HISTORY,
        );
        assert!(outcome.report.expect("应产生对比").preserved());
    }

    #[test]
    fn forget_drops_everything_for_a_session() {
        let watch = registry();
        watch.observe("s1", &[LlmMessage::user("one")], HISTORY);
        watch.forget("s1");
        assert!(watch.snapshot("s1").is_none());
        assert_eq!(watch.tracked_sessions(), 0);
    }

    #[test]
    fn recent_reports_are_bounded_by_the_history_limit() {
        let watch = registry();
        watch.observe("s1", &[LlmMessage::user("0")], HISTORY);
        for index in 1..10 {
            watch.observe(
                "s1",
                &[LlmMessage::user("0"), LlmMessage::user(index.to_string())],
                3,
            );
        }

        let snapshot = watch.snapshot("s1").unwrap();
        assert_eq!(snapshot.requests, 10);
        assert_eq!(snapshot.recent.len(), 3, "history=3 时只保留最近 3 条对比");
    }

    #[test]
    fn a_zero_history_limit_still_keeps_one_entry() {
        let watch = registry();
        watch.observe("s1", &[LlmMessage::user("0")], HISTORY);
        watch.observe("s1", &[LlmMessage::user("0"), LlmMessage::user("1")], 0);
        assert_eq!(watch.snapshot("s1").unwrap().recent.len(), 1);
    }

    #[test]
    fn tracked_sessions_are_capped() {
        let watch = registry();
        for index in 0..(MAX_TRACKED_SESSIONS + 5) {
            watch.observe(&format!("s{index}"), &[LlmMessage::user("x")], HISTORY);
        }
        assert_eq!(watch.tracked_sessions(), MAX_TRACKED_SESSIONS);
        assert!(watch.snapshot("s0").is_none(), "最早的会话应被淘汰");
        assert!(watch.snapshot("s4").is_none());
        assert!(watch.snapshot("s5").is_some());
    }
}
