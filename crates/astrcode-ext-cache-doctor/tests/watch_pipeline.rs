//! 端到端：驱动观测钩子与命令渲染走完整路径。
//!
//! 这里验证的是**组合行为**——钩子观测到的前缀事实，经过快照与渲染，真的会出现在
//! `/cache-doctor` 的输出里；以及用量读取失败时诊断仍然可用。纯判定逻辑由各模块的
//! 单元测试覆盖。

use std::sync::{Arc, Mutex};

use astrcode_extension_sdk::s5r::hooks::ProviderHookInput;
use astrcode_extension_worker::testing::{HostApi, with_host_api};
use astrcode_extension_worker::worker_prelude::{
    ErrorPayload, HostOperation, ModelSelection, ProviderResult, WireErrorCode,
};
use async_trait::async_trait;
use serde_json::{Value, json};

use astrcode_ext_cache_doctor::{
    config::ConfigStore,
    report,
    usage::scan_session,
    watch::WatchRegistry,
};

const SESSION: &str = "session-1";

/// 宿主侧 `astrcode.session.read_events` 的线缆名。
const READ_EVENTS: &str = "astrcode.session.read_events";

// ─── 观测路径 ───────────────────────────────────────────────────────────

fn request(messages: Vec<astrcode_extension_worker::worker_prelude::LlmMessage>) -> ProviderHookInput {
    ProviderHookInput {
        request_id: "req-1".to_owned(),
        session_id: SESSION.to_owned(),
        working_dir: "/tmp".to_owned(),
        // 宿主组装钩子上下文时走 `ModelSelection::simple`，provider_kind 恒为空串。
        model: ModelSelection::simple("deepseek-v4.1-flash"),
        messages,
    }
}

fn user(text: &str) -> astrcode_extension_worker::worker_prelude::LlmMessage {
    astrcode_extension_worker::worker_prelude::LlmMessage::user(text)
}

