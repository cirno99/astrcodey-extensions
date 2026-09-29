//! 钩子的适配层。
//!
//! | 钩子 | 职责 |
//! |---|---|
//! | `continue_after_stop` | 核心：判定该不该续跑，注入「继续」并再跑一个 step |
//! | `pre_tool_use` | 拦截提问类工具，按推荐项自动作答 |
//! | `post_tool_use` | 记录工具活动，供空转熔断判定 |
//! | `UserPromptSubmit` | 人工接手：续跑预算归零 |
//! | `SessionStart` | 重新读取配置，让外部手工编辑的 config.json 生效 |
//! | `turn_end` | turn 失败后自动重试：读持久事件日志找新错误，分类后重新排队 |
//!
//! 钩子只做「取参 → 判定 → 落状态 → 映射回 S5R 结果」；判定在 [`crate::plan`]，
//! 状态在 [`crate::state`]。

use std::{future::Future, sync::Arc, time::Duration};

use astrcode_extension_worker::worker_prelude::*;
use serde_json::Value;

use crate::{
    plan::{self, Decision, StopReason},
    state::SharedState,
};

/// 续跑判定后的处置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continuation {
    /// 不续跑（闸门关着，或判定为到顶/空转）。
    Stop,
    /// 注入这段文本并再跑一个 step。
    Inject { text: String },
}

/// 判定要不要续跑，并把「该停下」的原因落进运行期状态。
///
/// 与 [`continue_run`] 分开是为了可测：这段判定不碰宿主的注入通道，因此不需要构造
/// `WorkerInvocationContext`（它在测试里造不出来）。真正的注入由集成测试用模拟宿主覆盖。
pub async fn plan_continuation(
    state: &Arc<SharedState>,
    session_id: &str,
    assistant_text: &str,
) -> Continuation {
    if !state.session_enabled(session_id).await {
        return Continuation::Stop;
    }

    // 先结算本步活动再判定：`idle_streak` 与 `no_progress_streak` 必须已经把「本步有没有
    // 产生工具调用」「本步回复有没有新内容」算进去。
    let progress = state.settle_step(session_id, assistant_text);
    let config = state.config();
    match plan::decide(&config, &progress) {
        Decision::Stop(reason) => {
            state.record_stop(session_id, reason);
            Continuation::Stop
        }
        Decision::Continue { text } => Continuation::Inject { text },
    }
}

/// `continue_after_stop`：模型自然停下后再推一把。
///
/// **本函数不返回 `Err`。** 宿主的 `emit_continue_after_stop` 会把 handler 的错误直接上抛成
/// turn 失败（`astrcode-extensions::runner` 里那一句 `?`），也就是说插件内部的任何异常都会
/// 打断用户正在跑的那个 turn。判定不出来就停下：停下只是少了一次续跑，报错是任务被打断。
pub async fn continue_run(
    state: &Arc<SharedState>,
    input: &ContinueAfterStopHookInput,
    ctx: &WorkerInvocationContext,
) -> Result<ContinueAfterStopResult, ErrorPayload> {
    continue_with(
        state,
        &input.session_id,
        &input.assistant_text,
        |text| async move {
            ctx.defer_context(text)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )
    .await
}

/// 续跑主体：判定、注入、结算。
///
/// 注入通道做成参数是为了可测——`WorkerInvocationContext` 在插件外部构造不出来（字段私有、
/// 没有公开构造器），而这里真正要验收的是「调了哪个宿主操作、载荷长什么样」，不是那个上下文
/// 本身。集成测试因此能用真实的 `HostClient::session_control()` 打模拟宿主走一遍。
pub async fn continue_with<F, Fut>(
    state: &Arc<SharedState>,
    session_id: &str,
    assistant_text: &str,
    inject: F,
) -> Result<ContinueAfterStopResult, ErrorPayload>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let Continuation::Inject { text } = plan_continuation(state, session_id, assistant_text).await
    else {
        return Ok(ContinueAfterStopResult::EndTurn);
    };

    match inject(text).await {
        Ok(()) => {
            state.note_continuation(session_id);
            Ok(ContinueAfterStopResult::ContinueOneStep)
        }
        Err(detail) => {
            // 注入失败就不能续跑：只多跑一个 step 而不喂话，模型会原地复读同一段总结，
            // 比直接停下更浪费额度。
            state.record_stop(session_id, StopReason::InjectFailed { detail });
            Ok(ContinueAfterStopResult::EndTurn)
        }
    }
}

