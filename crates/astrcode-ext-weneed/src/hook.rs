//! 四个钩子的适配层。
//!
//! | 钩子 | 职责 |
//! |---|---|
//! | `prompt_build` | 把完整规范注入 system prompt 的静态前缀区 |
//! | `provider_contribution` | 需要时往请求尾部追加一条极简风格提醒 |
//! | `after_provider_response` | 读 assistant 的推理通道，判定风格漂移 |
//! | `pre_tool_use` | 拦截黑名单里的工具直呼 |
//!
//! 钩子只做「取参 → 判定 → 映射回 S5R 结果」，状态读写集中在 [`SharedState`]。

use std::sync::Arc;

use astrcode_extension_worker::worker_prelude::*;

use crate::{
    config::ReminderMode, drift, guard, model, reminder, spec::WE_NEED_SPEC, state::SharedState,
};

/// `prompt_build` 贡献：DeepSeek 会话注入完整规范。
///
/// 模型闸门在宿主调用之前就返回空贡献：非 DeepSeek 会话**一次 `session_state` 都不发**。
pub async fn contribute(
    state: &Arc<SharedState>,
    input: &PromptBuildHookInput,
) -> Result<PromptContributions, ErrorPayload> {
    if !model::is_deepseek(&input.model.model) {
        return Ok(PromptContributions::default());
    }
    if !state.session_enabled(&input.session_id).await {
        return Ok(PromptContributions::default());
    }
    state.count_injection();
    Ok(PromptContributions {
        system_prompts: vec![WE_NEED_SPEC.to_owned()],
        ..Default::default()
    })
}

/// `provider_contribution` prepare：判定这一轮是否追加贴尾提醒。
///
/// 提醒是**请求局部**的（`AppendMessages` 只作用于当前 LLM 请求，不落 transcript），
/// 因此没有需要靠 `acknowledge` 落定的扩展侧状态：`acknowledge` 阶段直接返回 `None`。
///
/// contribution id 由 request id 派生：pending 状态就是「为请求 R 追加的那条提醒」，
/// 请求换了 id 就换，同一请求被重复 prepare 时复用同一个 id。
pub async fn plan_reminder(
    state: &Arc<SharedState>,
    input: &ProviderContributionHookInput,
) -> Result<Option<ProviderContributionData>, ErrorPayload> {
    let ProviderContributionHookInput::Prepare {
        request_id,
        session_id,
        model,
        ..
    } = input
    else {
        return Ok(None);
    };

    if !model::is_deepseek(&model.model) {
        return Ok(None);
    }
    let config = state.config();
    if !config.enabled || config.reminder == ReminderMode::Off {
        return Ok(None);
    }
    if !state.session_enabled(session_id).await {
        return Ok(None);
    }
    if !state.should_remind(session_id, config.reminder) {
        return Ok(None);
    }

    state.count_reminder();
    Ok(Some(ProviderContributionData {
        contribution_id: format!("weneed-reminder:{request_id}"),
        effect: ProviderContributionEffect::AppendMessages {
            messages: vec![LlmMessage::user(reminder::REMINDER_TEXT)],
        },
    }))
}

/// `after_provider_response`：读 assistant 的推理通道判定漂移。
///
/// 该钩子固定 advisory 模式，但宿主仍会把 `ReplaceMessages` / `AppendMessages` 的结果
/// 并进最终回复正文（`turn_runner::dispatch_after_provider_response`），所以这里**只能**
/// 返回 [`ProviderResult::Allow`]。
///
/// 闸门与另外三个钩子一致：关掉的会话不做观测。规范没注入时模型不按规范展开是预期行为，
/// 计进 `drifts` 只会让统计变得误导。会话状态读路径带内存缓存，且同一轮的 `prompt_build`
/// 更早触发过，因此这道闸门不产生额外的宿主往返。
pub async fn observe_reasoning(
    state: &Arc<SharedState>,
    input: &ProviderHookInput,
) -> Result<ProviderResult, ErrorPayload> {
    let config = state.config();
    if !config.enabled || !config.drift_check {
        return Ok(ProviderResult::Allow);
    }
    if !model::is_deepseek(&input.model.model) {
        return Ok(ProviderResult::Allow);
    }
    if !state.session_enabled(&input.session_id).await {
        return Ok(ProviderResult::Allow);
    }
    let Some(reasoning) = last_reasoning(input) else {
        return Ok(ProviderResult::Allow);
    };
    state.note_observation(&input.session_id, drift::inspect(reasoning));
    Ok(ProviderResult::Allow)
}