fn temp_config(name: &str) -> ConfigStore {
    let dir = std::env::temp_dir().join(format!(
        "astrcode-cache-doctor-it-{}-{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    ConfigStore::at(dir.join("config.json"))
}

#[test]
fn a_growing_conversation_keeps_the_prefix_and_the_hook_always_allows() {
    let watch = WatchRegistry::new();
    let config = temp_config("growing");

    for messages in [
        vec![user("one")],
        vec![user("one"), user("two")],
        vec![user("one"), user("two"), user("three")],
    ] {
        let result = astrcode_ext_cache_doctor::command::observe(&watch, &config, &request(messages));
        assert!(matches!(result, ProviderResult::Allow));
    }

    let snapshot = watch.snapshot(SESSION).expect("应已观测");
    assert_eq!(snapshot.requests, 3);
    assert_eq!(snapshot.preserved, 2);
    assert_eq!(snapshot.broken, 0);
    assert_eq!(snapshot.last_break, None);

    let text = report::status_report(
        "deepseek-v4.1-flash",
        &config.get(),
        Some(&snapshot),
        "config.json",
    );
    assert!(text.contains("本会话 3 次请求 · 前缀完整 2 · 断点 0（保留率 100.0%）"), "{text}");
    assert!(text.contains("最近断点：无。"), "{text}");
    assert_eq!(report::status_text(Some(&snapshot)), "prefix 2/2");
}

#[test]
fn a_rewritten_history_shows_up_as_a_break_in_every_report() {
    let watch = WatchRegistry::new();
    let config = temp_config("rewritten");

    let _ = astrcode_ext_cache_doctor::command::observe(
        &watch,
        &config,
        &request(vec![user("one"), user("two")]),
    );
    // 历史被追溯改写：第二条内容变了。
    let _ = astrcode_ext_cache_doctor::command::observe(
        &watch,
        &config,
        &request(vec![user("one"), user("TWO")]),
    );

    let snapshot = watch.snapshot(SESSION).expect("应已观测");
    let last = snapshot.last_break.expect("应记录断点");
    assert_eq!(last.index, 1);
    assert_eq!(last.at_request, 2);
    assert!(!last.truncated);

    let status = report::status_report("m", &config.get(), Some(&snapshot), "config.json");
    // 两侧字节数相同（56）却仍被判定为断点：相等只看指纹，不看体量。
    assert!(status.contains("位置 #1（user · 56 字符 → user · 56 字符）"), "{status}");

    let stats = report::stats_report(&config.get(), Some(&snapshot), None);
    assert!(stats.contains("断点 #1"), "{stats}");

    let doctor = report::doctor_report("m", &config.get(), Some(&snapshot), None);
    assert!(doctor.contains("检测到 1 次历史被追溯改写（最近一次在第 1 条消息）"), "{doctor}");
}

#[test]
fn disabling_watch_stops_recording_without_changing_the_hook_result() {
    let watch = WatchRegistry::new();
    let config = temp_config("disabled");
    let mut settings = config.get();
    settings.watch = false;
    config.set(settings).expect("写入应成功");

    let result = astrcode_ext_cache_doctor::command::observe(
        &watch,
        &config,
        &request(vec![user("one")]),
    );
    assert!(matches!(result, ProviderResult::Allow));
    assert!(
        watch.snapshot(SESSION).is_none(),
        "关闭观测后不应留下任何记录"
    );

    let doctor = report::doctor_report("m", &config.get(), None, None);
    assert!(doctor.contains("观测已关闭"), "{doctor}");
}

#[test]
fn a_truncated_history_is_reported_as_such() {
    let watch = WatchRegistry::new();
    let config = temp_config("truncated");

    let _ = astrcode_ext_cache_doctor::command::observe(
        &watch,
        &config,
        &request(vec![user("one"), user("two"), user("three")]),
    );
    // 上下文压缩：历史被裁到只剩一条。
    let _ = astrcode_ext_cache_doctor::command::observe(&watch, &config, &request(vec![user("one")]));

    let snapshot = watch.snapshot(SESSION).unwrap();
    let last = snapshot.last_break.unwrap();
    assert!(last.truncated);
    assert_eq!(last.current, None);

    let status = report::status_report("m", &config.get(), Some(&snapshot), "config.json");
    assert!(status.contains("历史被裁剪"), "{status}");
}

// ─── 用量路径 ───────────────────────────────────────────────────────────

/// 按顺序吐预设页面的模拟宿主。
struct PagedHost {
    pages: Mutex<Vec<Value>>,
    requests: Mutex<Vec<Value>>,
}

impl PagedHost {
    fn new(pages: Vec<Value>) -> Self {
        Self {
            pages: Mutex::new(pages),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl HostApi for PagedHost {
    fn host_supports(&self, _operation: HostOperation) -> bool {
        true
    }

    async fn call(&self, capability: &str, input: Value) -> Result<Value, ErrorPayload> {
        if capability != READ_EVENTS {
            return Err(ErrorPayload::new(WireErrorCode::UnknownCapability, capability));
        }
        self.requests.lock().unwrap().push(input);
        let mut pages = self.pages.lock().unwrap();
        if pages.is_empty() {
            panic!("扫描请求了超出预设的页数");
        }
        Ok(pages.remove(0))
    }
}

fn event(seq: u64, payload: Value) -> Value {
    json!({
        "seq": seq,
        "id": format!("event-{seq}"),
        "session_id": SESSION,
        "timestamp": "2026-09-25T00:00:00Z",
        "payload": payload,
    })
}

fn page(events: Vec<Value>, next_cursor: &str, has_more: bool) -> Value {
    json!({ "events": events, "next_cursor": next_cursor, "has_more": has_more })
}

#[tokio::test]
async fn doctor_combines_prefix_stats_with_usage_read_from_the_host() {
    let watch = WatchRegistry::new();
    let config = temp_config("doctor");

    for messages in [
        vec![user("one")],
        vec![user("one"), user("two")],
    ] {
        let _ = astrcode_ext_cache_doctor::command::observe(&watch, &config, &request(messages));
    }

    let host = Arc::new(PagedHost::new(vec![page(
        vec![
            event(1, json!({ "type": "turn_started" })),
            event(
                2,
                json!({
                    "type": "token_usage_recorded",
                    "usage": {
                        "input_tokens": 1000,
                        "cached_input_tokens": 900,
                        "output_tokens": 50,
                        "input_accounting": "inclusive"
                    },
                    "model_context_window": 1_000_000
                }),
            ),
        ],
        "2",
        false,
    )]));

    let scan = with_host_api(host.clone(), scan_session(SESSION))
        .await
        .expect("扫描应成功");
    assert_eq!(host.requests().len(), 1);

    let snapshot = watch.snapshot(SESSION).unwrap();
    let text = report::doctor_report(
        "deepseek-v4.1-flash",
        &config.get(),
        Some(&snapshot),
        Some(&scan),
    );

    assert!(text.contains("缓存命中 90.0% · 输入 1.0K（命中 900 / 未命中 100）"), "{text}");
    assert!(text.contains("前缀：2 次请求中完整保留 1 次（100.0%）· 断点 0 次"), "{text}");
    assert!(text.contains("前缀完整、命中率 90.0%，无需处理。"), "{text}");
}
