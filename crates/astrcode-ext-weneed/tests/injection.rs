//! 通过 worker 的 `testing` 缝隙注入模拟宿主，驱动注入判定与会话开关走完整路径。
//!
//! 这里验证的是**与宿主的交互契约**：模型闸门与全局闸门是否真的拦在宿主调用之前、
//! 开关是否按会话持久化、宿主失败时的回落。纯判定逻辑由各模块的单元测试覆盖
//! （`drift` 的漂移判定、`guard` 的拦截原因、`hook` 的四个钩子、`config` 的归一化）。

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use astrcode_extension_worker::testing::{HostApi, with_host_api};
use astrcode_extension_worker::worker_prelude::{
    ErrorPayload, HostOperation, ModelSelection, PromptBuildHookInput, WireErrorCode,
};
use async_trait::async_trait;
use serde_json::{Value, json};

use astrcode_ext_weneed::config::store_at;
use astrcode_ext_weneed::hook::contribute;
use astrcode_ext_weneed::spec::WE_NEED_SPEC;
use astrcode_ext_weneed::state::SharedState;
use astrcode_ext_weneed::toggle::SessionSwitch;

/// 宿主侧 `session_state` 读写的线缆名。
const STATE_READ: &str = "astrcode.session.state.read";
const STATE_WRITE: &str = "astrcode.session.state.write";

/// 记录调用并返回预设值的模拟宿主。
///
/// `stored` 模拟宿主磁盘上的会话状态：写入会落进去，读取从里面取，
/// 因此「扩展重载后开关仍然有效」可以被真实地复现。
struct StateHost {
    stored: Mutex<Option<String>>,
    calls: Mutex<Vec<String>>,
    read_failure: Option<ErrorPayload>,
    write_failure: Option<ErrorPayload>,
}

impl StateHost {
    fn new(stored: Option<&str>) -> Self {
        Self {
            stored: Mutex::new(stored.map(str::to_owned)),
            calls: Mutex::new(Vec::new()),
            read_failure: None,
            write_failure: None,
        }
    }

    fn failing_read(error: ErrorPayload) -> Self {
        Self {
            read_failure: Some(error),
            ..Self::new(None)
        }
    }