/// 取消息列表里最后一条带推理通道的 assistant 消息。
///
/// 不判角色：`reasoning_content` 按契约只出现在 assistant 消息上（`astrcode-core::llm`），
/// 而 worker 作者面不导出 `LlmRole`。
fn last_reasoning(input: &ProviderHookInput) -> Option<&str> {
    input
        .messages
        .iter()
        .rev()
        .find_map(|message| message.reasoning_content.as_deref())
        .filter(|reasoning| !reasoning.trim().is_empty())
}

/// `pre_tool_use`：拦截黑名单里的工具直呼。
///
/// 判定顺序按代价排：先看本地配置，最后才碰宿主的会话状态。
pub async fn guard_tool_call(
    state: &Arc<SharedState>,
    input: &ToolUseHookInput,
) -> Result<PreToolUseResult, ErrorPayload> {
    let config = state.config();
    if !config.enabled || !config.guard.enabled {
        return Ok(PreToolUseResult::Allow);
    }
    if !model::is_deepseek(&input.model.model) {
        return Ok(PreToolUseResult::Allow);
    }
    let Some(reason) = guard::block_reason(&input.tool_name, &config.guard.blocked_tools) else {
        return Ok(PreToolUseResult::Allow);
    };
    if !state.session_enabled(&input.session_id).await {
        return Ok(PreToolUseResult::Allow);
    }

    state.count_block();
    Ok(PreToolUseResult::Block { reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str) -> (Arc<SharedState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-weneed-hook-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (state, warning) = SharedState::load(crate::config::store_at(dir.join("config.json")));
        assert!(warning.is_none());
        (state, dir)
    }

    fn prepare_input(request_id: &str, model: &str) -> ProviderContributionHookInput {
        serde_json::from_value(serde_json::json!({
            "phase": "prepare",
            "request_id": request_id,
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": model, "provider_kind": "" },
            "messages": []
        }))
        .expect("prepare 载荷应可解析")
    }

    fn provider_input(model: &str, reasoning: Option<&str>) -> ProviderHookInput {
        let message = serde_json::json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "done" }],
            "reasoning_content": reasoning,
        });
        serde_json::from_value(serde_json::json!({
            "request_id": "req-1",
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": model, "provider_kind": "" },
            "messages": [message]
        }))
        .expect("provider 载荷应可解析")
    }

    fn tool_input(model: &str, tool_name: &str) -> ToolUseHookInput {
        serde_json::from_value(serde_json::json!({
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": model, "provider_kind": "" },
            "tool_call_id": "call-1",
            "tool_name": tool_name,
            "tool_input": {},
            "available_tools": []
        }))
        .expect("tool use 载荷应可解析")
    }

    #[tokio::test]
    async fn the_acknowledge_phase_contributes_nothing() {
        let (state, dir) = state("acknowledge");
        let input: ProviderContributionHookInput = serde_json::from_value(serde_json::json!({
            "phase": "acknowledge",
            "request_id": "req-1",
            "contribution_id": "weneed-reminder:req-1",
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": "deepseek-chat", "provider_kind": "" }
        }))
        .expect("acknowledge 载荷应可解析");

        assert!(plan_reminder(&state, &input).await.unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_first_request_of_a_session_gets_a_tail_reminder() {
        let (state, dir) = state("first-request");
        let contribution = plan_reminder(&state, &prepare_input("req-1", "deepseek-chat"))
            .await
            .unwrap()
            .expect("首轮应当贴提醒");

        assert_eq!(contribution.contribution_id, "weneed-reminder:req-1");
        let ProviderContributionEffect::AppendMessages { messages } = contribution.effect else {
            panic!("提醒应当是追加消息");
        };
        assert_eq!(messages.len(), 1);
        assert_eq!(state.stats().reminders, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_later_compliant_request_gets_no_reminder() {
        let (state, dir) = state("compliant");
        plan_reminder(&state, &prepare_input("req-1", "deepseek-chat"))
            .await
            .unwrap();

        observe_reasoning(
            &state,
            &provider_input(
                "deepseek-chat",
                Some("We need to read the file. I will open it."),
            ),
        )
        .await
        .unwrap();

        assert!(
            plan_reminder(&state, &prepare_input("req-2", "deepseek-chat"))
                .await
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_drifting_response_rearms_the_reminder() {
        let (state, dir) = state("drift");
        plan_reminder(&state, &prepare_input("req-1", "deepseek-chat"))
            .await
            .unwrap();

        observe_reasoning(
            &state,
            &provider_input("deepseek-chat", Some("Let me check the file first.")),
        )
        .await
        .unwrap();

        let contribution = plan_reminder(&state, &prepare_input("req-2", "deepseek-chat"))
            .await
            .unwrap()
            .expect("漂移后应当重新贴提醒");
        assert_eq!(contribution.contribution_id, "weneed-reminder:req-2");
        assert_eq!(state.stats().drifts, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn other_models_never_get_a_tail_reminder() {
        let (state, dir) = state("other-model");
        for model in ["gpt-4.1", "claude-sonnet-4-5"] {
            assert!(
                plan_reminder(&state, &prepare_input("req-1", model))
                    .await
                    .unwrap()
                    .is_none(),
                "{model} 不应收到提醒"
            );
        }
        assert_eq!(state.stats().reminders, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn drift_check_can_be_turned_off() {
        let (state, dir) = state("drift-off");
        let mut config = state.config();
        config.drift_check = false;
        state.set_config(config).unwrap();

        observe_reasoning(
            &state,
            &provider_input("deepseek-chat", Some("Let me check the file first.")),
        )
        .await
        .unwrap();

        assert_eq!(state.stats().drifts, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_missing_reasoning_channel_is_not_an_observation() {
        let (state, dir) = state("no-reasoning");
        observe_reasoning(&state, &provider_input("deepseek-chat", None))
            .await
            .unwrap();
        assert_eq!(state.stats().drifts, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// advisory 钩子的返回值会被并进最终回复正文，绝不能带消息。
    #[tokio::test]
    async fn the_observer_never_rewrites_the_reply() {
        let (state, dir) = state("allow-only");
        let result = observe_reasoning(
            &state,
            &provider_input("deepseek-chat", Some("We need to read the file.")),
        )
        .await
        .unwrap();
        assert!(matches!(result, ProviderResult::Allow));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_guard_is_off_by_default() {
        let (state, dir) = state("guard-default");
        let result = guard_tool_call(&state, &tool_input("deepseek-chat", "edit"))
            .await
            .unwrap();
        assert!(matches!(result, PreToolUseResult::Allow));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_guard_blocks_the_configured_tools() {
        let (state, dir) = state("guard-on");
        let mut config = state.config();
        config.guard.enabled = true;
        state.set_config(config).unwrap();

        match guard_tool_call(&state, &tool_input("deepseek-chat", "edit"))
            .await
            .unwrap()
        {
            PreToolUseResult::Block { reason } => {
                assert!(reason.contains("`replace`"), "{reason}");
            }
            other => panic!("edit 应当被拦，实际是 {other:?}"),
        }
        // 未列入黑名单的工具照常放行。
        assert!(matches!(
            guard_tool_call(&state, &tool_input("deepseek-chat", "write"))
                .await
                .unwrap(),
            PreToolUseResult::Allow
        ));
        assert_eq!(state.stats().blocks, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_guard_ignores_other_models() {
        let (state, dir) = state("guard-other-model");
        let mut config = state.config();
        config.guard.enabled = true;
        state.set_config(config).unwrap();

        assert!(matches!(
            guard_tool_call(&state, &tool_input("gpt-4.1", "edit"))
                .await
                .unwrap(),
            PreToolUseResult::Allow
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 关闭的会话开关要同时管住注入、提醒与守卫。
    #[tokio::test]
    async fn a_disabled_session_switch_turns_everything_off() {
        let (state, dir) = state("session-off");
        let mut config = state.config();
        config.guard.enabled = true;
        state.set_config(config).unwrap();
        state.cache_session_switch_for_tests("s-1", crate::toggle::SessionSwitch::Off);

        assert!(
            plan_reminder(&state, &prepare_input("req-1", "deepseek-chat"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            guard_tool_call(&state, &tool_input("deepseek-chat", "edit"))
                .await
                .unwrap(),
            PreToolUseResult::Allow
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
