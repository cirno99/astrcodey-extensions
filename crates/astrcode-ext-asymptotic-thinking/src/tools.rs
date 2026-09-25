//! 三个状态机工具，逐条对照上游 `src/index.ts` 的 `registerTool`。
//!
//! | 工具 | 用途 |
//! |---|---|
//! | `asymptotic-think_set-task-info` | START 阶段设定任务画像（难度 + 大类型 + 小类型）；超限时可重估难度 |
//! | `asymptotic-think_transition` | 状态流转（目标为当前状态的动态可用集，含向前/回退） |
//! | `asymptotic-think_status` | 查询状态机位置、画像、轮次、可用流转 |
//!
//! 三个工具都只动插件自己的会话状态，因此 planner 一律声明
//! [`HostResource::Session`]，让宿主的权限系统看得见这次访问。
//!
//! 判定逻辑落在 [`apply_transition`] / [`apply_task_info`] / [`status_receipt`] 上，
//! worker handler 只是解析参数再转一次 [`Receipt`]；集成测试直接驱动前者。

use astrcode_extension_sdk::builder::ExtensionToolDefinition;
use astrcode_extension_sdk::tool::ExecutionMode;
use astrcode_extension_worker::worker_prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::machine::{self, format_next_state_hint};
use crate::state::{self, StoreRegistry};
use crate::types::{
    Difficulty, MasterTaskType, SessionState, SubTaskType, TOOL_STATUS, TOOL_TASK_INFO,
    TOOL_TRANSITION, ThinkingState, difficulty_label, flow_diagram, get_max_state_turns,
    master_task_type_label, state_label, sub_task_type_label, tool_describe_difficulty,
    tool_describe_master, tool_describe_sub,
};
use crate::worker::EXTENSION_ID;

/// 框架被禁用时三个工具的统一回执。
const DISABLED_TEXT: &str = "渐近式思考已禁用，请使用 /asymptotic-toggle 启用";

/// 一次工具调用的回执：正文 + 是否算错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub text: String,
    pub is_error: bool,
}

impl Receipt {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }

    /// 折成 S5R 的工具结果。
    pub fn into_handler_result(self) -> HandlerResult {
        tool_text(self.text, self.is_error)
    }
}

/// 枚举取值不合法时的回执（上游靠 TypeBox schema 在进入 execute 前拦下，
/// 这里补一条可读的拒绝理由，免得模型只拿到一个线缆错误）。
fn invalid_value(field: &str, raw: &str, allowed: &[&str]) -> Receipt {
    Receipt::error(format!(
        "ERROR: `{field}` 取值 `{raw}` 不合法。可用取值：{}",
        allowed.join(" | ")
    ))
}

// ── 工具 1：asymptotic-think_transition ─────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionArgs {
    to: String,
}

/// 工具 1 的注册定义。
pub fn transition_tool() -> ExtensionToolDefinition {
    tool(TOOL_TRANSITION)
        .description("渐近式思考状态机工具：变更思考进行中的状态机的当前状态")
        .parameters(json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "enum": ThinkingState::ALL.iter().map(|state| state.as_str()).collect::<Vec<_>>(),
                    "description": "下一个思考目标状态"
                }
            },
            "required": ["to"],
            "additionalProperties": false
        }))
        .build()
}

/// 执行一次状态流转并给出回执。
///
/// 成功时顺带清掉违规标记——模型确实流转了，上一轮的 `violationWarning` 就不该再出现。
pub async fn apply_transition(
    registry: &StoreRegistry,
    session_id: &str,
    to: ThinkingState,
) -> Result<Receipt, ErrorPayload> {
    let outcome = mutate(registry, session_id, move |store| {
        if !store.enabled() {
            return TransitionOutcome::Disabled;
        }
        let from = store.machine().state;
        match machine::transition(store.machine_mut(), to) {
            Ok(()) => {
                store.mark_transition_called();
                store.set_violation_pending(false);
                TransitionOutcome::Moved {
                    from,
                    after: store.machine().clone(),
                }
            },
            Err(error) => TransitionOutcome::Rejected(Rejection {
                current_state: store.machine().state,
                current_difficulty: store.machine().difficulty,
                reason: error.reason(),
            }),
        }
    })
    .await?;

    Ok(match outcome {
        TransitionOutcome::Disabled => Receipt::ok(DISABLED_TEXT),
        TransitionOutcome::Moved { from, after } => {
            Receipt::ok(transition_success_text(from, &after))
        },
        TransitionOutcome::Rejected(rejection) => {
            Receipt::error(transition_rejected_text(to, &rejection))
        },
    })
}

