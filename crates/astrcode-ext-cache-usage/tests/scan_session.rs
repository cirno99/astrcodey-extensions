//! 通过 worker 的 `testing` 缝隙注入模拟宿主，驱动 `scan_session` 走完整分页路径。
//!
//! 这里验证的是**与宿主的交互契约**：游标推进、分页终止、能力名、错误传播。
//! 纯折叠逻辑由 `src/scan.rs` 与 `astrcode-ext-common` 的单元测试覆盖。

use std::sync::{Arc, Mutex};

use astrcode_extension_worker::testing::{HostApi, with_host_api};
use astrcode_extension_worker::worker_prelude::{ErrorPayload, HostOperation, WireErrorCode};
use async_trait::async_trait;
use serde_json::{Value, json};

use astrcode_ext_cache_usage::scan::scan_session;

/// 宿主侧 `astrcode.session.read_events` 的线缆名。
const READ_EVENTS: &str = "astrcode.session.read_events";

/// 按顺序吐预设页面的模拟宿主。
struct PagedHost {
    pages: Mutex<Vec<Value>>,
    requests: Mutex<Vec<Value>>,
    failure: Option<ErrorPayload>,
}

impl PagedHost {
    fn new(pages: Vec<Value>) -> Self {
        Self {
            pages: Mutex::new(pages),
            requests: Mutex::new(Vec::new()),
            failure: None,
        }
    }

    fn failing(error: ErrorPayload) -> Self {
        Self {
            pages: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            failure: Some(error),
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
            return Err(ErrorPayload::new(
                WireErrorCode::UnknownCapability,
                capability,
            ));
        }
        self.requests.lock().unwrap().push(input);
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
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
        "session_id": "session-1",
        "timestamp": "2026-09-25T00:00:00Z",
        "payload": payload,
    })
}

fn usage_payload(usage: Value, window: u64) -> Value {
    json!({
        "type": "token_usage_recorded",
        "usage": usage,
        "model_context_window": window,
    })
}

fn page(events: Vec<Value>, next_cursor: &str, has_more: bool) -> Value {
    json!({ "events": events, "next_cursor": next_cursor, "has_more": has_more })
}

#[tokio::test]
async fn scan_advances_the_cursor_and_folds_both_accounting_styles() {
    let host = Arc::new(PagedHost::new(vec![
        page(
            vec![
                event(1, json!({ "type": "user_message", "text": "hi" })),
                event(
                    2,
                    usage_payload(
                        json!({
                            "input_tokens": 100,
                            "cached_input_tokens": 80,
                            "input_accounting": "inclusive"
                        }),
                        1_000_000,
                    ),
                ),
            ],
            "2",
            true,
        ),
        page(
            vec![event(
                3,
                usage_payload(
                    json!({
                        "input_tokens": 20,
                        "cached_input_tokens": 70,
                        "cache_creation_input_tokens": 10,
                        "input_accounting": "components"
                    }),
                    200_000,
                ),
            )],
            "3",
            false,
        ),
    ]));

    let outcome = with_host_api(host.clone(), scan_session("session-1"))
        .await
        .expect("扫描应成功");

    assert_eq!(outcome.events_seen, 3, "非用量事件也要计入已扫描条数");
    assert_eq!(outcome.totals.requests, 2);
    assert_eq!(outcome.totals.prompt_tokens, 200);
    assert_eq!(outcome.totals.cached_tokens, 150);
    assert_eq!(outcome.totals.uncached_tokens(), 50);
    assert_eq!(outcome.totals.hit_rate(), Some(0.75));
    assert_eq!(outcome.totals.context_window, 1_000_000);

    let requests = host.requests();
    assert_eq!(requests.len(), 2, "两页应产生两次宿主调用");
    assert_eq!(requests[0]["session_id"], "session-1");
    assert_eq!(requests[0]["cursor"], Value::Null, "首页不带游标");
    assert_eq!(requests[0]["limit"], 500, "单页上限应对齐宿主上限");
    assert_eq!(requests[1]["cursor"], "2", "次页应带上首页返回的游标");
}

#[tokio::test]
async fn scan_stops_on_a_single_page() {
    let host = Arc::new(PagedHost::new(vec![page(
        vec![event(
            1,
            usage_payload(
                json!({ "input_tokens": 10, "cached_input_tokens": 5 }),
                4_000,
            ),
        )],
        "1",
        false,
    )]));

    let outcome = with_host_api(host.clone(), scan_session("session-1"))
        .await
        .expect("扫描应成功");

    assert_eq!(outcome.events_seen, 1);
    assert_eq!(outcome.totals.hit_rate(), Some(0.5));
    assert_eq!(host.requests().len(), 1);
}

#[tokio::test]
async fn scan_stops_when_the_host_claims_more_pages_but_returns_none() {
    // 宿主标记 has_more 却返回空页：必须终止，否则会无进展地死循环。
    let host = Arc::new(PagedHost::new(vec![page(vec![], "0", true)]));

    let outcome = with_host_api(host.clone(), scan_session("session-1"))
        .await
        .expect("扫描应成功");

    assert_eq!(outcome.events_seen, 0);
    assert_eq!(outcome.totals.requests, 0);
    assert_eq!(host.requests().len(), 1);
}

#[tokio::test]
async fn scan_reports_an_empty_session() {
    let host = Arc::new(PagedHost::new(vec![page(vec![], "0", false)]));

    let outcome = with_host_api(host.clone(), scan_session("session-1"))
        .await
        .expect("扫描应成功");

    assert_eq!(outcome.events_seen, 0);
    assert_eq!(outcome.totals.hit_rate(), None);
}

#[tokio::test]
async fn scan_propagates_host_errors() {
    let host = Arc::new(PagedHost::failing(ErrorPayload::new(
        WireErrorCode::ContextUnavailable,
        "no session context",
    )));

    let error = with_host_api(host, scan_session("session-1"))
        .await
        .expect_err("宿主报错应向上传播");

    assert_eq!(error.code_enum(), Some(WireErrorCode::ContextUnavailable));
}
