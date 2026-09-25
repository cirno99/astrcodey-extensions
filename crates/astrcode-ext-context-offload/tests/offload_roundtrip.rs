//! 端到端：`post_tool_use` 换出 → `retrieve` 取回。
//!
//! 单元测试各自覆盖决策与渲染；这里验证两者对**同一份落盘数据**闭环，并确认
//! 存储可注入（真实路径依赖宿主数据目录，测试必须能指向临时目录）。

use std::fs;

use astrcode_extension_sdk::s5r::hooks::PostToolUseHookInput;
use astrcode_extension_worker::worker_prelude::PostToolUseResult;

use astrcode_ext_context_offload::{
    hook,
    offload::OffloadPolicy,
    store::OffloadStore,
    tool::{self, RetrieveArgs},
};

const SESSION: &str = "session-1";
const EXTENSION: &str = "astrcode-context-offload";

fn input(tool_name: &str, content: &str, is_error: bool) -> PostToolUseHookInput {
    serde_json::from_value(serde_json::json!({
        "session_id": SESSION,
        "working_dir": "/workspace",
        "model": { "profile_name": "default", "model": "m", "provider_kind": "openai" },
        "tool_call_id": "call-1",
        "tool_name": tool_name,
        "tool_input": {},
        "tool_result": { "content": content, "is_error": is_error, "metadata": {} },
        "is_error": is_error
    }))
    .expect("测试用的 hook 载荷必须与线缆形状一致")
}

fn temp_store(name: &str) -> OffloadStore {
    let root = std::env::temp_dir().join(format!(
        "astrcode-offload-e2e-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    OffloadStore::under(root, EXTENSION, SESSION)
}

fn retrieve_args(reference: &str) -> RetrieveArgs {
    RetrieveArgs {
        reference: reference.into(),
        offset: 0,
        limit: None,
    }
}

#[tokio::test]
async fn oversized_output_round_trips_through_offload_and_retrieve() {
    let store = temp_store("round-trip");
    let content = "npm run build\n".to_string() + &"x".repeat(9_000);

    let outcome = hook::offload(
        input("shell", &content, false),
        &store,
        OffloadPolicy::default(),
    )
    .await
    .expect("offload 应成功");

    let PostToolUseResult::ModifyResult {
        content: placeholder,
    } = outcome
    else {
        panic!("超阈值输出必须被替换为占位符");
    };
    assert!(placeholder.starts_with("📦 [offload #call-1 · shell · 9.0K chars]"));
    assert!(placeholder.contains("npm run build"));

    // 占位符本身必须远小于原文，否则这次替换没有意义。
    assert!(placeholder.chars().count() < 400);

    // 原文确实落盘，且 retrieve 能按页取回（原文超过一页，首页应标注续页偏移）。
    let retrieval = tool::retrieve(retrieve_args("call-1"), &store).expect("retrieve 应成功");
    assert!(!retrieval.is_error);
    let expected_head: String = content.chars().take(tool::DEFAULT_PAGE_CHARS).collect();
    assert!(retrieval.text.starts_with(&expected_head));
    assert!(retrieval.text.contains("next offset="));

    let _ = fs::remove_dir_all(store.dir());
}

#[tokio::test]
async fn retrieve_pages_output_that_exceeds_one_page() {
    let store = temp_store("paging");
    let content = "y".repeat(tool::DEFAULT_PAGE_CHARS + 500);

    hook::offload(
        input("shell", &content, false),
        &store,
        OffloadPolicy::default(),
    )
    .await
    .expect("offload 应成功");

    let first = tool::retrieve(retrieve_args("call-1"), &store).expect("首页应成功");
    assert!(first.text.contains("next offset=8000"));

    let second = tool::retrieve(
        RetrieveArgs {
            reference: "call-1".into(),
            offset: tool::DEFAULT_PAGE_CHARS,
            limit: None,
        },
        &store,
    )
    .expect("次页应成功");
    assert!(second.text.contains("· complete]"));

    // 两页拼起来就是原文：分页不能丢字符，也不能重复。
    let page_one: String = first.text.chars().take(tool::DEFAULT_PAGE_CHARS).collect();
    let page_two: String = second.text.chars().take(500).collect();
    assert_eq!(page_one.len() + page_two.len(), content.len());

    let _ = fs::remove_dir_all(store.dir());
}

#[tokio::test]
async fn small_output_is_left_untouched() {
    let store = temp_store("small");

    let outcome = hook::offload(
        input("shell", "short output", false),
        &store,
        OffloadPolicy::default(),
    )
    .await
    .expect("offload 应成功");

    assert!(matches!(outcome, PostToolUseResult::Allow));
    assert!(
        store.entries().expect("列目录应成功").is_empty(),
        "未换出的输出不应产生任何落盘文件"
    );
}

#[tokio::test]
async fn retrieve_output_is_never_offloaded_again() {
    let store = temp_store("self-call");
    // 取回的原文可以合法地超过阈值；若再被换出，模型会陷入 retrieve 死循环。
    let content = "z".repeat(30_000);

    let outcome = hook::offload(
        input(tool::TOOL_NAME, &content, false),
        &store,
        OffloadPolicy::default(),
    )
    .await
    .expect("offload 应成功");

    assert!(matches!(outcome, PostToolUseResult::Allow));
    assert!(store.entries().expect("列目录应成功").is_empty());
}

#[tokio::test]
async fn unknown_reference_reports_an_error_without_failing_the_call() {
    let store = temp_store("unknown-ref");

    let retrieval = tool::retrieve(retrieve_args("never-written"), &store)
        .expect("未知 ref 不应让工具调用失败");

    assert!(retrieval.is_error);
    assert!(retrieval.text.contains("never-written"));
}

#[tokio::test]
async fn error_results_are_offloaded_with_an_error_mark() {
    let store = temp_store("error-result");
    let content = "e".repeat(9_000);

    let outcome = hook::offload(
        input("shell", &content, true),
        &store,
        OffloadPolicy::default(),
    )
    .await
    .expect("offload 应成功");

    let PostToolUseResult::ModifyResult {
        content: placeholder,
    } = outcome
    else {
        panic!("超阈值的错误输出同样应被换出");
    };
    assert!(placeholder.contains("shell · error · "));

    let _ = fs::remove_dir_all(store.dir());
}