/// 工具 1 的 handler。
pub fn transition_handler(registry: StoreRegistry) -> ToolHandlerFn {
    tool_handler_args(move |args: TransitionArgs, ctx| {
        let registry = registry.clone();
        async move {
            let Some(to) = ThinkingState::parse(&args.to) else {
                let allowed: Vec<&str> = ThinkingState::ALL.iter().map(|s| s.as_str()).collect();
                return Ok(invalid_value("to", &args.to, &allowed).into_handler_result());
            };
            let receipt = apply_transition(&registry, ctx.session_id(), to).await?;
            Ok(receipt.into_handler_result())
        }
    })
}

/// 流转结果（跨线程返回给 handler 组装回执）。
enum TransitionOutcome {
    Disabled,
    Moved {
        from: ThinkingState,
        after: SessionState,
    },
    Rejected(Rejection),
}

/// 被拒绝时的现场快照。
pub struct Rejection {
    /// 拒绝发生时的状态（未被改动）。
    pub current_state: ThinkingState,
    /// 拒绝发生时的难度。
    pub current_difficulty: Option<Difficulty>,
    /// [`machine::TransitionError::reason`] 的文案。
    pub reason: String,
}

/// 工具 1 的成功回执。
///
/// 转入 END 时追加收尾提示；`after` 的难度已被清空，因此流转提示为空串——
/// 与上游「先流转再查难度」的顺序一致。
pub fn transition_success_text(from: ThinkingState, after: &SessionState) -> String {
    let end_suffix = if after.state == ThinkingState::END {
        "\n上一任务已完成，保持空闲等待新指令，不执行任何修改操作"
    } else {
        ""
    };
    let hint = format_next_state_hint(after.state, after.difficulty);
    format!(
        "✅ 状态流转成功：从【{}】转移到【{}】。\n{hint}{end_suffix}",
        state_label(from),
        state_label(after.state)
    )
}

/// 工具 1 的拒绝回执。
pub fn transition_rejected_text(to: ThinkingState, rejection: &Rejection) -> String {
    let hint = format_next_state_hint(rejection.current_state, rejection.current_difficulty);
    format!(
        "ERROR: 状态流转被拒绝：当前处于【{}】状态，无法转移到【{}】。\n原因：{}。\n{hint}",
        state_label(rejection.current_state),
        state_label(to),
        rejection.reason
    )
}

// ── 工具 2：asymptotic-think_set-task-info ──────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskInfoArgs {
    #[serde(rename = "difficulty")]
    difficulty: String,
    #[serde(rename = "masterTaskType")]
    master_task_type: String,
    #[serde(rename = "subTaskType")]
    sub_task_type: String,
}

/// 工具 2 的注册定义。
pub fn task_info_tool() -> ExtensionToolDefinition {
    tool(TOOL_TASK_INFO)
        .description("渐近式思考状态机工具：设定用户指令的任务难度和大任务类型+小任务类型")
        .parameters(json!({
            "type": "object",
            "properties": {
                "difficulty": {
                    "type": "string",
                    "enum": Difficulty::ALL.iter().map(|value| value.as_str()).collect::<Vec<_>>(),
                    "description": tool_describe_difficulty()
                },
                "masterTaskType": {
                    "type": "string",
                    "enum": MasterTaskType::ALL.iter().map(|value| value.as_str()).collect::<Vec<_>>(),
                    "description": tool_describe_master()
                },
                "subTaskType": {
                    "type": "string",
                    "enum": SubTaskType::ALL.iter().map(|value| value.as_str()).collect::<Vec<_>>(),
                    "description": tool_describe_sub()
                }
            },
            "required": ["difficulty", "masterTaskType", "subTaskType"],
            "additionalProperties": false
        }))
        .build()
}

