//! 通过 worker 的 `testing` 缝隙注入模拟宿主，驱动续跑注入与会话开关走完整路径。
//!
//! 这里验证的是**与宿主的交互契约**：闸门是否真的拦在宿主调用之前、注入打到哪个操作、
//! 载荷长什么样、宿主失败时怎么回落。纯判定逻辑由各模块的单元测试覆盖（`plan` 的决策与
//! 摘要、`config` 的归一化、`state` 的结算、`hook` 的闸门）。
//!
//! 注入通道这里用的是**真实的** `HostClient::session_control()`，只把最底下的传输换成
//! 模拟宿主，因此操作名与请求 DTO 的拼装也在验收范围内。

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use astrcode_extension_worker::testing::{HostApi, with_host_api};
use astrcode_extension_worker::worker_prelude::{
    ContinueAfterStopResult, ErrorPayload, HostClient, HostOperation, HostSessionInputRequest,
    WireErrorCode,
};
use async_trait::async_trait;
use serde_json::{Value, json};

use astrcode_ext_sleep_continue::config::store_at;
use astrcode_ext_sleep_continue::hook;
use astrcode_ext_sleep_continue::plan::StopReason;
use astrcode_ext_sleep_continue::state::{SessionSwitch, SharedState};

/// 宿主侧三个操作的线缆名。
const STATE_READ: &str = "astrcode.session.state.read";
const STATE_WRITE: &str = "astrcode.session.state.write";
const DEFER_CONTEXT: &str = "astrcode.session.control.defer_context";
const READ_EVENTS: &str = "astrcode.session.read_events";
const QUEUE_OR_START: &str = "astrcode.session.control.queue_or_start";

const SESSION: &str = "session-1";

/// 一次宿主调用：能力名 + 入参。
///
/// 入参一起记下来，是为了断言「载荷长什么样」——只记能力名的话，注入了错文本或错会话
/// 都发现不了。
#[derive(Clone, Debug)]
struct Call {
    operation: String,
    input: Value,
}

/// 记录调用并返回预设值的模拟宿主。
///
/// `stored` 模拟宿主磁盘上的会话状态：写入会落进去，读取从里面取，因此「扩展重载后开关
/// 仍然有效」可以被真实地复现。
struct SleepHost {
    stored: Mutex<Option<String>>,
    calls: Mutex<Vec<Call>>,
    read_failure: Option<ErrorPayload>,
    inject_failure: Option<ErrorPayload>,
    /// 持久事件日志里已有的事件，按 `seq` 升序。
    events: Mutex<Vec<Value>>,
    /// 重试投递（`queue_or_start`）的失败注入。
    queue_failure: Option<ErrorPayload>,
}

impl SleepHost {
    fn new(stored: Option<&str>) -> Self {
        Self {
            stored: Mutex::new(stored.map(str::to_owned)),
            calls: Mutex::new(Vec::new()),
            read_failure: None,
            inject_failure: None,
            events: Mutex::new(Vec::new()),
            queue_failure: None,
        }
    }

    fn failing_read(error: ErrorPayload) -> Self {
        Self {
            read_failure: Some(error),
            ..Self::new(None)
        }
    }

    fn failing_inject(error: ErrorPayload) -> Self {
        Self {
            inject_failure: Some(error),
            ..Self::new(Some("on"))
        }
    }

    /// 预置持久事件日志。
    fn with_events(mut self, events: Vec<Value>) -> Self {
        self.events = Mutex::new(events);
        self
    }

    /// 追加一条持久事件：模拟「这一轮又失败了」。
    fn push_event(&self, event: Value) {
        self.events.lock().unwrap().push(event);
    }

    /// 注入重试投递的失败。
    fn failing_queue(error: ErrorPayload) -> Self {
        Self {
            queue_failure: Some(error),
            ..Self::new(Some("on"))
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// 只取某一类操作的调用次数：闸门测试关心的是「有没有打到那个操作」。
    fn call_count(&self, operation: &str) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.operation == operation)
            .count()
    }

    fn stored(&self) -> Option<String> {
        self.stored.lock().unwrap().clone()
    }
}

#[async_trait]
impl HostApi for SleepHost {
    fn host_supports(&self, _operation: HostOperation) -> bool {
        true
    }

