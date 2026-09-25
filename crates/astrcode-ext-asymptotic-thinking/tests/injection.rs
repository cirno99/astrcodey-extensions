//! 端到端：会话注册表 + 磁盘持久化 + 四个钩子 + 三个工具走完整路径。
//!
//! 这里验证的是**跨模块的契约**：开关是否真的闸住全部注入与工具、状态是否按会话隔离并
//! 落盘、`TurnStart`/`TurnEnd` 的计数与复位是否影响下一轮的引导、三个工具是否把状态机
//! 推进得动。纯逻辑（转移合法性、三级阈值、正则清洗、提示词正文）由各模块的单元测试与
//! `tests/prompts.rs` 的 golden 对照覆盖。
//!
//! 状态落盘在 `ASTRCODE_TEST_HOME` 下，因此不会碰用户真实的 `~/.astrcode`。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use astrcode_ext_asymptotic_thinking::framework_rules::FRAMEWORK_RULES;
use astrcode_ext_asymptotic_thinking::hook::{
    framework_contribution, handle_turn_end, handle_turn_start, is_enabled, provider_decision,
};
use astrcode_ext_asymptotic_thinking::state::{self, StoreRegistry};
use astrcode_ext_asymptotic_thinking::tools::{
    apply_task_info, apply_transition, status_receipt,
};
use astrcode_ext_asymptotic_thinking::types::{
    Difficulty, MasterTaskType, SubTaskType, ThinkingState,
};
use astrcode_ext_asymptotic_thinking::worker::EXTENSION_ID;
use astrcode_extension_sdk::llm::{LlmContent, LlmMessage};
use astrcode_extension_worker::worker_prelude::ProviderResult;

/// 整个测试二进制共用一个隔离根目录。
///
/// `ASTRCODE_TEST_HOME` 是进程级环境变量，只能在任何路径解析之前写一次；
/// 用例之间靠不同的 `session_id` 互不干扰。
fn test_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-asymptotic-flow-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        // SAFETY: 本测试二进制内所有路径解析都经由本函数，这里只写一次。
        unsafe { std::env::set_var("ASTRCODE_TEST_HOME", &dir) };
        dir
    })
}

/// 每个用例一个独立的会话 id，避免共享根目录下的互相污染。
fn registry() -> StoreRegistry {
    let _ = test_root();
    StoreRegistry::default()
}

