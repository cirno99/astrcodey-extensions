//! Worker 装配：注册三个工具、四个钩子与 `/asymptotic-toggle`。
//!
//! # 钩子为什么落在这四个点上
//!
//! | 钩子 | 职责 | 对应上游 |
//! |---|---|---|
//! | `prompt_build` | 把 `SYSTEM.md` 静态规则注入 system prompt 前缀区 | `before_agent_start` 里的 `systemPrompt +=` |
//! | `TurnStart` | END → START 复位；清空本 turn 的流转标记 | `before_agent_start` + `turn_end` 里的复位 |
//! | `TurnEnd` | `state_turn_count` +1；记录本 turn 是否漏了流转 | `turn_end` 的 `bumpAndWarn` + 违规检测 |
//! | `before_provider_request` | 把引导与轮次提醒追加到消息列表末尾 | `before_agent_start` 的隐藏消息 + `turn_end` 的 steer 消息 |
//!
//! `TurnStart` / `TurnEnd` 用 `HookMode::Advisory`：它们只改插件自己的状态，
//! 不需要（也不该）阻断宿主流程。`before_provider_request` 用 `HookMode::Blocking`，
//! 因为要改写本次请求的消息列表。

use astrcode_extension_worker::worker_prelude::*;

use crate::state::StoreRegistry;
use crate::{command, hook, tools};

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-asymptotic-thinking";

/// 装配并运行 worker；宿主经 stdio 以 S5R 3.0 协议驱动。
pub async fn run() -> Result<(), ErrorPayload> {
    let mut worker = Worker::new(EXTENSION_ID, env!("CARGO_PKG_VERSION"));
    // 要改写 provider 请求的消息列表。声明它是为了在 manifest 里如实说明能力面；
    // 宿主当前不对 `before_provider_request` 做能力闸门。
    worker.capability(ExtensionCapability::ProviderRequest);

    let registry = StoreRegistry::default();

    worker.tool(
        tools::transition_tool(),
        tools::transition_planner(),
        tools::transition_handler(registry.clone()),
    )?;
    worker.tool(
        tools::task_info_tool(),
        tools::task_info_planner(),
        tools::task_info_handler(registry.clone()),
    )?;
    worker.tool(
        tools::status_tool(),
        tools::status_planner(),
        tools::status_handler(registry.clone()),
    )?;

    worker.on_prompt_build(hook::on_prompt_build(registry.clone()))?;
    worker.hook(
        LifecycleEvent::TurnStart,
        HookMode::Advisory,
        hook::on_lifecycle(registry.clone(), false),
    )?;
    worker.hook(
        LifecycleEvent::TurnEnd,
        HookMode::Advisory,
        hook::on_lifecycle(registry.clone(), true),
    )?;
    worker.hook(
        LifecycleEvent::BeforeProviderRequest,
        HookMode::Blocking,
        hook::on_before_provider_request(registry.clone()),
    )?;

    worker.command(
        command(command::COMMAND_NAME)
            .description(
                "渐近式思考框架：/asymptotic-toggle 切换本会话的启用状态，\
                 /asymptotic-toggle on|off|status 显式设置或查询。",
            )
            .build(),
        command::handler(registry),
    )?;

    worker.run_stdio().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_id_matches_the_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../extension.json")).expect("extension.json 必须可解析");
        assert_eq!(manifest["extension_id"], serde_json::json!(EXTENSION_ID));
        assert_eq!(
            manifest["command"][0],
            serde_json::json!("./astrcode-ext-asymptotic-thinking")
        );
    }

    #[test]
    fn the_command_name_is_the_upstream_one() {
        assert_eq!(command::COMMAND_NAME, "asymptotic-toggle");
    }
}
