//! 四个钩子的实现与纯逻辑。
//!
//! # 注入路径
//!
//! 上游分两处注入：`before_agent_start` 把引导作为 durable 的隐藏消息塞进历史，
//! `turn_end` 再用 `deliverAs: "steer"` 发一条状态提醒。AstrCode 没有隐藏消息通道，
//! 两处都落到 `before_provider_request` 的 [`ProviderResult::AppendMessages`] 上：
//!
//! | 上游 | 移植版 |
//! |---|---|
//! | `before_agent_start` → `[消息时间] <taskN>…` | 每个 LLM 请求追加一条同形消息 |
//! | `turn_end` → `<taskN>\n<stateGuard>…\n</taskN>` | 每个 LLM 请求追加一条同形消息 |
//! | `before_agent_start` → `systemPrompt += SYSTEM.md` | `prompt_build` → `system_prompts` |
//!
//! 两条消息都追加在消息列表**末尾**，因此不破坏 provider 的前缀缓存；内容在一个 turn
//! 内逐字节稳定（计数器只在 `TurnEnd` 变），所以同一 turn 内多次请求的追加段是同一份。
//!
//! `AppendMessages` 是请求级、不落盘的（`turn_runner.rs:865`），代价是每步请求重算并
//! 多带约 350–450 token，收益是引导在每一步都在场。上游那条 durable 消息只在 turn 开头
//! 注入一次，后续步骤靠历史重放。


use astrcode_extension_sdk::s5r::hooks::LifecycleHookInput;
use astrcode_extension_worker::worker_prelude::*;

use crate::machine::{self, WarnLevel, warn_level};
use crate::state::{self, Snapshot, StoreRegistry};
use crate::templates::build_template;
use crate::types::{
    SessionState, TOOL_TRANSITION, ThinkingState, get_max_state_turns, get_reminder_interval,
    state_label,
};
use crate::worker::EXTENSION_ID;

/// 非 START 状态追加的续作提醒，防止模型被用户的新消息带偏而放弃当前任务。
const CONTINUATION_NOTE: &str = "> ⚠️ 上一次对话未完成任务，不可中断——请回到当前【{label}】状态，继续把未完成的流程推进到底。\n严格遵守人格角色定义，严格遵守用户指令。";

/// 组装本轮要追加到 provider 消息列表末尾的隐藏引导。
///
/// 返回 1 条或 2 条消息：第一条是状态机引导（对应上游 `before_agent_start`），
/// 第二条是轮次提醒（对应上游 `turn_end`），后者在间隔未命中时缺席。
pub fn guidance_messages(
    machine_state: &SessionState,
    violation_pending: bool,
    timestamp: &str,
) -> Vec<String> {
    let state = machine_state.state;
    let guide = build_template(
        state,
        machine_state.task_turn_count,
        machine_state.master_task_type,
        machine_state.sub_task_type,
        machine_state.difficulty,
        &machine_state.visited,
    );

    // START 是全量注入；其余状态追加续作提醒。
    let final_guide = if state == ThinkingState::START {
        guide
    } else {
        format!(
            "{guide}\n\n{}",
            CONTINUATION_NOTE.replace("{label}", state_label(state))
        )
    };

    let mut messages = vec![format!("[消息时间：{timestamp}] {final_guide}")];
    if let Some(reminder) = reminder_text(machine_state, violation_pending) {
        let turn = machine_state.task_turn_count;
        messages.push(format!("<task{turn}>\n{reminder}\n</task{turn}>"));
    }
    messages
}