/// `pre_tool_use`：拦截提问类工具调用，按推荐项自动作答。
///
/// 判定顺序按代价排：先看本地配置与工具名单，最后才碰宿主的会话状态。
pub async fn answer_question(
    state: &Arc<SharedState>,
    input: &ToolUseHookInput,
) -> Result<PreToolUseResult, ErrorPayload> {
    let config = state.config();
    if !config.answer.enabled || !plan::is_answer_tool(&input.tool_name, &config.answer.tools) {
        return Ok(PreToolUseResult::Allow);
    }
    if !state.session_enabled(&input.session_id).await {
        return Ok(PreToolUseResult::Allow);
    }

    // 解析不出来时给一个空摘要而不是放行：放行会让这一轮卡在等人的弹窗上，而
    // `block_reason` 对空摘要的处理是「请按任务最合理的默认方案继续」，仍然把球踢给模型。
    let choices = plan::recommended_choices(&input.tool_input).unwrap_or_default();
    state.count_answer();
    Ok(PreToolUseResult::Block {
        reason: plan::block_reason(&input.tool_name, &choices),
    })
}

/// `post_tool_use`：工具活动是「模型还在干活」的唯一硬信号。
///
/// 全局关着时不记录：那种情况下续跑永远不会发生，记了也只是让运行期表做无意义的淘汰。
pub fn note_tool_call(state: &Arc<SharedState>, input: &PostToolUseHookInput) -> PostToolUseResult {
    if state.config().enabled {
        state.note_tool_call(&input.session_id);
    }
    PostToolUseResult::Allow
}

/// `UserPromptSubmit`：人工接手，续跑预算归零。
///
/// 这是「到顶只停止续跑、开关保持开启」能自洽的那一半：上限的语义是**单次人工 turn 的
/// 预算**，人插过一句话就重新给满。
///
/// 本插件自己注入的消息走 `defer_context`，在 turn 内被吸收，**不会**派发这个事件，
/// 因此这里只会被真正的人工输入触发。
pub fn reset_budget_on_prompt(state: &Arc<SharedState>, event: &Value) {
    if let Some(session_id) = session_id_of(event) {
        state.reset_budget(session_id);
    }
}

/// `SessionStart`：重新读取配置。
pub fn reload_config(state: &Arc<SharedState>) -> Option<String> {
    state.reload()
}

/// 每页读多少条持久事件。
const EVENTS_PAGE_LIMIT: usize = 50;

/// 首次建立游标时最多翻多少页。
///
/// 够长会话一路走到尾，又不至于让 turn 收尾无界地读下去。到顶就记下游标收工——见
/// [`read_new_failure`] 里对「首次只建游标」的说明。
const EVENTS_BOOTSTRAP_PAGES: usize = 1_000;

