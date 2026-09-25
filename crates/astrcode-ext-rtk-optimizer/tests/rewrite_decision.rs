//! `rtk rewrite` 退出码语义的集成测试。
//!
//! 改写决策完全由外部 `rtk` 二进制的退出码决定，因此这里用注入的 `HostApi` 模拟宿主的
//! `astrcode.process.spawn` 能力，把 `rtk rewrite` 的每种退出码都跑一遍。真实子进程的
//! 端到端验收由 `s5r-conformance` 负责。

use std::sync::{Arc, Mutex};

use astrcode_extension_worker::{
    testing::{HostApi, with_host_api},
    worker_prelude::{ErrorPayload, HostOperation, WireErrorCode},
};
use astrcode_ext_rtk_optimizer::{
    rewrite::{RewriteReason, compute_rewrite_decision},
    rtk::{RtkExecutable, parse_rtk_executable_path, probe_rtk_available, resolve_rtk_executable},
};
use async_trait::async_trait;
use serde_json::{Value, json};

/// `rtk rewrite` 的一次模拟返回：退出码、stdout、stderr。
type Response = (i32, String, String);

/// 模拟宿主子进程能力的 `HostApi`。
///
/// `responses` 按调用顺序消费；`fail_with` 非空时所有调用直接返回错误，用来覆盖
/// 「宿主拒绝 spawn」这条路径。
struct FakeHost {
    responses: Mutex<Vec<Response>>,
    calls: Mutex<Vec<Vec<String>>>,
    fail_with: Option<String>,
}

impl FakeHost {
    fn new(responses: Vec<Response>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
            fail_with: None,
        })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            fail_with: Some(message.to_owned()),
        })
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls lock").clone()
    }
}

#[async_trait]
impl HostApi for FakeHost {
    fn host_supports(&self, operation: HostOperation) -> bool {
        operation == HostOperation::ProcessSpawn
    }