/// 设定任务画像并给出回执。
pub async fn apply_task_info(
    registry: &StoreRegistry,
    session_id: &str,
    difficulty: Difficulty,
    master: MasterTaskType,
    sub: SubTaskType,
) -> Result<Receipt, ErrorPayload> {
    let outcome = mutate(registry, session_id, move |store| {
        if !store.enabled() {
            return TaskInfoOutcome::Disabled;
        }
        match machine::set_task_info(store.machine_mut(), difficulty, master, sub) {
            Ok(()) => TaskInfoOutcome::Set,
            Err(error) => TaskInfoOutcome::Rejected {
                current_state: store.machine().state,
                reason: error.reason(),
            },
        }
    })
    .await?;

    Ok(match outcome {
        TaskInfoOutcome::Disabled => Receipt::ok(DISABLED_TEXT),
        TaskInfoOutcome::Set => Receipt::ok(task_info_success_text(difficulty, master, sub)),
        TaskInfoOutcome::Rejected {
            current_state,
            reason,
        } => Receipt::error(task_info_rejected_text(current_state, reason)),
    })
}

/// 工具 2 的 handler。
pub fn task_info_handler(registry: StoreRegistry) -> ToolHandlerFn {
    tool_handler_args(move |args: TaskInfoArgs, ctx| {
        let registry = registry.clone();
        async move {
            // 上游 `prepareArguments` 把三个枚举统一转大写再校验，这里由
            // `parse` 的宽松匹配承担同一件事。
            let Some(difficulty) = Difficulty::parse(&args.difficulty) else {
                let allowed: Vec<&str> = Difficulty::ALL.iter().map(|v| v.as_str()).collect();
                return Ok(invalid_value("difficulty", &args.difficulty, &allowed)
                    .into_handler_result());
            };
            let Some(master) = MasterTaskType::parse(&args.master_task_type) else {
                let allowed: Vec<&str> = MasterTaskType::ALL.iter().map(|v| v.as_str()).collect();
                return Ok(invalid_value("masterTaskType", &args.master_task_type, &allowed)
                    .into_handler_result());
            };
            let Some(sub) = SubTaskType::parse(&args.sub_task_type) else {
                let allowed: Vec<&str> = SubTaskType::ALL.iter().map(|v| v.as_str()).collect();
                return Ok(invalid_value("subTaskType", &args.sub_task_type, &allowed)
                    .into_handler_result());
            };

            let receipt =
                apply_task_info(&registry, ctx.session_id(), difficulty, master, sub).await?;
            Ok(receipt.into_handler_result())
        }
    })
}

/// 设定画像的结果。
enum TaskInfoOutcome {
    Disabled,
    Set,
    Rejected {
        current_state: ThinkingState,
        reason: &'static str,
    },
}

/// 工具 2 的成功回执。
pub fn task_info_success_text(
    difficulty: Difficulty,
    master: MasterTaskType,
    sub: SubTaskType,
) -> String {
    format!(
        "✅ 任务画像已设定：{}-{} | {}难度。\n{}\n最后调用 {TOOL_TRANSITION} 工具流转状态。",
        master_task_type_label(master),
        sub_task_type_label(sub),
        difficulty_label(difficulty),
        format_next_state_hint(ThinkingState::START, Some(difficulty))
    )
}

/// 工具 2 的拒绝回执。
pub fn task_info_rejected_text(current_state: ThinkingState, reason: &str) -> String {
    format!(
        "X 任务信息设定失败：仅可在 START 阶段或状态超限时设定任务信息，当前处于【{}】状态。\n原因：{reason}。\n报告不为START且未超限的原因，立刻继续完成上一轮任务。完成后重新调用本工具。",
        state_label(current_state)
    )
}

// ── 工具 3：asymptotic-think_status ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArgs {}

/// 工具 3 的注册定义。无参数，纯查询。
pub fn status_tool() -> ExtensionToolDefinition {
    tool(TOOL_STATUS)
        .description(
            "渐近式思考状态机工具：查询当前状态机的完整状态(当前阶段、任务画像、轮次计数、\
             允许的下一步转移、六态流程图)",
        )
        .parameters(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }))
        .execution_mode(ExecutionMode::Parallel)
        .build()
}

/// 查询回执。
pub fn status_receipt(registry: &StoreRegistry, session_id: &str) -> Receipt {
    let snapshot = {
        let store = registry.session(EXTENSION_ID, session_id);
        let guard = state::lock(&store);
        guard.snapshot()
    };
    if !snapshot.enabled {
        return Receipt::ok(DISABLED_TEXT);
    }
    Receipt::ok(render_status(&snapshot.machine))
}