/// `turn_end`：turn 挂掉后自动重试。
///
/// # 为什么必须在这里就把消息排出去
///
/// `turn_end` 是在 turn 任务内部 await 的（宿主 `finalize_turn_on_error`），此刻 turn 还没
/// durable 完成、执行槽仍被占用。所以：
///
/// - **不能**用 `inject_or_start`：它会注进**正在失败**的那个 turn，消息随 turn 一起丢掉；
/// - **不能**看 `session.control.state().phase`：持久事件只 append 不 sync，投影可能还是旧
///   phase，判不出成败；
/// - 只能 `queue_or_start`：turn 仍活跃时它入队，由完成 watcher 排空后起新 turn；在
///   「已完成未 settle」的窗口里它会先 settle 再启动队首（`turn_scheduler/delivery.rs`）。
///
/// # 本函数不返回 `Err`
///
/// 同 [`continue_run`]：宿主的钩子分发会把 handler 的错误上抛成 turn 失败，插件内部的异常
/// 不该打断用户正在跑的 turn。判不出来就什么都不做。
pub async fn recover_failed_turn(state: &Arc<SharedState>, session_id: &str) {
    let config = state.config();
    if config.retry_max == 0 || !state.session_enabled(session_id).await {
        return;
    }

    let Some(message) = read_new_failure(state, session_id).await else {
        return;
    };
    state.note_failure(session_id, &plan::summarize(&message));

    let class = plan::classify_failure(&message);
    let progress = state.progress(session_id);
    match plan::decide_retry(&config, &progress, class, &message) {
        plan::RetryDecision::Stop(reason) => state.record_stop(session_id, reason),
        plan::RetryDecision::Retry { delay_ms } => {
            // 退避睡在 turn 收尾路径上，所以上限被 `retryMaxDelayMs` 卡着（默认 5s）：
            // 无人值守下等一会儿无所谓，把用户的新输入堵太久就不行。
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            retry_once(state, session_id, &config.continue_text).await;
        }
    }
}

/// 把重试消息排进宿主队列；成功记一次重试，失败记下原因停下。
///
/// 用 `queue_or_start` 而不是 `inject_or_start`，原因见 [`recover_failed_turn`]。
async fn retry_once(state: &Arc<SharedState>, session_id: &str, text: &str) {
    let request = HostSessionInputRequest {
        target_session_id: session_id.to_owned(),
        content: text.to_owned(),
    };
    let outcome = HostClient::session_control()
        .queue_or_start(request)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string());
    match outcome {
        Ok(()) => state.note_retry(session_id),
        Err(detail) => state.record_stop(session_id, StopReason::RetryFailed { detail }),
    }
}

/// 读持久事件日志，返回**自上次检测以来**新出现的错误文本。
///
/// 游标单调推进：即使这一轮没找到错误也要推到日志尾部，否则每次 `turn_end` 都要从头重扫。
///
/// 首次（没有游标）**不重扫历史**：扩展重载后从旧日志里翻出一条早就处理过的错误，去重试一个
/// 其实已经成功的 turn，比漏掉一次重试更糟。唯一的例外见函数末尾。
async fn read_new_failure(state: &Arc<SharedState>, session_id: &str) -> Option<String> {
    // 能力没声明或上下文不对时，宿主会在调用点回一个错误，由下面统一放弃本轮检测。
    let client = HostClient::session_history();
    let stored = state.event_cursor(session_id);
    let bootstrapping = stored.is_none();
    let mut cursor = stored;
    let mut failure = None;
    // 日志最后一条事件若是错误，这里就是它的文本；被任何一条非错误事件覆盖成 `None`。
    let mut tail = None;
    let mut pages = 0usize;
    loop {
        let request = HostSessionEventsPageRequest {
            session_id: session_id.to_owned(),
            cursor: cursor.clone(),
            limit: EVENTS_PAGE_LIMIT,
        };
        let Ok(page) = client.events_page(request).await else {
            return None;
        };
        for event in &page.events {
            match error_message(event) {
                Some(message) => {
                    failure = Some(message.clone());
                    tail = Some(message);
                }
                None => tail = None,
            }
        }
        cursor = Some(page.next_cursor.clone());
        pages += 1;
        if !page.has_more || pages >= EVENTS_BOOTSTRAP_PAGES {
            break;
        }
    }

    state.set_event_cursor(session_id, cursor);
    // 首次没有游标时不重扫历史，唯一的例外是「错误就是日志的最后一条事件」——那说明这一轮
    // 失败之后什么都没写下来，正是「刚挂掉」的形状；历史错误后面总跟着别的事件。
    if bootstrapping { tail } else { failure }
}