    async fn call(&self, capability: &str, input: Value) -> Result<Value, ErrorPayload> {
        self.calls.lock().unwrap().push(Call {
            operation: capability.to_owned(),
            input: input.clone(),
        });
        match capability {
            STATE_READ => {
                if let Some(error) = &self.read_failure {
                    return Err(error.clone());
                }
                Ok(json!({ "content": self.stored.lock().unwrap().clone() }))
            }
            STATE_WRITE => {
                *self.stored.lock().unwrap() = input["content"].as_str().map(str::to_owned);
                // 写应答是 `Acknowledgement { ok }`。
                Ok(json!({ "ok": true }))
            }
            DEFER_CONTEXT => {
                if let Some(error) = &self.inject_failure {
                    return Err(error.clone());
                }
                // 宿主对 `defer_context` 的成功应答是投递结果；活跃 turn 下即 `Injected`。
                Ok(json!({ "status": "injected", "turn_id": "turn-1" }))
            }
            READ_EVENTS => {
                // 游标语义与宿主一致：只回 `seq` 严格大于游标的事件，游标缺省为 0。
                let cursor = input["cursor"]
                    .as_str()
                    .and_then(|cursor| cursor.parse::<u64>().ok())
                    .unwrap_or(0);
                let events: Vec<Value> = self
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|event| event["seq"].as_u64().unwrap_or(0) > cursor)
                    .cloned()
                    .collect();
                let next_cursor = events
                    .last()
                    .map_or_else(|| cursor.to_string(), |event| event["seq"].to_string());
                Ok(json!({
                    "events": events,
                    "next_cursor": next_cursor,
                    "has_more": false
                }))
            }
            QUEUE_OR_START => {
                if let Some(error) = &self.queue_failure {
                    return Err(error.clone());
                }
                // 活跃 turn 下排队；应答是投递结果 `Queued`。
                Ok(json!({ "status": "queued", "queue_len": 1 }))
            }
            other => Err(ErrorPayload::new(WireErrorCode::UnknownCapability, other)),
        }
    }
}

/// 每个测试一份独立的配置文件路径，避免真实用户目录与用例间互相污染。
fn shared_state(name: &str) -> (Arc<SharedState>, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "astrcode-sleep-continue-injection-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let (state, warning) = SharedState::load(store_at(dir.join("config.json")));
    assert!(warning.is_none());
    (state, dir)
}

/// 在宿主作用域内跑一次续跑，注入走真实的 `HostClient::session_control()`。
async fn continue_run(
    state: &Arc<SharedState>,
    host: &Arc<SleepHost>,
    session_id: &str,
    assistant_text: &str,
) -> ContinueAfterStopResult {
    with_host_api(
        host.clone(),
        hook::continue_with(state, session_id, assistant_text, |text| async move {
            HostClient::session_control()
                .defer_context(HostSessionInputRequest {
                    target_session_id: SESSION.to_owned(),
                    content: text,
                })
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }),
    )
    .await
    .expect("续跑判定不应失败")
}

/// 写会话开关同样要在宿主作用域内。
async fn set(
    state: &Arc<SharedState>,
    host: &Arc<SleepHost>,
    switch: SessionSwitch,
) -> Result<(), ErrorPayload> {
    with_host_api(host.clone(), state.set_session_switch(SESSION, switch)).await
}

