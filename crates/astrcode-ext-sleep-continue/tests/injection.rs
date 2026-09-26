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
}

impl SleepHost {
    fn new(stored: Option<&str>) -> Self {
        Self {
            stored: Mutex::new(stored.map(str::to_owned)),
            calls: Mutex::new(Vec::new()),
            read_failure: None,
            inject_failure: None,
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
            },
            STATE_WRITE => {
                *self.stored.lock().unwrap() = input["content"].as_str().map(str::to_owned);
                // 写应答是 `Acknowledgement { ok }`。
                Ok(json!({ "ok": true }))
            },
            DEFER_CONTEXT => {
                if let Some(error) = &self.inject_failure {
                    return Err(error.clone());
                }
                // 宿主对 `defer_context` 的成功应答是投递结果；活跃 turn 下即 `Injected`。
                Ok(json!({ "status": "injected", "turn_id": "turn-1" }))
            },
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
) -> ContinueAfterStopResult {
    with_host_api(
        host.clone(),
        hook::continue_with(state, session_id, |text| async move {
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

    assert_eq!(
        continue_run(&state, &host, SESSION).await,
        ContinueAfterStopResult::ContinueOneStep
    );

    let calls = host.calls();
    assert_eq!(
        calls.iter().map(|call| call.operation.as_str()).collect::<Vec<_>>(),
        vec![STATE_READ, DEFER_CONTEXT],
        "应当「先读开关、再注入」，且只注入一次"
    );
    // 注入必须打到本会话，且正文就是配置里的续跑文本。
    assert_eq!(calls[1].input["target_session_id"], SESSION);
    assert_eq!(calls[1].input["content"], "继续");

    assert_eq!(state.progress(SESSION).continuations, 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// 没开启的会话一次注入都不能发：闸门必须拦在宿主之前。
#[tokio::test]
async fn an_unarmed_session_never_reaches_the_defer_operation() {
    let (state, dir) = shared_state("unarmed");
    let host = Arc::new(SleepHost::new(None));

    assert_eq!(
        continue_run(&state, &host, SESSION).await,
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
        continue_run(&state, &host, SESSION).await,
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
        continue_run(&state, &host, SESSION).await,
        ContinueAfterStopResult::ContinueOneStep
    );
    assert_eq!(
        continue_run(&state, &host, SESSION).await,
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
        continue_run(&state, &host, SESSION).await,
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
        continue_run(&state, &host, SESSION).await,
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
        continue_run(&reloaded, &host, SESSION).await,
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
        continue_run(&reloaded, &host, SESSION).await,
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
        continue_run(&state, &host, SESSION).await,
        ContinueAfterStopResult::EndTurn
    );

    set(&state, &host, SessionSwitch::On)
        .await
        .expect("写入应成功");
    assert_eq!(host.stored().as_deref(), Some("on"));
    assert_eq!(
        continue_run(&state, &host, SESSION).await,
        ContinueAfterStopResult::ContinueOneStep
    );
    let _ = std::fs::remove_dir_all(dir);
}