/// 取出一条消息的纯文本内容。
fn message_text(message: &LlmMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            LlmContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 取出 `ProviderResult::AppendMessages` 的文本；`Allow` 返回 `None`。
fn appended_texts(result: &ProviderResult) -> Option<Vec<String>> {
    match result {
        ProviderResult::AppendMessages { messages } => {
            Some(messages.iter().map(message_text).collect())
        },
        ProviderResult::Allow => None,
        other => panic!("意料之外的 provider 结果：{other:?}"),
    }
}

/// 启用且未初始化时：prompt_build 给出静态规则，provider 追加引导。
#[tokio::test]
async fn a_fresh_session_gets_the_rules_and_the_guidance() {
    let registry = registry();
    let session = "fresh";

    assert!(is_enabled(&registry, session));
    assert_eq!(
        framework_contribution(&registry, session).system_prompts,
        vec![FRAMEWORK_RULES.to_owned()]
    );

    let texts = appended_texts(&provider_decision(&registry, session)).expect("应当追加引导");
    assert_eq!(texts.len(), 2);
    assert!(texts[0].contains("第task0轮任务启动"));
    assert!(texts[1].contains("<stateGuard>"));
}

/// 关闭后四个钩子与三个工具全部静默。
#[tokio::test]
async fn turning_the_switch_off_silences_everything() {
    let registry = registry();
    let session = "switch-off";

    // 先造出一点状态，确认关闭不会清掉它。
    apply_task_info(
        &registry,
        session,
        Difficulty::HARD,
        MasterTaskType::CODING,
        SubTaskType::RUST_DEV,
    )
    .await
    .expect("设定画像应成功");
    set_enabled(&registry, session, false).await;

    assert!(!is_enabled(&registry, session));
    assert!(framework_contribution(&registry, session).system_prompts.is_empty());
    assert!(appended_texts(&provider_decision(&registry, session)).is_none());
    assert!(apply_transition(&registry, session, ThinkingState::DEEP_UNDERSTAND)
        .await
        .expect("禁用时不应报错")
        .text
        .contains("已禁用"));
    assert!(status_receipt(&registry, session).text.contains("已禁用"));

    // 状态本身没被清掉：重新开启后仍在 START 且画像还在。
    set_enabled(&registry, session, true).await;
    let status = status_receipt(&registry, session).text;
    assert!(status.contains("启动 (START)"));
    assert!(status.contains("编程类-Rust开发 | 困难难度"));
}

/// 开关写在会话文件里，所以扩展重载（新的注册表）后仍然生效。
#[tokio::test]
async fn the_switch_survives_a_reload() {
    let first = registry();
    let session = "reload";
    set_enabled(&first, session, false).await;

    // 新的注册表 = 扩展重载：内存里的缓存全部丢掉，只剩磁盘上的会话文件。
    let reloaded = registry();
    assert!(!is_enabled(&reloaded, session));
    assert!(appended_texts(&provider_decision(&reloaded, session)).is_none());
}

/// 状态落在插件自己的数据目录下，且按会话分桶。
#[tokio::test]
async fn state_lands_under_the_plugin_directory() {
    let registry = registry();
    apply_task_info(
        &registry,
        "layout-a",
        Difficulty::SIMPLE,
        MasterTaskType::GENERAL,
        SubTaskType::GENERAL,
    )
    .await
    .expect("设定画像应成功");

    let path = state::state_path(EXTENSION_ID, "layout-a");
    assert!(
        path.starts_with(test_root().join(".astrcode").join("extension_data").join(EXTENSION_ID)),
        "状态文件位置不对：{}",
        path.display()
    );
    assert!(path.exists(), "状态文件应当已落盘：{}", path.display());
    assert!(!state::state_path(EXTENSION_ID, "layout-b").exists());
}

/// `TurnEnd` 计数并记录漏掉的流转；`TurnStart` 清空本 turn 的标记。
///
/// 轮次提醒受上游的间隔闸门限制：TRIVIAL/EXECUTE 的间隔是 12，只有
/// `stateTurnCount % 12 == 0` 的轮次才发提醒（hardStop 除外）。
#[tokio::test]
async fn turn_end_counts_and_records_a_missed_transition() {
    let registry = registry();
    let session = "turns";
    apply_task_info(
        &registry,
        session,
        Difficulty::TRIVIAL,
        MasterTaskType::CODING,
        SubTaskType::RUST_DEV,
    )
    .await
    .expect("设定画像应成功");
    apply_transition(&registry, session, ThinkingState::EXECUTE)
        .await
        .expect("TRIVIAL 可直达 EXECUTE");

    // 第一轮落在间隔之外：只有引导，没有提醒。
    handle_turn_end(&registry, session).await.expect("TurnEnd 应成功");
    assert!(status_receipt(&registry, session)
        .text
        .contains("- **状态内轮次**: 1/500"));
    assert_eq!(
        appended_texts(&provider_decision(&registry, session))
            .expect("应当追加引导")
            .len(),
        1
    );

    // 推到第 12 轮（间隔命中）：提醒出现，且因为一直没流转而带违规警告。
    for _ in 1..12 {
        handle_turn_end(&registry, session).await.expect("TurnEnd 应成功");
    }
    let texts = appended_texts(&provider_decision(&registry, session)).expect("应当追加引导");
    assert_eq!(texts.len(), 2);
    assert!(texts[1].contains("<stateGuard>"), "{:?}", texts[1]);
    assert!(texts[1].contains("<violationWarning>"), "{:?}", texts[1]);

    // 流转之后违规标记被清掉，且状态内轮次归零（0 也命中间隔）。
    apply_transition(&registry, session, ThinkingState::VERIFY)
        .await
        .expect("EXECUTE → VERIFY");
    let texts = appended_texts(&provider_decision(&registry, session)).expect("应当追加引导");
    assert_eq!(texts.len(), 2);
    assert!(!texts[1].contains("<violationWarning>"), "{:?}", texts[1]);
    assert!(status_receipt(&registry, session)
        .text
        .contains("- **状态内轮次**: 0/100"));
}

/// 轮次超限后警告进入注入文本；`hardStop` 整条替换掉 stateGuard。
#[tokio::test]
async fn the_turn_warning_shows_up_in_the_guidance() {
    let registry = registry();
    let session = "warn";
    apply_task_info(
        &registry,
        session,
        Difficulty::TRIVIAL,
        MasterTaskType::CODING,
        SubTaskType::RUST_DEV,
    )
    .await
    .expect("设定画像应成功");
    apply_transition(&registry, session, ThinkingState::EXECUTE)
        .await
        .expect("TRIVIAL 可直达 EXECUTE");

    // TRIVIAL/EXECUTE：maxTurns = 500，间隔 12。504 既超过上限又命中间隔。
    for _ in 0..504 {
        handle_turn_end(&registry, session).await.expect("TurnEnd 应成功");
    }
    let texts = appended_texts(&provider_decision(&registry, session)).expect("应当追加引导");
    assert!(texts[1].contains("<turnWarning>"), "{:?}", texts[1]);
    assert!(texts[1].contains("已超过上限"), "{:?}", texts[1]);

    // hardStop 阈值：500 + max(1, ceil(500/3)) = 667，从 668 起；672 命中间隔。
    for _ in 504..672 {
        handle_turn_end(&registry, session).await.expect("TurnEnd 应成功");
    }
    let texts = appended_texts(&provider_decision(&registry, session)).expect("应当追加引导");
    assert!(texts[1].starts_with("<task1>\n\n<hardStop>"), "{:?}", texts[1]);
    assert!(!texts[1].contains("<stateGuard>"), "{:?}", texts[1]);
}

/// END 之后的下一个 turn 自动复位成 START，并保留累计任务数。
#[tokio::test]
async fn reaching_end_resets_to_start_on_the_next_turn() {
    let registry = registry();
    let session = "end-reset";
    apply_task_info(
        &registry,
        session,
        Difficulty::TRIVIAL,
        MasterTaskType::CODING,
        SubTaskType::RUST_DEV,
    )
    .await
    .expect("设定画像应成功");
    apply_transition(&registry, session, ThinkingState::EXECUTE)
        .await
        .expect("TRIVIAL 可直达 EXECUTE");
    apply_transition(&registry, session, ThinkingState::VERIFY)
        .await
        .expect("EXECUTE → VERIFY");
    let receipt = apply_transition(&registry, session, ThinkingState::END)
        .await
        .expect("VERIFY → END");
    assert!(receipt.text.contains("上一任务已完成，保持空闲等待新指令"));

    // END 下不再计数、也不再发提醒。
    handle_turn_end(&registry, session).await.expect("TurnEnd 应成功");
    let texts = appended_texts(&provider_decision(&registry, session)).expect("应当追加引导");
    assert_eq!(texts.len(), 1, "END 不该有轮次提醒：{texts:?}");
    assert!(texts[0].contains("上一任务已完成，保持空闲等待新指令"));

    // 下一个 turn 复位成 START，任务轮次保留。
    handle_turn_start(&registry, session).await.expect("TurnStart 应成功");
    let status = status_receipt(&registry, session).text;
    assert!(status.contains("启动 (START)"), "{status}");
    assert!(status.contains("- **任务轮次**: 第 1 轮"), "{status}");
    assert!(status.contains("未设定-未设定 | 未设定难度"), "{status}");
}

/// 完整走一遍 MODERATE 任务：画像 → 四态 → END。
#[tokio::test]
async fn the_full_task_flow_walks_the_state_machine() {
    let registry = registry();
    let session = "full-flow";

    // START 未设画像时不能流转。
    let rejected = apply_transition(&registry, session, ThinkingState::DEEP_UNDERSTAND)
        .await
        .expect("不应报基础设施错误");
    assert!(rejected.is_error);
    assert!(rejected.text.contains("START 流转前必须调用"));

    apply_task_info(
        &registry,
        session,
        Difficulty::MODERATE,
        MasterTaskType::CODING,
        SubTaskType::CODE_REVIEW,
    )
    .await
    .expect("设定画像应成功");

    // MODERATE 必须先 DEEP_UNDERSTAND，不能直接跳 DESIGN。
    let rejected = apply_transition(&registry, session, ThinkingState::DESIGN)
        .await
        .expect("不应报基础设施错误");
    assert!(rejected.is_error);
    assert!(rejected.text.contains("START 状态下不能转移到 DESIGN"));

    for (from, to) in [
        (ThinkingState::START, ThinkingState::DEEP_UNDERSTAND),
        (ThinkingState::DEEP_UNDERSTAND, ThinkingState::DESIGN),
        (ThinkingState::DESIGN, ThinkingState::EXECUTE),
        (ThinkingState::EXECUTE, ThinkingState::VERIFY),
        (ThinkingState::VERIFY, ThinkingState::END),
    ] {
        let receipt = apply_transition(&registry, session, to)
            .await
            .unwrap_or_else(|error| panic!("{from} → {to} 失败：{error:?}"));
        assert!(!receipt.is_error, "{from} → {to}：{}", receipt.text);
    }

    let status = status_receipt(&registry, session).text;
    assert!(status.contains("结束 (END)"), "{status}");
    assert!(status.contains("- **任务轮次**: 第 1 轮"), "{status}");
    // 转入 END 清空任务画像。
    assert!(status.contains("未设定-未设定 | 未设定难度"), "{status}");
}

/// TRIVIAL 直达 EXECUTE 后，注入文本带路径感知适配段。
#[tokio::test]
async fn a_shortcut_path_is_reflected_in_the_guidance() {
    let registry = registry();
    let session = "shortcut";
    apply_task_info(
        &registry,
        session,
        Difficulty::TRIVIAL,
        MasterTaskType::CODING,
        SubTaskType::RUST_DEV,
    )
    .await
    .expect("设定画像应成功");
    apply_transition(&registry, session, ThinkingState::EXECUTE)
        .await
        .expect("TRIVIAL 可直达 EXECUTE");

    let texts = appended_texts(&provider_decision(&registry, session)).expect("应当追加引导");
    assert!(
        texts[0].contains("本任务直达执行模式：未经过深度理解与方案设计"),
        "{:?}",
        texts[0]
    );
    assert!(texts[0].contains("第1/500轮 执行阶段 · 编程类-Rust开发 · 微不足道"));
}

/// 两个会话的状态互不影响。
#[tokio::test]
async fn sessions_do_not_leak_into_each_other() {
    let registry = registry();
    apply_task_info(
        &registry,
        "isolated-a",
        Difficulty::HARD,
        MasterTaskType::CODING,
        SubTaskType::BUG_FIX,
    )
    .await
    .expect("设定画像应成功");

    assert!(status_receipt(&registry, "isolated-b")
        .text
        .contains("启动 (START)"));
    assert!(status_receipt(&registry, "isolated-a")
        .text
        .contains("困难难度"));
}

async fn set_enabled(registry: &StoreRegistry, session: &str, enabled: bool) {
    let store = registry.session(EXTENSION_ID, session);
    state::mutate(store, move |store| store.set_enabled(enabled))
        .await
        .expect("写开关应成功");
}