#[tokio::test]
async fn an_armed_session_continues_and_injects_the_configured_text() {
    let (state, dir) = shared_state("armed");
    let host = Arc::new(SleepHost::new(Some("on")));
    // 这一步干过活（有工具调用），所以续跑喂的是配置的「继续」。
    state.note_tool_call(SESSION);
    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::ContinueOneStep
    );

    let calls = host.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.operation.as_str())
            .collect::<Vec<_>>(),
        vec![STATE_READ, DEFER_CONTEXT],
        "应当「先读开关、再注入」，且只注入一次"
    );
    // 注入必须打到本会话，且正文就是配置里的续跑文本。
    assert_eq!(calls[1].input["target_session_id"], SESSION);
    assert_eq!(calls[1].input["content"], "继续");

    assert_eq!(state.progress(SESSION).continuations, 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// 上一步没有工具调用时改喂纠正提示：对复读的模型再说一次「继续」等于给它同一张牌。
#[tokio::test]
async fn a_step_without_tool_calls_injects_the_nudge_text() {
    let (state, dir) = shared_state("nudge");
    let host = Arc::new(SleepHost::new(Some("on")));

    assert_eq!(
        continue_run(&state, &host, SESSION, "我打算先看看 a.rs。").await,
        ContinueAfterStopResult::ContinueOneStep
    );

    let calls = host.calls();
    assert_eq!(calls[1].operation, DEFER_CONTEXT);
    assert_eq!(
        calls[1].input["content"],
        astrcode_ext_sleep_continue::config::DEFAULT_NUDGE_TEXT
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 没开启的会话一次注入都不能发：闸门必须拦在宿主之前。
#[tokio::test]
async fn an_unarmed_session_never_reaches_the_defer_operation() {
    let (state, dir) = shared_state("unarmed");
    let host = Arc::new(SleepHost::new(None));

    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::EndTurn
    );
    assert_eq!(
        host.call_count(DEFER_CONTEXT),
        0,
        "未开启的会话不应注入：{:?}",
        host.calls()
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 全局总开关关掉后同样拦在宿主之前。
#[tokio::test]
async fn a_globally_disabled_plugin_never_reaches_the_defer_operation() {
    let (state, dir) = shared_state("global-off");
    let host = Arc::new(SleepHost::new(Some("on")));

    let mut config = state.config();
    config.enabled = false;
    state.set_config(config).expect("保存应成功");

    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::EndTurn
    );
    assert_eq!(host.call_count(DEFER_CONTEXT), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// 到顶之后只停止续跑，不再打宿主；原因留给 `/sleep status`。
#[tokio::test]
async fn the_cap_stops_without_calling_the_host_again() {
    let (state, dir) = shared_state("cap");
    let host = Arc::new(SleepHost::new(Some("on")));

    let mut config = state.config();
    config.max = 1;
    state.set_config(config).expect("保存应成功");

    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::ContinueOneStep
    );
    assert_eq!(
        continue_run(&state, &host, SESSION, "看了 b.rs。").await,
        ContinueAfterStopResult::EndTurn
    );

    assert_eq!(host.call_count(DEFER_CONTEXT), 1, "到顶后不应再注入");
    assert_eq!(
        state.progress(SESSION).stop_reason,
        Some(StopReason::CapReached { count: 1, max: 1 })
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 注入失败就停下：只多跑一个 step 而不喂话，模型会原地复读。
#[tokio::test]
async fn a_failed_injection_stops_and_records_the_reason() {
    let (state, dir) = shared_state("inject-failure");
    let host = Arc::new(SleepHost::failing_inject(ErrorPayload::new(
        WireErrorCode::InvalidInput,
        "no active turn",
    )));

    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::EndTurn
    );
    // 失败不算续跑：计数不能加，否则失败会被当成消耗掉的预算。
    assert_eq!(state.progress(SESSION).continuations, 0);
    let reason = state.progress(SESSION).stop_reason.expect("应当记下原因");
    assert!(
        matches!(&reason, StopReason::InjectFailed { detail } if detail.contains("no active turn")),
        "{reason:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 读不到开关状态时回落「未开启」：判定失败不该把会话打开。
#[tokio::test]
async fn a_host_read_failure_leaves_the_session_disabled() {
    let (state, dir) = shared_state("read-failure");
    let host = Arc::new(SleepHost::failing_read(ErrorPayload::new(
        WireErrorCode::BackendUnavailable,
        "session state unavailable",
    )));

    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::EndTurn
    );
    assert_eq!(host.call_count(DEFER_CONTEXT), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// 开关写在宿主的会话状态里，所以扩展重载后（新的 `SharedState`）仍然生效。
#[tokio::test]
async fn the_switch_survives_a_reload() {
    let (state, dir) = shared_state("reload");
    let host = Arc::new(SleepHost::new(None));

    set(&state, &host, SessionSwitch::On)
        .await
        .expect("写入应成功");
    assert_eq!(host.stored().as_deref(), Some("on"));

    let (reloaded, _) = SharedState::load(store_at(dir.join("config.json")));
    assert_eq!(
        continue_run(&reloaded, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::ContinueOneStep,
        "重载后开关仍应有效"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 清除会话开关要写空串：读路径把「文件缺失」与「空串」都解析成未设置，
/// 因此宿主落盘的状态与内存缓存保持一致，不会在重载后「复活」成开启。
///
/// 注意语义与全局开关相反：`Unset` 在这里意味着**不续跑**（续跑必须显式开启），
/// 所以清除之后会话是关的，而不是回落到「跟着全局开」。
#[tokio::test]
async fn clearing_the_switch_persists_an_empty_value_and_stays_disabled() {
    let (state, dir) = shared_state("clear");
    let host = Arc::new(SleepHost::new(Some("on")));

    set(&state, &host, SessionSwitch::Unset)
        .await
        .expect("写入应成功");
    assert_eq!(host.stored().as_deref(), Some(""));

    let (reloaded, _) = SharedState::load(store_at(dir.join("config.json")));
    assert_eq!(
        continue_run(&reloaded, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::EndTurn,
        "清除开关后应停止续跑"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 关闭后再打开必须重新落盘，否则第二次重载会退回关闭。
#[tokio::test]
async fn turning_the_switch_back_on_is_also_persisted() {
    let (state, dir) = shared_state("back-on");
    let host = Arc::new(SleepHost::new(Some("on")));

    set(&state, &host, SessionSwitch::Off)
        .await
        .expect("写入应成功");
    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::EndTurn
    );

    set(&state, &host, SessionSwitch::On)
        .await
        .expect("写入应成功");
    assert_eq!(host.stored().as_deref(), Some("on"));
    assert_eq!(
        continue_run(&state, &host, SESSION, "改完了 a.rs。").await,
        ContinueAfterStopResult::ContinueOneStep
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ─── turn 失败后的自动重试 ─────────────────────────────────────────────────
//
// `turn_end` 钩子的载荷里没有错误信息，所以插件只能自己去读持久事件日志。这些用例验收的是
// 「读到的错误怎么用」：投递打到哪个操作、载荷长什么样、什么时候不投递。

/// 一条持久错误事件，形状与宿主 `session.read_events` 返回的一致。
fn error_event(seq: u64, message: &str) -> Value {
    json!({
        "seq": seq,
        "id": format!("evt-{seq}"),
        "session_id": SESSION,
        "timestamp": "2026-01-01T00:00:00Z",
        "payload": {
            "type": "error_occurred",
            "code": 0,
            "message": message,
            "recoverable": false
        }
    })
}

/// 一条非错误事件：用来表示「错误后面还写着别的东西」。
fn step_event(seq: u64) -> Value {
    json!({
        "seq": seq,
        "id": format!("evt-{seq}"),
        "session_id": SESSION,
        "timestamp": "2026-01-01T00:00:00Z",
        "payload": { "type": "step_completed", "step_index": 0 }
    })
}

/// 退避压到 1 毫秒：用例不该为了验证「有界退避」真睡上几秒。
fn fast_retries(state: &Arc<SharedState>) {
    let mut config = state.config();
    config.retry_base_delay_ms = 1;
    config.retry_max_delay_ms = 1;
    state.set_config(config).expect("保存应成功");
}

async fn recover(state: &Arc<SharedState>, host: &Arc<SleepHost>) {
    with_host_api(host.clone(), hook::recover_failed_turn(state, SESSION)).await;
}

/// 日志末尾就是错误 → 这一轮刚挂掉：自动重试，把续跑文本排进宿主队列。
#[tokio::test]
async fn a_transient_failure_at_the_log_tail_is_retried() {
    let (state, dir) = shared_state("retry-transient");
    let host = Arc::new(SleepHost::new(Some("on")).with_events(vec![error_event(
        1,
        "transport error: read streaming response body failed for \
         https://api.r4.codes/v1/chat/completions: status=200, bytes-read=1329670: \
         error decoding response body; Connection timed out (os error 110)",
    )]));
    fast_retries(&state);

    recover(&state, &host).await;

    let calls = host.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.operation.as_str())
            .collect::<Vec<_>>(),
        vec![STATE_READ, READ_EVENTS, QUEUE_OR_START],
        "应当「先读事件、再排队」，且只排一次"
    );
    let queued = calls
        .iter()
        .find(|call| call.operation == QUEUE_OR_START)
        .expect("应当有排队投递");
    assert_eq!(queued.input["target_session_id"], SESSION);
    assert_eq!(queued.input["content"], "继续");
    assert_eq!(state.progress(SESSION).retries, 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// 历史错误（后面还写着别的事件）不重试：扩展重载后不该把旧账翻出来重跑一遍。
#[tokio::test]
async fn a_historical_error_is_not_retried() {
    let (state, dir) = shared_state("retry-history");
    let host = Arc::new(SleepHost::new(Some("on")).with_events(vec![
        error_event(
            1,
            "transport error: connection reset by peer (os error 104)",
        ),
        step_event(2),
    ]));
    fast_retries(&state);

    recover(&state, &host).await;
    recover(&state, &host).await;

    assert_eq!(host.call_count(QUEUE_OR_START), 0, "{:?}", host.calls());
    let _ = std::fs::remove_dir_all(dir);
}

/// 致命错误（模型不存在、鉴权失败）不重试，只把原因留下。
#[tokio::test]
async fn a_fatal_failure_is_not_retried() {
    let (state, dir) = shared_state("retry-fatal");
    let host = Arc::new(SleepHost::new(Some("on")).with_events(vec![error_event(
        1,
        "model not found (404): 404 page not found",
    )]));
    fast_retries(&state);

    recover(&state, &host).await;

    assert_eq!(host.call_count(QUEUE_OR_START), 0);
    let reason = state.progress(SESSION).stop_reason.expect("应当记下原因");
    assert!(
        matches!(reason, StopReason::FatalError { .. }),
        "{reason:?}"
    );
    assert_eq!(state.progress(SESSION).retries, 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// 重试有界：预算用完后再失败也不投递。
#[tokio::test]
async fn the_retry_budget_is_bounded() {
    let (state, dir) = shared_state("retry-bound");
    let host = Arc::new(SleepHost::new(Some("on")).with_events(vec![error_event(
        1,
        "The stream was terminated by the server. Please retry.",
    )]));
    fast_retries(&state);
    let mut config = state.config();
    config.retry_max = 1;
    state.set_config(config).expect("保存应成功");

    recover(&state, &host).await;
    assert_eq!(host.call_count(QUEUE_OR_START), 1);

    // 又挂了一次：游标之后确实是新错误，但预算已经用完。
    host.push_event(error_event(2, "service unavailable (503)"));
    recover(&state, &host).await;

    assert_eq!(host.call_count(QUEUE_OR_START), 1, "到顶后不应再排队");
    assert_eq!(
        state.progress(SESSION).stop_reason,
        Some(StopReason::RetryExhausted { count: 1, max: 1 })
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// `retryMax = 0` 时连事件日志都不读：闸门必须拦在宿主之前。
#[tokio::test]
async fn retries_disabled_never_reach_the_host() {
    let (state, dir) = shared_state("retry-off");
    let host =
        Arc::new(SleepHost::new(Some("on")).with_events(vec![error_event(1, "transport error")]));
    let mut config = state.config();
    config.retry_max = 0;
    state.set_config(config).expect("保存应成功");

    recover(&state, &host).await;

    assert_eq!(host.calls().len(), 0, "{:?}", host.calls());
    let _ = std::fs::remove_dir_all(dir);
}

/// 会话没开启时同样不碰宿主。
#[tokio::test]
async fn a_closed_session_never_retries() {
    let (state, dir) = shared_state("retry-closed");
    let host = Arc::new(SleepHost::new(None).with_events(vec![error_event(1, "transport error")]));
    fast_retries(&state);

    recover(&state, &host).await;

    // 闸门读一次会话开关，但事件日志与投递一次都不该碰。
    assert_eq!(
        host.calls()
            .iter()
            .map(|call| call.operation.as_str())
            .collect::<Vec<_>>(),
        vec![STATE_READ],
        "未开启的会话不应读事件、更不该重试"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 重试投递失败也要留下原因，而不是静默地什么都不做。
#[tokio::test]
async fn a_failed_retry_delivery_is_recorded() {
    let (state, dir) = shared_state("retry-delivery-failure");
    let host = Arc::new(
        SleepHost::failing_queue(ErrorPayload::new(
            WireErrorCode::BackendUnavailable,
            "no active turn",
        ))
        .with_events(vec![error_event(1, "transport error")]),
    );
    fast_retries(&state);

    recover(&state, &host).await;

    let reason = state.progress(SESSION).stop_reason.expect("应当记下原因");
    assert!(
        matches!(&reason, StopReason::RetryFailed { detail } if detail.contains("no active turn")),
        "{reason:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