/// 轮次提醒正文（上游 `turn_end` 钩子的 `reminder`）。
///
/// 三级警告由 [`machine::warn_level`] 判定；间隔闸门与上游一致：非 hardStop 时必须
/// `state_turn_count % interval == 0` 才发，避免稀释正常提示词。
pub fn reminder_text(machine_state: &SessionState, violation_pending: bool) -> Option<String> {
    let state = machine_state.state;
    if state == ThinkingState::END {
        return None;
    }

    let difficulty = machine_state.difficulty;
    let turn = machine_state.state_turn_count;
    let max_turns = get_max_state_turns(state, difficulty);
    let level = warn_level(turn, max_turns);
    let is_hard_stop = level == Some(WarnLevel::HardStop);

    // `get_reminder_interval` 恒 ≥ 1（上游用 `|| 1` 兜底），`interval > 0` 因此恒真；
    // 保留它只为与上游的判定式逐条对齐。
    let interval = get_reminder_interval(state, difficulty);
    if !is_hard_stop && interval > 0 && !turn.is_multiple_of(interval) {
        return None;
    }

    let warning = level
        .map(|level| machine::render_warning(level, state, turn, max_turns, difficulty))
        .unwrap_or_default();

    // hardStop 整条替换为强提醒，不拼接任何其他内容。
    if is_hard_stop {
        return Some(warning);
    }

    let guard = format!(
        "<stateGuard>\n渐近式思考·强制执行：当前处于【{}】状态（第{turn}/{max_turns}轮）。\n本阶段必须完成该状态职责后，调用 `{TOOL_TRANSITION}` 工具流转状态。\n未调用前请勿结束回复；若阶段未完成请说明原因后继续推进。\n</stateGuard>",
        state_label(state)
    );
    let violation = if violation_pending {
        format!(
            "\n<violationWarning>⚠️ 本轮未调用 {TOOL_TRANSITION} 工具。若状态职责已完成，必须立即流转；若未完成，说明原因后继续。</violationWarning>"
        )
    } else {
        String::new()
    };

    Some(format!("{guard}{violation}{warning}"))
}

/// `TurnStart`：END（任务已终结）→ START，保留累计任务数；同时清空本 turn 的流转标记。
///
/// 上游分两处处理这件事：`before_agent_start` 里把 `END`/空状态复位成 START，
/// `turn_end` 里再检测「执行中用户发了消息」补一次复位。AstrCode 的 `TurnStart`
/// 天然覆盖两者——能进 turn 就说明用户发了消息——因此合并成一处。
pub fn reset_for_new_turn(store: &mut state::SessionStore) {
    store.take_transition_called();

    if store.machine().state != ThinkingState::END {
        return;
    }

    let machine = store.machine_mut();
    let task_turn_count = machine.task_turn_count;
    *machine = SessionState {
        state: ThinkingState::START,
        task_turn_count,
        ..SessionState::default()
    };
    store.set_violation_pending(false);
}

/// `TurnEnd`：计数 + 记录本 turn 是否漏了流转。
pub fn record_turn_end(store: &mut state::SessionStore) {
    if store.machine().state == ThinkingState::END {
        store.take_transition_called();
        return;
    }

    let transition_called = store.take_transition_called();
    machine::bump(store.machine_mut());
    store.set_violation_pending(!transition_called);
}

/// 读会话快照。锁在函数内取、在函数内放，不跨 `.await`。
fn snapshot(registry: &StoreRegistry, session_id: &str) -> Snapshot {
    let store = registry.session(EXTENSION_ID, session_id);
    let guard = state::lock(&store);
    guard.snapshot()
}

/// 在阻塞线程池上改会话状态并落盘。
async fn mutate<T, F>(
    registry: &StoreRegistry,
    session_id: &str,
    f: F,
) -> Result<T, ErrorPayload>
where
    F: FnOnce(&mut state::SessionStore) -> T + Send + 'static,
    T: Send + 'static,
{
    let store = registry.session(EXTENSION_ID, session_id);
    state::mutate(store, f).await.map_err(|error| {
        ErrorPayload::new(
            WireErrorCode::InternalError,
            format!("asymptotic-thinking 状态任务失败：{error}"),
        )
    })
}

// ── 判定逻辑（可直接驱动，集成测试走这一层）─────────────────────────────────

/// 会话是否启用。读不到状态时按启用处理——与上游「无记录默认启用」一致。
pub fn is_enabled(registry: &StoreRegistry, session_id: &str) -> bool {
    snapshot(registry, session_id).enabled
}

/// `prompt_build` 的贡献：启用时给出静态框架规则，禁用时为空。
pub fn framework_contribution(registry: &StoreRegistry, session_id: &str) -> PromptContributions {
    if !is_enabled(registry, session_id) {
        return PromptContributions::default();
    }
    PromptContributions {
        system_prompts: vec![crate::framework_rules::FRAMEWORK_RULES.to_owned()],
        ..Default::default()
    }
}

/// `TurnStart`：END → START 复位，并清空本 turn 的流转标记。
pub async fn handle_turn_start(
    registry: &StoreRegistry,
    session_id: &str,
) -> Result<(), ErrorPayload> {
    mutate(registry, session_id, |store| {
        if !store.enabled() {
            return;
        }
        reset_for_new_turn(store);
    })
    .await
    .map(|_| ())
}