/// 工具 3 的 handler。
pub fn status_handler(registry: StoreRegistry) -> ToolHandlerFn {
    tool_handler_args(move |_args: StatusArgs, ctx| {
        let registry = registry.clone();
        async move { Ok(status_receipt(&registry, ctx.session_id()).into_handler_result()) }
    })
}

/// `asymptotic-think_status` 的正文。
pub fn render_status(machine: &SessionState) -> String {
    let state = machine.state;
    let difficulty = machine.difficulty;
    let max_turns = get_max_state_turns(state, difficulty);
    let allowed = machine::get_allowed_targets(state, difficulty);

    let difficulty_text = difficulty.map(difficulty_label).unwrap_or("未设定");
    let master_text = machine
        .master_task_type
        .map(master_task_type_label)
        .unwrap_or("未设定");
    let sub_text = machine
        .sub_task_type
        .map(sub_task_type_label)
        .unwrap_or("未设定");
    let allowed_text = allowed
        .iter()
        .map(|target| format!("{}({})", state_label(*target), target))
        .collect::<Vec<_>>()
        .join(" | ");

    format!(
        "## 状态机查询结果\n- **当前状态**: {} ({state})\n- **任务画像**: {master_text}-{sub_text} | {difficulty_text}难度\n- **任务轮次**: 第 {} 轮\n- **状态内轮次**: {}/{max_turns}\n- **允许的下一步**: {allowed_text}\n\n### 六态流程图\n```\n{}\n```",
        state_label(state),
        machine.task_turn_count,
        machine.state_turn_count,
        flow_diagram()
    )
}

// ── 共用 ────────────────────────────────────────────────────────────────────

/// 三个工具都只动插件自己的会话状态，planner 一律声明 [`HostResource::Session`]。
fn session_planner<A: serde::de::DeserializeOwned + Send + 'static>() -> ToolPlannerFn {
    tool_planner_args(|_args: A, _ctx| async move { Ok(ToolPlan::host(HostResource::Session)) })
}

/// 工具 1 的 planner。
pub fn transition_planner() -> ToolPlannerFn {
    session_planner::<TransitionArgs>()
}

/// 工具 2 的 planner。
pub fn task_info_planner() -> ToolPlannerFn {
    session_planner::<TaskInfoArgs>()
}

/// 工具 3 的 planner。
pub fn status_planner() -> ToolPlannerFn {
    session_planner::<StatusArgs>()
}