/// 从一条持久事件里取出错误文本；不是错误事件就返回 `None`。
///
/// 直接读原始 JSON 而不反序列化成 `DurableEventPayload`：宿主给事件加字段不该让本插件
/// 解析失败，而这里只需要 `type` 与 `message` 两个字段。
fn error_message(event: &HostSessionEvent) -> Option<String> {
    let payload = event.payload.as_object()?;
    if payload.get("type")?.as_str()? != "error_occurred" {
        return None;
    }
    payload.get("message")?.as_str().map(str::to_owned)
}

/// 从生命周期钩子的原始载荷里取出会话 id。
///
/// 生命周期钩子没有类型化构造器——`worker_prelude` 只导出了带类型的那些钩子——所以这里
/// 直接读原始 JSON。只取需要的字段、不设 `deny_unknown_fields`：宿主给载荷加字段不该让
/// 本插件失效。
pub(crate) fn session_id_of(event: &Value) -> Option<&str> {
    event
        .get("input")
        .unwrap_or(event)
        .get("session_id")?
        .as_str()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::state::{Progress, SessionSwitch};

    /// 造一个「会话已开、全局已开」的状态，并把会话开关写进内存缓存绕开宿主往返。
    fn enabled_state(name: &str) -> (Arc<SharedState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-sleep-continue-hook-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (state, warning) = SharedState::load(crate::config::store_at(dir.join("config.json")));
        assert!(warning.is_none());
        state.cache_session_switch_for_tests("s-1", SessionSwitch::On);
        (state, dir)
    }

    fn tool_input(tool_name: &str, input: Value) -> ToolUseHookInput {
        serde_json::from_value(json!({
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": "any-model", "provider_kind": "" },
            "tool_call_id": "call-1",
            "tool_name": tool_name,
            "tool_input": input,
            "available_tools": []
        }))
        .expect("工具载荷应可解析")
    }

    #[tokio::test]
    async fn a_closed_session_never_continues() {
        let (state, dir) = enabled_state("closed");
        state.cache_session_switch_for_tests("s-1", SessionSwitch::Off);

        assert_eq!(
            plan_continuation(&state, "s-1", "不该被结算的一步").await,
            Continuation::Stop
        );
        // 闸门关着时连结算都不做：工具调用计数留给人工接手时的归零。
        assert_eq!(state.progress("s-1"), Progress::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 干过活的一步（有工具调用）：续跑用配置的「继续」。
    #[tokio::test]
    async fn an_open_session_continues_with_the_configured_text() {
        let (state, dir) = enabled_state("open");
        state.note_tool_call("s-1");

        assert_eq!(
            plan_continuation(&state, "s-1", "改完了 a.rs。").await,
            Continuation::Inject {
                text: "继续".to_owned()
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 没干活的一步（没有工具调用）：换用纠正提示，而不是裸的「继续」。
    #[tokio::test]
    async fn a_step_without_tool_calls_gets_the_nudge_text() {
        let (state, dir) = enabled_state("nudge");

        assert_eq!(
            plan_continuation(&state, "s-1", "我打算先看看 a.rs。").await,
            Continuation::Inject {
                text: crate::config::DEFAULT_NUDGE_TEXT.to_owned()
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 复读熔断：回复与上一步逐字相同（且没有工具调用）就停下，不再喂话。
    #[tokio::test]
    async fn the_no_progress_breaker_stops_repeats() {
        let (state, dir) = enabled_state("repeat");
        let mut config = state.config();
        // 只验复读熔断，别让空转先命中。
        config.idle_stop = 0;
        state.set_config(config).expect("写配置");

        // 第一次：这段内容是新出现的，只算「没干活」，续跑。
        assert!(matches!(
            plan_continuation(&state, "s-1", "Let me output.").await,
            Continuation::Inject { .. }
        ));
        state.note_continuation("s-1");

        // 第二次：与上一步规范化后完全相同 → 无进展，熔断。
        assert_eq!(
            plan_continuation(&state, "s-1", "  Let me   output. ").await,
            Continuation::Stop
        );
        assert_eq!(
            state.progress("s-1").stop_reason,
            Some(StopReason::NoProgress {
                streak: 1,
                limit: 1
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 空回复同样算「无新内容」。
    #[tokio::test]
    async fn an_empty_reply_counts_as_no_progress() {
        let (state, dir) = enabled_state("empty");
        let mut config = state.config();
        config.idle_stop = 0;
        state.set_config(config).expect("写配置");

        assert_eq!(
            plan_continuation(&state, "s-1", "   ").await,
            Continuation::Stop
        );
        assert_eq!(
            state.progress("s-1").stop_reason,
            Some(StopReason::NoProgress {
                streak: 1,
                limit: 1
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 到顶只停止续跑，并把原因留下——`/sleep status` 是唯一能看到它的地方。
    #[tokio::test]
    async fn the_cap_stops_and_records_a_reason() {
        let (state, dir) = enabled_state("cap");
        let mut config = state.config();
        config.max = 1;
        state.set_config(config).expect("写配置");

        assert_eq!(
            plan_continuation(&state, "s-1", "先看 a.rs。").await,
            Continuation::Inject {
                text: crate::config::DEFAULT_NUDGE_TEXT.to_owned()
            }
        );
        state.note_continuation("s-1");

        assert_eq!(
            plan_continuation(&state, "s-1", "看了 b.rs。").await,
            Continuation::Stop
        );
        assert_eq!(
            state.progress("s-1").stop_reason,
            Some(StopReason::CapReached { count: 1, max: 1 })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 空转熔断：连着几次续跑都没产生工具调用就停。
    #[tokio::test]
    async fn the_idle_breaker_stops_and_records_a_reason() {
        let (state, dir) = enabled_state("idle");
        let mut config = state.config();
        config.idle_stop = 2;
        state.set_config(config).expect("写配置");

        // 第一次停下：空转链 1，仍续跑。
        assert!(matches!(
            plan_continuation(&state, "s-1", "先看 a.rs。").await,
            Continuation::Inject { .. }
        ));
        state.note_continuation("s-1");

        // 第二次停下：空转链到 2，触发熔断。
        assert_eq!(
            plan_continuation(&state, "s-1", "再看 b.rs。").await,
            Continuation::Stop
        );
        assert_eq!(
            state.progress("s-1").stop_reason,
            Some(StopReason::Idle {
                streak: 2,
                limit: 2
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 干过活就不算空转：工具调用把空转链断开。
    #[tokio::test]
    async fn tool_activity_resets_the_idle_streak() {
        let (state, dir) = enabled_state("activity");
        let mut config = state.config();
        config.idle_stop = 2;
        state.set_config(config).expect("写配置");

        assert!(matches!(
            plan_continuation(&state, "s-1", "先看 a.rs。").await,
            Continuation::Inject { .. }
        ));
        state.note_tool_call("s-1");

        assert!(matches!(
            plan_continuation(&state, "s-1", "看了 b.rs。").await,
            Continuation::Inject { .. }
        ));
        assert_eq!(state.progress("s-1").idle_streak, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_answer_hook_blocks_a_listed_tool_with_the_recommended_choice() {
        let (state, dir) = enabled_state("answer");
        let input = tool_input(
            "askUser",
            json!({
                "questions": [{
                    "question": "用哪个？",
                    "header": "选择",
                    "options": [
                        { "label": "甲", "description": "", "recommended": true },
                        { "label": "乙", "description": "" }
                    ]
                }]
            }),
        );

        let PreToolUseResult::Block { reason } = answer_question(&state, &input).await.unwrap()
        else {
            panic!("askUser 应当被拦下");
        };
        assert!(reason.contains("-「用哪个？」→ 已选「甲」"), "{reason}");
        assert_eq!(state.stats().answers, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn the_answer_hook_passes_through_other_tools() {
        let (state, dir) = enabled_state("pass");
        let input = tool_input("shell", json!({}));

        assert!(matches!(
            answer_question(&state, &input).await.unwrap(),
            PreToolUseResult::Allow
        ));
        assert_eq!(state.stats().answers, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 会话没开时不该拦提问：人还在电脑前，弹窗本来就是给她的。
    #[tokio::test]
    async fn the_answer_hook_passes_through_when_the_session_is_closed() {
        let (state, dir) = enabled_state("answer-closed");
        state.cache_session_switch_for_tests("s-1", SessionSwitch::Off);
        let input = tool_input("askUser", json!({ "questions": [] }));

        assert!(matches!(
            answer_question(&state, &input).await.unwrap(),
            PreToolUseResult::Allow
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 解析不出选项也要拦下：放行会让这一轮卡在等人的弹窗上。
    #[tokio::test]
    async fn the_answer_hook_still_blocks_when_the_input_is_unparsable() {
        let (state, dir) = enabled_state("answer-unparsable");
        let input = tool_input("askUser", json!({ "questions": [] }));

        let PreToolUseResult::Block { reason } = answer_question(&state, &input).await.unwrap()
        else {
            panic!("解析不出选项时也应当拦下");
        };
        assert!(reason.contains("最合理的默认方案"), "{reason}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 应答开关关掉后提问一律放行。
    #[tokio::test]
    async fn the_answer_hook_can_be_disabled() {
        let (state, dir) = enabled_state("answer-off");
        let mut config = state.config();
        config.answer.enabled = false;
        state.set_config(config).expect("写配置");

        let input = tool_input("askUser", json!({ "questions": [] }));
        assert!(matches!(
            answer_question(&state, &input).await.unwrap(),
            PreToolUseResult::Allow
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tool_results_are_counted() {
        let (state, dir) = enabled_state("count");
        let input: PostToolUseHookInput = serde_json::from_value(json!({
            "session_id": "s-1",
            "working_dir": "/tmp",
            "model": { "profile_name": "", "model": "any-model", "provider_kind": "" },
            "tool_call_id": "call-1",
            "tool_name": "shell",
            "tool_input": {},
            "tool_result": { "content": "ok", "is_error": false, "metadata": {} },
            "is_error": false
        }))
        .expect("工具结果载荷应可解析");

        assert!(matches!(
            note_tool_call(&state, &input),
            PostToolUseResult::Allow
        ));
        assert_eq!(state.progress("s-1").tool_calls, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_user_prompt_clears_the_budget() {
        let (state, dir) = enabled_state("prompt");
        state.note_continuation("s-1");
        state.note_continuation("s-1");
        assert_eq!(state.progress("s-1").continuations, 2);

        reset_budget_on_prompt(
            &state,
            &json!({ "on": "user_prompt_submit", "input": { "session_id": "s-1" } }),
        );
        assert_eq!(state.progress("s-1").continuations, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 载荷形状不认识时不该 panic，也不该把预算算到别的会话头上。
    #[test]
    fn an_unrecognised_lifecycle_payload_is_ignored() {
        assert_eq!(session_id_of(&json!({})), None);
        assert_eq!(session_id_of(&json!({ "input": {} })), None);
        assert_eq!(
            session_id_of(&json!({ "input": { "session_id": 7 } })),
            None
        );
        // 没有 `input` 包层时读顶层，兼容不带包层的载荷。
        assert_eq!(session_id_of(&json!({ "session_id": "s-1" })), Some("s-1"));
        assert_eq!(
            session_id_of(&json!({ "input": { "session_id": "s-1" } })),
            Some("s-1")
        );
    }
}