/// `TurnEnd`：计数 + 记录本 turn 是否漏了流转。
pub async fn handle_turn_end(
    registry: &StoreRegistry,
    session_id: &str,
) -> Result<(), ErrorPayload> {
    mutate(registry, session_id, |store| {
        if !store.enabled() {
            return;
        }
        record_turn_end(store);
    })
    .await
    .map(|_| ())
}

/// `before_provider_request` 的决策：禁用时放行，否则把引导追加到消息列表末尾。
pub fn provider_decision(registry: &StoreRegistry, session_id: &str) -> ProviderResult {
    let snapshot = snapshot(registry, session_id);
    if !snapshot.enabled {
        return ProviderResult::Allow;
    }

    let messages = guidance_messages(
        &snapshot.machine,
        snapshot.violation_pending,
        &crate::clock::utc_now(),
    );
    ProviderResult::AppendMessages {
        messages: messages.into_iter().map(LlmMessage::user).collect(),
    }
}

// ── Worker handler 装配 ─────────────────────────────────────────────────────

/// `prompt_build`：注入静态框架规则（落在 system prompt 的静态前缀区）。
pub fn on_prompt_build(registry: StoreRegistry) -> HookHandlerFn {
    prompt_build_handler(move |input: PromptBuildHookInput, ctx| {
        let registry = registry.clone();
        async move { Ok(framework_contribution(&registry, &session_id(&input.session_id, &ctx))) }
    })
}

/// `TurnStart` / `TurnEnd` 生命周期钩子。
pub fn on_lifecycle(registry: StoreRegistry, on_end: bool) -> HookHandlerFn {
    hook_handler_args(move |_input: LifecycleHookInput, ctx| {
        let registry = registry.clone();
        async move {
            let session_id = ctx.session_id().to_owned();
            if on_end {
                handle_turn_end(&registry, &session_id).await?;
            } else {
                handle_turn_start(&registry, &session_id).await?;
            }
            Ok(HandlerResult::from(HookResult::Allow))
        }
    })
}

/// `before_provider_request`：把引导追加到本次请求的消息列表末尾。
pub fn on_before_provider_request(registry: StoreRegistry) -> HookHandlerFn {
    provider_handler(move |input: ProviderHookInput, ctx| {
        let registry = registry.clone();
        async move { Ok(provider_decision(&registry, &session_id(&input.session_id, &ctx))) }
    })
}

