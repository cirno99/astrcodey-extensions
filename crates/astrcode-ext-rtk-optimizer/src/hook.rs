//! 两个钩子的适配层：`tool_input_transform`（命令改写）与 `post_tool_use`（输出压缩）。
//!
//! 钩子只做「取参 → 转交纯逻辑 → 映射回 S5R 结果」，状态读写集中在 [`SharedState`]。

use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use astrcode_extension_sdk::s5r::hooks::{PostToolUseHookInput, ToolUseHookInput};
use astrcode_extension_worker::worker_prelude::*;
use serde_json::json;

use astrcode_ext_common::config::ConfigStore;

use crate::{
    compact,
    config::{Config, Mode},
    rewrite::{self, RtkRuntime},
    shell,
};

/// 钩子与命令共享的可变状态。
pub struct SharedState {
    store: ConfigStore,
    config: RwLock<Config>,
    runtime: RtkRuntime,
}

impl SharedState {
    /// 从磁盘加载配置并构造状态。
    pub fn load(store: ConfigStore) -> (Arc<Self>, Option<String>) {
        let loaded = store.load();
        (
            Arc::new(Self {
                store,
                config: RwLock::new(loaded.config),
                runtime: RtkRuntime::new(),
            }),
            loaded.warning,
        )
    }

    pub fn config(&self) -> Config {
        match self.config.read() {
            Ok(config) => config.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn store_path(&self) -> std::path::PathBuf {
        self.store.path().to_path_buf()
    }

    pub fn runtime(&self) -> &RtkRuntime {
        &self.runtime
    }

    /// 归一化后写回磁盘并更新内存缓存。内存先更新，因此落盘失败也不会让本次会话的
    /// 配置回退；错误如实上抛给调用方。
    pub fn set_config(&self, next: Config) -> Result<(), String> {
        let normalized = next
            .normalized()
            .map_err(|error| format!("Failed to normalize config: {error}"))?;
        match self.config.write() {
            Ok(mut current) => *current = normalized.clone(),
            Err(poisoned) => *poisoned.into_inner() = normalized.clone(),
        }
        self.store
            .save(&normalized)
            .map_err(|error| format!("Failed to save {}: {error}", self.store.path().display()))
    }

    /// 从磁盘重新加载配置（`/rtk` 与 `session_start` 使用）。
    pub fn reload(&self) -> Option<String> {
        let loaded = self.store.load();
        match self.config.write() {
            Ok(mut current) => *current = loaded.config,
            Err(poisoned) => *poisoned.into_inner() = loaded.config,
        }
        loaded.warning
    }
}

/// `post_tool_use` 钩子：超阈值则压缩工具结果。
pub async fn post_tool_use(
    input: PostToolUseHookInput,
    state: Arc<SharedState>,
) -> Result<PostToolUseResult, ErrorPayload> {
    let config = state.config();
    if !config.enabled || !config.output_compaction.enabled {
        return Ok(PostToolUseResult::Allow);
    }

    let outcome = compact::compact_tool_result(
        &input.tool_name,
        &input.tool_input,
        &input.tool_result.content,
        Path::new(&input.working_dir),
        &config,
    );

    match outcome {
        Some(outcome) => Ok(PostToolUseResult::ModifyResult {
            content: outcome.text,
        }),
        None => Ok(PostToolUseResult::Allow),
    }
}

/// `tool_input_transform` 钩子：把 `shell` 命令改写成 rtk 等价命令。
pub async fn tool_input_transform(
    input: ToolUseHookInput,
    state: Arc<SharedState>,
) -> Result<ToolInputTransformResult, ErrorPayload> {
    if input.tool_name != compact::SHELL_TOOL {
        return Ok(ToolInputTransformResult::Unchanged);
    }

    let config = state.config();
    if !config.enabled {
        return Ok(ToolInputTransformResult::Unchanged);
    }

    let Some(command) = input
        .tool_input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(ToolInputTransformResult::Unchanged);
    };
    if command.trim().is_empty() {
        return Ok(ToolInputTransformResult::Unchanged);
    }

    let mut tool_input = input.tool_input.clone();
    let mut changed = false;

    // win32 上的 bash 兼容修正在改写之前做（与上游顺序一致）。
    let platform = std::env::consts::OS;
    let (fixed, _applied) = shell::apply_windows_bash_compatibility_fixes(&command, platform);
    if fixed != command {
        tool_input["command"] = json!(fixed);
        changed = true;
    }
    let command = tool_input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&command)
        .to_owned();

    state.runtime().ensure_fresh(&config).await;
    let status = state.runtime().status();
    if rewrite::should_skip_command_handling(&config, &status) {
        return Ok(finish(tool_input, changed));
    }

    let decision = rewrite::compute_rewrite_decision(&command, status.executable.as_ref()).await;

    if !decision.changed {
        if let Some(warning) = &decision.warning {
            state.runtime().record_notice(rewrite::format_rewrite_warning(
                &decision.original_command,
                warning,
            ));
        }
        return Ok(finish(tool_input, changed));
    }