/// 在阻塞线程池上改会话状态并落盘。
async fn mutate<T, F>(registry: &StoreRegistry, session_id: &str, f: F) -> Result<T, ErrorPayload>
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionState {
        SessionState {
            state: ThinkingState::DESIGN,
            difficulty: Some(Difficulty::MODERATE),
            master_task_type: Some(MasterTaskType::CODING),
            sub_task_type: Some(SubTaskType::RUST_DEV),
            task_turn_count: 2,
            state_turn_count: 3,
            last_transition_time: 0,
            visited: vec![ThinkingState::DEEP_UNDERSTAND],
        }
    }

    #[test]
    fn status_reports_the_full_picture() {
        let text = render_status(&sample());
        assert!(text.contains("- **当前状态**: 方案设计 (DESIGN)"));
        assert!(text.contains("- **任务画像**: 编程类-Rust开发 | 中等难度"));
        assert!(text.contains("- **任务轮次**: 第 2 轮"));
        assert!(text.contains("- **状态内轮次**: 3/1000"));
        assert!(
            text.contains(
                "- **允许的下一步**: 执行(EXECUTE) | 深度理解(DEEP_UNDERSTAND) | 自检验证(VERIFY)"
            )
        );
        assert!(text.contains("启动 → 深度理解 → 方案设计 → 执行 → 自检验证 → 结束"));
    }

    #[test]
    fn status_marks_an_unset_profile() {
        let text = render_status(&SessionState::default());
        assert!(text.contains("- **任务画像**: 未设定-未设定 | 未设定难度"));
        assert!(text.contains("- **状态内轮次**: 0/1"));
    }

    /// END 没有可用流转，`允许的下一步` 必须是空串而不是 panic。
    #[test]
    fn status_handles_end() {
        let mut machine = sample();
        machine.state = ThinkingState::END;
        machine.difficulty = None;
        let text = render_status(&machine);
        assert!(text.contains("- **允许的下一步**: \n"));
        assert_eq!(get_max_state_turns(ThinkingState::END, None), 0);
    }

    #[test]
    fn invalid_enum_values_get_a_readable_rejection() {
        let receipt = invalid_value("to", "idle", &["START", "DESIGN"]);
        assert!(receipt.is_error);
        assert!(receipt.text.starts_with("ERROR: `to` 取值 `idle` 不合法。可用取值：START | DESIGN"));
    }

    /// 流转成功的回执：转入 END 时追加收尾提示，且此时难度已被清空。
    #[test]
    fn transition_success_text_matches_upstream() {
        let after = sample();
        let text = transition_success_text(ThinkingState::DEEP_UNDERSTAND, &after);
        assert!(text.starts_with("✅ 状态流转成功：从【深度理解】转移到【方案设计】。"));
        assert!(text.contains("本阶段完成可前移至[执行(EXECUTE)、自检验证(VERIFY)]状态；"));
        assert!(!text.contains("上一任务已完成"));

        let mut ended = after.clone();
        ended.state = ThinkingState::END;
        ended.difficulty = None;
        let text = transition_success_text(ThinkingState::VERIFY, &ended);
        assert!(text.ends_with("上一任务已完成，保持空闲等待新指令，不执行任何修改操作"));
    }

    #[test]
    fn transition_rejected_text_matches_upstream() {
        let rejection = Rejection {
            current_state: ThinkingState::DESIGN,
            current_difficulty: Some(Difficulty::MODERATE),
            reason: "DESIGN 状态下不能转移到 END。可转移: EXECUTE, DEEP_UNDERSTAND, VERIFY"
                .to_owned(),
        };
        let text = transition_rejected_text(ThinkingState::END, &rejection);
        assert!(
            text.starts_with("ERROR: 状态流转被拒绝：当前处于【方案设计】状态，无法转移到【结束】。")
        );
        assert!(text.contains("原因：DESIGN 状态下不能转移到 END。"));
        assert!(text.contains("本阶段完成可前移至[执行(EXECUTE)、自检验证(VERIFY)]状态；"));
    }

    #[test]
    fn task_info_texts_match_upstream() {
        let success = task_info_success_text(
            Difficulty::TRIVIAL,
            MasterTaskType::CODING,
            SubTaskType::RUST_DEV,
        );
        assert!(success.starts_with("✅ 任务画像已设定：编程类-Rust开发 | 微不足道难度。"));
        assert!(
            success.contains("本阶段完成可前移至[深度理解(DEEP_UNDERSTAND)、执行(EXECUTE)]状态；")
        );
        assert!(success.ends_with("最后调用 asymptotic-think_transition 工具流转状态。"));

        let rejected = task_info_rejected_text(
            ThinkingState::EXECUTE,
            "仅可在 START 阶段或状态超限时设定任务信息",
        );
        assert!(rejected.starts_with(
            "X 任务信息设定失败：仅可在 START 阶段或状态超限时设定任务信息，当前处于【执行】状态。"
        ));
        assert!(rejected.contains("立刻继续完成上一轮任务。完成后重新调用本工具。"));
    }

    #[test]
    fn the_tool_schemas_declare_every_enum_variant() {
        let transition = transition_tool();
        let variants = transition.definition().parameters["properties"]["to"]["enum"]
            .as_array()
            .expect("enum 必须是数组")
            .len();
        assert_eq!(variants, ThinkingState::ALL.len());
        assert_eq!(transition.definition().parameters["required"], json!(["to"]));

        let task_info = task_info_tool();
        assert_eq!(
            task_info.definition().parameters["properties"]["difficulty"]["enum"]
                .as_array()
                .expect("enum 必须是数组")
                .len(),
            Difficulty::ALL.len()
        );
        assert_eq!(
            task_info.definition().parameters["required"],
            json!(["difficulty", "masterTaskType", "subTaskType"])
        );

        let status = status_tool();
        assert_eq!(status.definition().parameters["type"], json!("object"));
        assert!(status.definition().parameters.get("required").is_none());
    }
}