/// hook 载荷里的 `session_id` 为空时回落到调用上下文。
fn session_id(from_input: &str, ctx: &WorkerInvocationContext) -> String {
    if from_input.is_empty() {
        ctx.session_id().to_owned()
    } else {
        from_input.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Difficulty, MasterTaskType, SubTaskType};

    fn profile(state: ThinkingState, difficulty: Difficulty) -> SessionState {
        SessionState {
            state,
            difficulty: Some(difficulty),
            master_task_type: Some(MasterTaskType::CODING),
            sub_task_type: Some(SubTaskType::RUST_DEV),
            ..SessionState::default()
        }
    }

    fn temp_store(name: &str) -> state::SessionStore {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-asymptotic-hook-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        state::SessionStore::load(dir.join("state.json"))
    }

    /// START 注入全量引导（不追加续作提醒），并带一条 stateGuard。
    #[test]
    fn start_guidance_has_no_continuation_note() {
        let machine = SessionState::default();
        let messages = guidance_messages(&machine, false, "2025-01-01 00:00:00 UTC");
        assert_eq!(messages.len(), 2);
        assert!(messages[0].starts_with("[消息时间：2025-01-01 00:00:00 UTC] <task0>"));
        assert!(!messages[0].contains("上一次对话未完成任务"));
        assert!(messages[1].contains("当前处于【启动】状态（第0/1轮）"));
    }

    /// 非 START 追加续作提醒；轮次提醒在间隔命中时作为第二条消息出现。
    #[test]
    fn a_business_state_appends_the_continuation_note() {
        let machine = profile(ThinkingState::DESIGN, Difficulty::MODERATE);
        let messages = guidance_messages(&machine, false, "T");
        assert_eq!(messages.len(), 2);
        assert!(messages[0].contains("上一次对话未完成任务，不可中断——请回到当前【方案设计】状态"));
        assert!(messages[1].starts_with("<task0>\n<stateGuard>"));
        assert!(messages[1].ends_with("</task0>"));
    }

    /// 间隔闸门：非 hardStop 时必须落在 `stateTurnCount % interval == 0` 上。
    #[test]
    fn the_interval_gate_throttles_the_reminder() {
        // MODERATE/DESIGN 的间隔是 12。
        let mut machine = profile(ThinkingState::DESIGN, Difficulty::MODERATE);
        machine.state_turn_count = 1;
        assert!(reminder_text(&machine, false).is_none());
        machine.state_turn_count = 11;
        assert!(reminder_text(&machine, false).is_none());
        machine.state_turn_count = 12;
        assert!(reminder_text(&machine, false).is_some());
    }

    /// hardStop 不受间隔限制，且整条替换为强提醒（不拼接 stateGuard）。
    #[test]
    fn hard_stop_bypasses_the_gate_and_replaces_the_guard() {
        // MODERATE/DESIGN：maxTurns = 1000，hardStop 从 1000 + 334 + 1 = 1335 起。
        let mut machine = profile(ThinkingState::DESIGN, Difficulty::MODERATE);
        machine.state_turn_count = 1335;
        let text = reminder_text(&machine, true).expect("hardStop 必须发");
        assert!(text.starts_with("\n<hardStop>⛔ 强制停止："));
        assert!(!text.contains("<stateGuard>"));
        assert!(!text.contains("<violationWarning>"));
    }

    #[test]
    fn violation_warning_only_shows_while_pending() {
        let machine = profile(ThinkingState::DESIGN, Difficulty::MODERATE);
        let clean = reminder_text(&machine, false).expect("间隔命中");
        assert!(!clean.contains("<violationWarning>"));
        let dirty = reminder_text(&machine, true).expect("间隔命中");
        assert!(dirty.contains("本轮未调用 asymptotic-think_transition 工具"));
    }

    #[test]
    fn end_has_no_reminder() {
        let mut machine = profile(ThinkingState::END, Difficulty::MODERATE);
        machine.state_turn_count = 5;
        assert!(reminder_text(&machine, true).is_none());
    }

    /// END 复位成 START 时必须保留累计任务数，并清掉违规标记。
    #[test]
    fn turn_start_resets_end_back_to_start() {
        let mut store = temp_store("turn-start");
        {
            let machine = store.machine_mut();
            machine.state = ThinkingState::END;
            machine.task_turn_count = 7;
        }
        store.set_violation_pending(true);
        store.mark_transition_called();

        reset_for_new_turn(&mut store);

        assert_eq!(store.machine().state, ThinkingState::START);
        assert_eq!(store.machine().task_turn_count, 7);
        assert!(!store.violation_pending());
        assert!(!store.take_transition_called());
    }

    /// 非 END 状态不该被 TurnStart 改写。
    #[test]
    fn turn_start_leaves_a_running_task_alone() {
        let mut store = temp_store("turn-start-keep");
        {
            let machine = store.machine_mut();
            machine.state = ThinkingState::DESIGN;
            machine.state_turn_count = 4;
        }
        reset_for_new_turn(&mut store);
        assert_eq!(store.machine().state, ThinkingState::DESIGN);
        assert_eq!(store.machine().state_turn_count, 4);
    }

    #[test]
    fn turn_end_counts_and_records_the_violation() {
        let mut store = temp_store("turn-end");
        {
            let machine = store.machine_mut();
            machine.state = ThinkingState::DESIGN;
            machine.difficulty = Some(Difficulty::MODERATE);
        }

        // 没有调用 transition → 记违规。
        record_turn_end(&mut store);
        assert_eq!(store.machine().state_turn_count, 1);
        assert!(store.violation_pending());

        // 调用过 transition → 不记违规。
        store.mark_transition_called();
        record_turn_end(&mut store);
        assert_eq!(store.machine().state_turn_count, 2);
        assert!(!store.violation_pending());
    }

    /// END 不计数。
    #[test]
    fn turn_end_skips_end() {
        let mut store = temp_store("turn-end-end");
        store.machine_mut().state = ThinkingState::END;
        record_turn_end(&mut store);
        assert_eq!(store.machine().state_turn_count, 0);
    }
}