    match config.mode {
        Mode::Rewrite => {
            if config.show_rewrite_notifications {
                state.runtime().record_notice(rewrite::format_rewrite_notice(
                    &decision.original_command,
                    &decision.rewritten_command,
                ));
            }
            let scoped = shell::apply_rtk_command_environment(&decision.rewritten_command);
            let final_command = shell::apply_rewritten_command_shell_safety_fixups(&scoped, platform);
            tool_input["command"] = json!(final_command);
            Ok(ToolInputTransformResult::Replace { tool_input })
        },
        Mode::Suggest => {
            if config.show_rewrite_notifications {
                state.runtime().record_notice(format!(
                    "RTK suggestion: {}",
                    decision.rewritten_command
                ));
            }
            Ok(finish(tool_input, changed))
        },
    }
}

/// 只在入参确实被改过时才返回 `Replace`。
fn finish(tool_input: serde_json::Value, changed: bool) -> ToolInputTransformResult {
    if changed {
        ToolInputTransformResult::Replace { tool_input }
    } else {
        ToolInputTransformResult::Unchanged
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn temp_state(name: &str) -> (Arc<SharedState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-rtk-hook-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        let (state, warning) = SharedState::load(crate::config::store_at(&path));
        assert!(warning.is_none());
        (state, dir)
    }

    fn post_input(tool_name: &str, tool_input: serde_json::Value, content: &str) -> PostToolUseHookInput {
        serde_json::from_value(json!({
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": "test", "provider_kind": "" },
            "tool_call_id": "call-1",
            "tool_name": tool_name,
            "tool_input": tool_input,
            "tool_result": { "content": content, "is_error": false, "metadata": {} },
            "is_error": false
        }))
        .expect("post tool use input is well formed")
    }

    fn transform_input(tool_name: &str, tool_input: serde_json::Value) -> ToolUseHookInput {
        serde_json::from_value(json!({
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": "test", "provider_kind": "" },
            "tool_call_id": "call-1",
            "tool_name": tool_name,
            "tool_input": tool_input,
            "available_tools": []
        }))
        .expect("tool use input is well formed")
    }

    #[tokio::test]
    async fn post_tool_use_leaves_non_compacted_tools_alone() {
        let (state, dir) = temp_state("post-unknown");
        let result = post_tool_use(post_input("write", json!({}), "hello"), state)
            .await
            .unwrap();
        assert!(matches!(result, PostToolUseResult::Allow));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn post_tool_use_compacts_grep_output() {
        let (state, dir) = temp_state("post-grep");
        let content = "src/b.ts:3:const b = 1;\nsrc/a.ts:1:const a = 1;\n";
        let result = post_tool_use(post_input("grep", json!({"pattern": "const"}), content), state)
            .await
            .unwrap();

        match result {
            PostToolUseResult::ModifyResult { content } => {
                assert!(content.starts_with("2 matches in 2 files:"));
            },
            other => panic!("expected ModifyResult, got {other:?}"),
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn post_tool_use_respects_the_master_switch() {
        let (state, dir) = temp_state("post-disabled");
        let mut config = state.config();
        config.enabled = false;
        state.set_config(config).unwrap();

        let content = "src/b.ts:3:const b = 1;\nsrc/a.ts:1:const a = 1;\n";
        let result = post_tool_use(post_input("grep", json!({}), content), state)
            .await
            .unwrap();
        assert!(matches!(result, PostToolUseResult::Allow));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tool_input_transform_ignores_other_tools() {
        let (state, dir) = temp_state("transform-other");
        let result = tool_input_transform(transform_input("read", json!({"path": "a"})), state)
            .await
            .unwrap();
        assert!(matches!(result, ToolInputTransformResult::Unchanged));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tool_input_transform_leaves_rtk_commands_alone() {
        let (state, dir) = temp_state("transform-already-rtk");
        let result = tool_input_transform(
            transform_input(compact::SHELL_TOOL, json!({"command": "rtk git status"})),
            state,
        )
        .await
        .unwrap();
        assert!(matches!(result, ToolInputTransformResult::Unchanged));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tool_input_transform_leaves_empty_commands_alone() {
        let (state, dir) = temp_state("transform-empty");
        let result = tool_input_transform(
            transform_input(compact::SHELL_TOOL, json!({"command": "   "})),
            state,
        )
        .await
        .unwrap();
        assert!(matches!(result, ToolInputTransformResult::Unchanged));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tool_input_transform_is_disabled_by_the_master_switch() {
        let (state, dir) = temp_state("transform-disabled");
        let mut config = state.config();
        config.enabled = false;
        state.set_config(config).unwrap();

        let result = tool_input_transform(
            transform_input(compact::SHELL_TOOL, json!({"command": "git status"})),
            state,
        )
        .await
        .unwrap();
        assert!(matches!(result, ToolInputTransformResult::Unchanged));
        let _ = fs::remove_dir_all(dir);
    }
}