    fn failing_write(error: ErrorPayload) -> Self {
        Self {
            write_failure: Some(error),
            ..Self::new(None)
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn stored(&self) -> Option<String> {
        self.stored.lock().unwrap().clone()
    }
}

#[async_trait]
impl HostApi for StateHost {
    fn host_supports(&self, _operation: HostOperation) -> bool {
        true
    }

    async fn call(&self, capability: &str, input: Value) -> Result<Value, ErrorPayload> {
        self.calls.lock().unwrap().push(capability.to_owned());
        match capability {
            STATE_READ => {
                if let Some(error) = &self.read_failure {
                    return Err(error.clone());
                }
                Ok(json!({ "content": self.stored.lock().unwrap().clone() }))
            }
            STATE_WRITE => {
                if let Some(error) = &self.write_failure {
                    return Err(error.clone());
                }
                *self.stored.lock().unwrap() = input["content"].as_str().map(str::to_owned);
                // 写应答是 `Acknowledgement { ok }`。
                Ok(json!({ "ok": true }))
            }
            other => Err(ErrorPayload::new(WireErrorCode::UnknownCapability, other)),
        }
    }
}

fn hook_input(model_id: &str) -> PromptBuildHookInput {
    PromptBuildHookInput {
        session_id: "session-1".to_owned(),
        working_dir: "/tmp".to_owned(),
        // 宿主组装 hook 上下文时走 `ModelSelection::simple`，provider_kind 恒为空串。
        // 这里照搬那个形状，防止实现偷偷依赖 provider_kind 而不被测试发现。
        model: ModelSelection::simple(model_id),
    }
}

/// 每个测试一份独立的配置文件路径，避免真实用户目录与用例间互相污染。
fn shared_state(name: &str) -> (Arc<SharedState>, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "astrcode-weneed-injection-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let (state, warning) = SharedState::load(store_at(dir.join("config.json")));
    assert!(warning.is_none());
    (state, dir)
}

/// 读开关：模拟宿主作用域内的 `prompt_build` 调用。
async fn inject(state: &Arc<SharedState>, host: &Arc<StateHost>, model_id: &str) -> Vec<String> {
    with_host_api(host.clone(), contribute(state, &hook_input(model_id)))
        .await
        .expect("注入不应失败")
        .system_prompts
}

/// 写开关：`session_state` 是反向调用，同样必须在宿主作用域内。
async fn set(
    state: &Arc<SharedState>,
    host: &Arc<StateHost>,
    switch: SessionSwitch,
) -> Result<(), ErrorPayload> {
    with_host_api(host.clone(), state.set_session_switch("session-1", switch)).await
}

#[tokio::test]
async fn deepseek_sessions_get_the_spec_by_default() {
    let (state, dir) = shared_state("default");
    let host = Arc::new(StateHost::new(None));

    assert_eq!(
        inject(&state, &host, "deepseek-chat").await,
        vec![WE_NEED_SPEC.to_owned()]
    );
    assert_eq!(host.calls(), vec![STATE_READ.to_owned()]);
    let _ = std::fs::remove_dir_all(dir);
}

/// 模型闸门必须拦在宿主调用之前：非 DeepSeek 会话一次 `session_state` 都不该发。
#[tokio::test]
async fn other_models_produce_no_contribution_and_touch_no_host_state() {
    let (state, dir) = shared_state("other-model");
    let host = Arc::new(StateHost::new(None));

    for model in ["gpt-4.1", "claude-sonnet-4-5", "qwen3-coder"] {
        assert!(
            inject(&state, &host, model).await.is_empty(),
            "{model} 不应收到规范"
        );
    }

    assert!(
        host.calls().is_empty(),
        "非 DeepSeek 会话不应访问宿主：{:?}",
        host.calls()
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 全局闸门同样拦在会话状态之前：全局关掉后一次 `session_state` 都不该发。
#[tokio::test]
async fn a_globally_disabled_plugin_injects_nothing_and_touches_no_host_state() {
    let (state, dir) = shared_state("global-off");
    let host = Arc::new(StateHost::new(None));

    let mut config = state.config();
    config.enabled = false;
    state.set_config(config).expect("保存应成功");

    assert!(inject(&state, &host, "deepseek-chat").await.is_empty());
    assert!(
        host.calls().is_empty(),
        "全局关闭时不应访问宿主：{:?}",
        host.calls()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn turning_the_switch_off_stops_the_injection() {
    let (state, dir) = shared_state("off");
    let host = Arc::new(StateHost::new(None));

    set(&state, &host, SessionSwitch::Off)
        .await
        .expect("写入应成功");
    assert_eq!(host.stored().as_deref(), Some("off"));
    assert!(inject(&state, &host, "deepseek-chat").await.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// 开关写在宿主的会话状态里，所以扩展重载后（新的 `SharedState`）仍然生效。
#[tokio::test]
async fn the_switch_survives_a_reload() {
    let (state, dir) = shared_state("reload");
    let host = Arc::new(StateHost::new(None));

    set(&state, &host, SessionSwitch::Off)
        .await
        .expect("写入应成功");

    let (reloaded, _) = SharedState::load(store_at(dir.join("config.json")));
    assert!(
        inject(&reloaded, &host, "deepseek-chat").await.is_empty(),
        "重载后仍应保持关闭"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 重载后重新开启也必须持久化，否则第二次重载会退回关闭。
#[tokio::test]
async fn turning_the_switch_back_on_is_also_persisted() {
    let (state, dir) = shared_state("back-on");
    let host = Arc::new(StateHost::new(Some("off")));

    set(&state, &host, SessionSwitch::On)
        .await
        .expect("写入应成功");
    assert_eq!(host.stored().as_deref(), Some("on"));
    assert_eq!(
        inject(&state, &host, "deepseek-chat").await,
        vec![WE_NEED_SPEC.to_owned()]
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 清除会话开关要写空串：读路径把「文件缺失」与「空串」都解析成未设置，
/// 因此宿主落盘的状态与内存缓存保持一致，不会在重载后「复活」成关闭。
#[tokio::test]
async fn clearing_the_switch_persists_an_empty_value() {
    let (state, dir) = shared_state("clear");
    let host = Arc::new(StateHost::new(Some("off")));

    set(&state, &host, SessionSwitch::Unset)
        .await
        .expect("写入应成功");
    assert_eq!(host.stored().as_deref(), Some(""));

    let (reloaded, _) = SharedState::load(store_at(dir.join("config.json")));
    assert_eq!(
        inject(&reloaded, &host, "deepseek-chat").await,
        vec![WE_NEED_SPEC.to_owned()],
        "清除后应回落到全局默认（开启）"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 读不到开关状态时回落到「未设置」即跟随全局，而不是让本轮 prompt 组装失败。
#[tokio::test]
async fn a_failed_state_read_falls_back_to_the_default() {
    let (state, dir) = shared_state("read-failure");
    let host = Arc::new(StateHost::failing_read(ErrorPayload::new(
        WireErrorCode::BackendUnavailable,
        "session store unavailable",
    )));

    assert_eq!(
        inject(&state, &host, "deepseek-chat").await,
        vec![WE_NEED_SPEC.to_owned()]
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// 写失败要如实上抛，但内存缓存已经更新，本次会话内开关仍然生效。
#[tokio::test]
async fn a_failed_state_write_is_reported_but_still_takes_effect_locally() {
    let (state, dir) = shared_state("write-failure");
    let host = Arc::new(StateHost::failing_write(ErrorPayload::new(
        WireErrorCode::BackendUnavailable,
        "session store unavailable",
    )));

    let error = set(&state, &host, SessionSwitch::Off)
        .await
        .expect_err("写失败应当上抛");
    assert_eq!(error.code, "backend_unavailable");

    assert!(
        inject(&state, &host, "deepseek-chat").await.is_empty(),
        "本地缓存应当已经关闭"
    );
    let _ = std::fs::remove_dir_all(dir);
}