    async fn call(&self, capability: &str, input: Value) -> Result<Value, ErrorPayload> {
        if capability != "astrcode.process.spawn" {
            return Err(ErrorPayload::new(WireErrorCode::UnknownCapability, capability));
        }
        if let Some(message) = &self.fail_with {
            return Err(ErrorPayload::new(WireErrorCode::ProcessFailed, message.clone()));
        }

        let mut invocation = vec![input["command"].as_str().unwrap_or_default().to_owned()];
        invocation.extend(
            input["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .filter_map(|arg| arg.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
        self.calls.lock().expect("calls lock").push(invocation);

        let (status, stdout, stderr) = {
            let mut responses = self.responses.lock().expect("responses lock");
            if responses.is_empty() {
                (1, String::new(), String::new())
            } else {
                responses.remove(0)
            }
        };

        Ok(json!({
            "status": status,
            "success": status == 0,
            "stdout": stdout,
            "stderr": stderr,
            "combined": format!("{stdout}{stderr}"),
            "stdout_truncated": false,
            "stderr_truncated": false,
            "combined_truncated": false,
        }))
    }
}

/// 指向假命令的已解析 rtk 可执行文件。
fn fake_executable() -> RtkExecutable {
    RtkExecutable {
        command: "/usr/bin/rtk".to_owned(),
        resolved_path: Some("/usr/bin/rtk".to_owned()),
        resolver: "which",
        warning: None,
    }
}

/// 把 `Arc<FakeHost>` 收窄成 `Arc<dyn HostApi>`，便于在断言里继续持有原引用。
fn as_host(host: &Arc<FakeHost>) -> Arc<dyn HostApi> {
    Arc::clone(host) as Arc<dyn HostApi>
}

async fn decide(
    host: Arc<FakeHost>,
    command: &str,
) -> astrcode_ext_rtk_optimizer::rewrite::RewriteDecision {
    let executable = fake_executable();
    with_host_api(as_host(&host), async move {
        compute_rewrite_decision(command, Some(&executable)).await
    })
    .await
}

#[tokio::test]
async fn exit_code_zero_rewrites_the_command() {
    let host = FakeHost::new(vec![(0, "rtk git status\n".to_owned(), String::new())]);
    let decision = decide(Arc::clone(&host), "git status").await;

    assert!(decision.changed);
    assert_eq!(decision.reason, RewriteReason::Rewritten);
    assert_eq!(decision.original_command, "git status");
    assert_eq!(decision.rewritten_command, "rtk git status");
    assert!(decision.warning.is_none());

    // 调用形态：`<rtk> rewrite <原命令>`。
    assert_eq!(
        host.calls(),
        vec![vec![
            "/usr/bin/rtk".to_owned(),
            "rewrite".to_owned(),
            "git status".to_owned()
        ]]
    );
}

/// `rtk rewrite` 用 3 表示「改写成功但形态不同」，与 0 同等对待。
#[tokio::test]
async fn exit_code_three_is_also_a_success() {
    let host = FakeHost::new(vec![(3, "rtk ls -la".to_owned(), String::new())]);
    let decision = decide(host, "ls -la").await;

    assert!(decision.changed);
    assert_eq!(decision.rewritten_command, "rtk ls -la");
}

#[tokio::test]
async fn exit_code_one_means_no_equivalent_command() {
    let host = FakeHost::new(vec![(1, String::new(), String::new())]);
    let decision = decide(host, "echo hi").await;

    assert!(!decision.changed);
    assert_eq!(decision.reason, RewriteReason::NoMatch);
    assert!(decision.warning.is_none());
    assert_eq!(decision.rewritten_command, "echo hi");
}

#[tokio::test]
async fn exit_code_two_reports_the_denial_reason() {
    let host = FakeHost::new(vec![(2, String::new(), "denied by policy\n".to_owned())]);
    let decision = decide(host, "rm -rf /").await;

    assert!(!decision.changed);
    assert_eq!(decision.reason, RewriteReason::NoMatch);
    assert_eq!(decision.warning.as_deref(), Some("denied by policy"));
}

#[tokio::test]
async fn exit_code_two_without_stderr_uses_a_generic_message() {
    let host = FakeHost::new(vec![(2, String::new(), String::new())]);
    let decision = decide(host, "rm -rf /").await;

    assert_eq!(decision.warning.as_deref(), Some("rtk denied rewrite"));
}

#[tokio::test]
async fn empty_stdout_on_success_is_reported_as_a_warning() {
    let host = FakeHost::new(vec![(0, "   \n".to_owned(), String::new())]);
    let decision = decide(host, "git status").await;

    assert!(!decision.changed);
    assert_eq!(decision.warning.as_deref(), Some("rtk returned empty output"));
}

#[tokio::test]
async fn an_identical_rewrite_is_not_a_change() {
    let host = FakeHost::new(vec![(0, "git status\n".to_owned(), String::new())]);
    let decision = decide(host, "git status").await;

    assert!(!decision.changed);
    assert!(decision.warning.is_none());
}

#[tokio::test]
async fn an_unexpected_exit_code_is_reported() {
    let host = FakeHost::new(vec![(7, "whatever".to_owned(), String::new())]);
    let decision = decide(host, "git status").await;

    assert!(!decision.changed);
    assert_eq!(decision.warning.as_deref(), Some("unexpected exit code 7"));
}

#[tokio::test]
async fn a_spawn_failure_is_reported_without_panicking() {
    let decision = decide(FakeHost::failing("process spawn denied"), "git status").await;

    assert!(!decision.changed);
    assert_eq!(decision.reason, RewriteReason::NoMatch);
    assert_eq!(decision.warning.as_deref(), Some("process spawn denied"));
}

/// 命令已经是 `rtk ...` 时不应产生任何宿主调用。
#[tokio::test]
async fn an_already_rtk_command_never_reaches_the_host() {
    let host = FakeHost::new(vec![]);
    let decision = decide(Arc::clone(&host), "rtk git status").await;

    assert!(!decision.changed);
    assert_eq!(decision.reason, RewriteReason::AlreadyRtk);
    assert!(host.calls().is_empty());
}

#[tokio::test]
async fn resolving_the_executable_uses_which_and_reports_the_path() {
    let host = FakeHost::new(vec![(0, "/opt/rtk/bin/rtk\n".to_owned(), String::new())]);
    let executable = with_host_api(as_host(&host), async { resolve_rtk_executable().await }).await;

    assert_eq!(executable.command, "/opt/rtk/bin/rtk");
    assert_eq!(executable.resolved_path.as_deref(), Some("/opt/rtk/bin/rtk"));
    assert_eq!(executable.resolver, "which");
    assert!(executable.warning.is_none());
    assert_eq!(host.calls(), vec![vec!["which".to_owned(), "rtk".to_owned()]]);
}

#[tokio::test]
async fn a_failed_resolution_falls_back_to_the_bare_command() {
    let host = FakeHost::new(vec![(1, String::new(), "not found\n".to_owned())]);
    let executable = with_host_api(as_host(&host), async { resolve_rtk_executable().await }).await;

    assert_eq!(executable.command, "rtk");
    assert!(executable.resolved_path.is_none());
    let warning = executable.warning.expect("fallback must carry a warning");
    assert!(warning.contains("which failed"), "{warning}");
    assert!(warning.contains("not found"), "{warning}");
}

#[tokio::test]
async fn probing_availability_reads_the_version_exit_code() {
    let host = FakeHost::new(vec![(0, "rtk 0.50.0\n".to_owned(), String::new())]);
    let executable = fake_executable();
    let (available, error) =
        with_host_api(as_host(&host), async move { probe_rtk_available(&executable).await }).await;

    assert!(available);
    assert!(error.is_none());
    assert_eq!(
        host.calls(),
        vec![vec![
            "/usr/bin/rtk".to_owned(),
            "--version".to_owned()
        ]]
    );
}

#[tokio::test]
async fn probing_reports_the_reason_when_the_binary_fails() {
    let host = FakeHost::new(vec![(127, String::new(), "no such file\n".to_owned())]);
    let executable = fake_executable();
    let (available, error) =
        with_host_api(as_host(&host), async move { probe_rtk_available(&executable).await }).await;

    assert!(!available);
    assert_eq!(error.as_deref(), Some("no such file"));
}

#[test]
fn resolver_output_parsing_is_shared_with_the_unit_tests() {
    assert_eq!(
        parse_rtk_executable_path("\n/usr/local/bin/rtk\n").as_deref(),
        Some("/usr/local/bin/rtk")
    );
}
