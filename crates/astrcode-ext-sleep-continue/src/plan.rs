//! 纯判定逻辑：给定配置与运行期状态，决定续跑还是停下、生成给模型看的拦截原因。
//!
//! 本模块**不碰宿主**，因此可以逐条单元测试。宿主交互全部在 [`crate::hook`]。
//!
//! 判定拆成两半是刻意的：`plan` 回答「该不该继续」，`hook` 回答「怎么把它做出来」。
//! 混在一起的话，最需要证据的那些边界（到顶、空转、注入失败回落）就得靠集成测试才能覆盖。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use crate::{config::Config, state::Progress};

/// 停止续跑的原因。
///
/// 停下时必须留下原因：没有 UI 通知通道（S5R 注册不了状态栏），`/sleep status` 是唯一
/// 能看到「为什么它不跑了」的地方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// 本次人工 turn 的续跑次数到顶。
    CapReached { count: u32, max: u32 },
    /// 连续多次续跑都没有产生工具调用，判定为空转。
    Idle { streak: u32, limit: u32 },
    /// 注入续跑消息失败。只续跑不喂话会让模型原地复读，所以偏向停下。
    InjectFailed { detail: String },
    /// 连续多次续跑都没有新内容（回复与上一次重复或为空），判定为复读。
    NoProgress { streak: u32, limit: u32 },
    /// 遇到不该重试的错误（模型不存在、鉴权失败等），直接停下等人处理。
    FatalError { message: String },
    /// 本次人工 turn 的重试次数到顶。
    RetryExhausted { count: u32, max: u32 },
    /// 重试投递失败：排队输入没能送达宿主。
    RetryFailed { detail: String },
}

impl StopReason {
    /// 中文描述，用于 `/sleep status`。
    pub fn text(&self) -> String {
        match self {
            Self::CapReached { count, max } => {
                format!("已达续跑上限 {count}/{max}——再输入任意消息即可重置预算")
            }
            Self::Idle { streak, limit } => {
                format!("连续 {streak} 次续跑都没有产生工具调用（空转阈值 {limit}），已判定为空转")
            }
            Self::InjectFailed { detail } => format!("注入续跑消息失败：{detail}"),
            Self::NoProgress { streak, limit } => format!(
                "连续 {streak} 次续跑都没有新内容（回复与上一次重复或为空，复读阈值 {limit}），已判定为复读"
            ),
            Self::FatalError { message } => {
                format!("遇到不可重试的错误，已停止续跑：{}", summarize(message))
            }
            Self::RetryExhausted { count, max } => {
                format!("本次 turn 已达重试上限 {count}/{max}——再输入任意消息即可重置预算")
            }
            Self::RetryFailed { detail } => format!("重试投递失败：{detail}"),
        }
    }
}

/// 一次停下的处置结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 注入 `text` 并再跑一个 agent step。
    Continue { text: String },
    /// 不再续跑，并记下原因。
    Stop(StopReason),
}

/// 该不该继续。
///
/// `progress` 必须是**已结算本步活动**的快照（见 [`crate::state::SharedState::settle_step`]），
/// 也就是说 `idle_streak` 已经把「本步有没有产生工具调用」算了进去。
///
/// 判定顺序是复读 → 空转 → 到顶：越具体的原因越值得人看。复读意味着模型在输出里打转，
/// 空转意味着它连话都不说了，到顶只是预算走完——三者同时命中时报告最靠前的那个。
pub fn decide(config: &Config, progress: &Progress) -> Decision {
    if config.no_progress_stop > 0 && progress.no_progress_streak >= config.no_progress_stop {
        return Decision::Stop(StopReason::NoProgress {
            streak: progress.no_progress_streak,
            limit: config.no_progress_stop,
        });
    }
    if config.idle_stop > 0 && progress.idle_streak >= config.idle_stop {
        return Decision::Stop(StopReason::Idle {
            streak: progress.idle_streak,
            limit: config.idle_stop,
        });
    }
    if progress.continuations >= config.max {
        return Decision::Stop(StopReason::CapReached {
            count: progress.continuations,
            max: config.max,
        });
    }
    Decision::Continue {
        text: continue_text(config, progress),
    }
}

/// 续跑时喂给模型的那句话。
///
/// 上一步没干活（没有工具调用，或者回复与上一次重复/为空）时换用纠正提示而不是裸的
/// 「继续」：对复读的模型再说一次「继续」等于给它同一张牌，只有明说「上一步没进展」才
/// 有可能把它从填充式推理里拽出来。
fn continue_text(config: &Config, progress: &Progress) -> String {
    if progress.no_progress_streak > 0 || progress.idle_streak > 0 {
        config.nudge_text.clone()
    } else {
        config.continue_text.clone()
    }
}

/// 失败的可重试性分类。
///
/// 宿主的 `ErrorOccurred.recoverable` 实测恒为 `false`，状态码也没有结构化字段，所以只能读
/// `message` 文本。分类只区分「明确不该重试」和「其余」——无人值守下再试一次的成本远低于
/// 卡死的成本，因此**未命中任何特征一律按可重试处理**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// 值得再试一次：传输中断、限流、上游 5xx。
    Transient,
    /// 再试也没用，得人来处理：模型不存在、鉴权失败、请求非法。
    Fatal,
}

/// 出现即判 `Fatal` 的文本特征（按小写匹配）。
const FATAL_MARKERS: &[&str] = &[
    "model not found",
    "no such model",
    "unsupported model",
    "invalid api key",
    "invalid_api_key",
    "unauthorized",
    "authentication",
    "permission denied",
    "insufficient quota",
    "insufficient_quota",
    "exceeded your current quota",
    "billing",
    "invalid request",
    "invalid_request_error",
    "bad request",
    "content filter",
    "content_filter",
    "content policy",
];

/// 出现即判 `Fatal` 的 HTTP 状态码。
///
/// 必须按**词边界**匹配：错误文本里混着 `bytes-read=1329670` 这类字节数，直接 `contains`
/// 就会把数字串里的片段当成状态码，误判成致命错误、白白放弃重试。
const FATAL_STATUS_CODES: &[&str] = &["400", "401", "403", "404", "413", "422"];

/// 把失败文本分类。
pub fn classify_failure(message: &str) -> FailureClass {
    let lower = message.to_ascii_lowercase();
    if FATAL_MARKERS.iter().any(|marker| lower.contains(marker))
        || FATAL_STATUS_CODES
            .iter()
            .any(|code| has_status_code(&lower, code))
    {
        FailureClass::Fatal
    } else {
        FailureClass::Transient
    }
}

/// `text` 里是否出现独立的 `code`（前后都不是数字）。
fn has_status_code(text: &str, code: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(index) = text[from..].find(code) {
        let start = from + index;
        let end = start + code.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_digit();
        let after_ok = end >= bytes.len() || !bytes[end].is_ascii_digit();
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// 重试与否的处置结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// 等 `delay_ms` 毫秒后再起一个 turn。
    Retry { delay_ms: u64 },
    /// 不再重试，并记下原因。
    Stop(StopReason),
}

/// 这次失败要不要重试。
///
/// `progress.retries` 是**本 turn 已成功投递的重试次数**，退避按 `base * 2^(attempt-1)`
/// 截到 `max_delay`：失败往往是同一侧在限流或抖动，紧贴着连续重试只会把对方压得更死。
pub fn decide_retry(
    config: &Config,
    progress: &Progress,
    class: FailureClass,
    message: &str,
) -> RetryDecision {
    if class == FailureClass::Fatal {
        return RetryDecision::Stop(StopReason::FatalError {
            message: message.to_owned(),
        });
    }
    if progress.retries >= config.retry_max {
        return RetryDecision::Stop(StopReason::RetryExhausted {
            count: progress.retries,
            max: config.retry_max,
        });
    }
    RetryDecision::Retry {
        delay_ms: retry_delay_ms(config, progress.retries.saturating_add(1)),
    }
}

/// 第 `attempt` 次重试前的等待时长（毫秒，`attempt` 从 1 起算）。
pub fn retry_delay_ms(config: &Config, attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(32);
    u64::from(config.retry_base_delay_ms)
        .saturating_mul(1u64 << shift)
        .min(u64::from(config.retry_max_delay_ms))
}

/// 本步是否「没有新内容」。
///
/// 判据是**没有工具调用**，且回复去空白后为空、或与上一步规范化后完全相同。只看这两条是
/// 刻意的：模型在填充式推理里复读时句子往往一模一样（"Let me output." / "OK." 交替），
/// 任何更松的相似度判定都会把正常的长回复误伤成复读。
pub fn is_no_progress(previous: Option<u64>, current: &str, tool_calls: u32) -> bool {
    if tool_calls > 0 {
        return false;
    }
    let normalized = normalize(current);
    if normalized.is_empty() {
        return true;
    }
    previous == Some(text_fingerprint(&normalized))
}

/// 回复内容的指纹，用于跨步比较。
///
/// 存指纹而不是原文：256 个会话各留一份完整回复太占内存，而比较只需要相等性。
/// `DefaultHasher` 在同一次运行内稳定，够用。
pub fn text_fingerprint(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    normalize(text).hash(&mut hasher);
    hasher.finish()
}

/// 折叠空白，让「只差缩进或换行」的两步算作同一段内容。
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 把可能很长的错误文本压成一行，供 `/sleep status` 显示。
pub fn summarize(message: &str) -> String {
    /// 状态行里最多留这么多字符。
    const LIMIT: usize = 160;
    let collapsed = normalize(message);
    if collapsed.chars().count() <= LIMIT {
        return collapsed;
    }
    let head: String = collapsed.chars().take(LIMIT).collect();
    format!("{head}…")
}

/// 该工具名是否属于「自动应答」名单。精确相等，不做前缀匹配。
pub fn is_answer_tool(tool_name: &str, tools: &[String]) -> bool {
    tools.iter().any(|name| name == tool_name)
}

/// 一道题被自动作答的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendedChoice {
    pub question: String,
    pub answer: String,
}

/// 从提问工具的入参里提取每题的自动答案。
///
/// 取值优先级：**标了 `recommended: true` 的选项** → 第一个选项。宿主内建的 `askUser`
/// 会显式标记推荐项（用户超时未响应时宿主也选它），所以标记优先比上游 pi 插件「一律取
/// 第一个」更准；但**至少给出一个答案**——上游的取舍是宁可答错也不卡住，这里保持一致。
///
/// 入参不合预期（不是对象、没有 `questions`、题型残缺）时返回 `None`，调用方据此退回
/// 「不改写、只让模型自己继续」。
pub fn recommended_choices(input: &Value) -> Option<Vec<RecommendedChoice>> {
    let questions = input.get("questions")?.as_array()?;
    if questions.is_empty() {
        return None;
    }

    let mut choices = Vec::with_capacity(questions.len());
    for question in questions {
        let text =
            string_field(question, "question").or_else(|| string_field(question, "prompt"))?;
        let options = question.get("options")?.as_array()?;
        if options.is_empty() {
            return None;
        }

        let recommended: Vec<&str> = options
            .iter()
            .filter(|option| option.get("recommended").and_then(Value::as_bool) == Some(true))
            .filter_map(|option| option_label(option))
            .collect();
        // 多选题的语义是「可以同时选几项」，那就把所有推荐项一起勾上；单选题只取第一项。
        let multi_select = question
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let answer = if multi_select && !recommended.is_empty() {
            recommended.join(", ")
        } else {
            recommended
                .first()
                .map(|label| (*label).to_owned())
                .or_else(|| options.first().and_then(option_label).map(str::to_owned))?
        };

        choices.push(RecommendedChoice {
            question: text.to_owned(),
            answer,
        });
    }
    Some(choices)
}

/// 选项标签：`label` 优先，回落到 `value`。
fn option_label(option: &Value) -> Option<&str> {
    string_field(option, "label").or_else(|| string_field(option, "value"))
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|text| !text.is_empty())
}

/// 渲染自动作答摘要，附在拦截原因里给模型看。
pub fn render_choices(choices: &[RecommendedChoice]) -> String {
    choices
        .iter()
        .map(|choice| format!("-「{}」→ 已选「{}」", choice.question, choice.answer))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 生成拦截提问工具的原因。**这段文本是模型可见的**。
///
/// 只说「不允许」的话，模型往往原样重试；给出「已经替你选了什么」和「真需要人怎么办」，
/// 才能把「提问 → 被拒 → 再提问」的死循环压成一次。
pub fn block_reason(tool_name: &str, choices: &[RecommendedChoice]) -> String {
    let summary = if choices.is_empty() {
        "未能解析出选项，请按任务最合理的默认方案继续。".to_owned()
    } else {
        format!(
            "自动选择如下（标了 recommended 的优先，没有标记时取第一个）：\n{}",
            render_choices(choices)
        )
    };
    format!(
        "【无人值守模式】已拦截对 `{tool_name}` 的调用，并按推荐项自动作答，无需等待用户点选。\n\
         {summary}\n\
         请按上述选择继续执行任务，不要再次调用提问工具；如确需人类决策，\
         请在回复末尾注明「需要人工确认：…」然后停下，不要死循环追问。"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn config() -> Config {
        Config {
            enabled: true,
            ..Config::default()
        }
    }

    fn progress(continuations: u32, tool_calls: u32, idle_streak: u32) -> Progress {
        Progress {
            continuations,
            tool_calls,
            idle_streak,
            ..Progress::default()
        }
    }

    /// 同上，外加复读链与重试次数。
    fn progress_with(no_progress_streak: u32, retries: u32) -> Progress {
        Progress {
            no_progress_streak,
            retries,
            ..Progress::default()
        }
    }

    #[test]
    fn a_working_step_continues_with_the_configured_text() {
        let decision = decide(&config(), &progress(3, 5, 0));
        assert_eq!(
            decision,
            Decision::Continue {
                text: "继续".to_owned()
            }
        );
    }

    #[test]
    fn a_configured_continue_text_is_used_verbatim() {
        let config = Config {
            continue_text: "继续按计划推进".to_owned(),
            ..config()
        };
        assert_eq!(
            decide(&config, &progress(0, 1, 0)),
            Decision::Continue {
                text: "继续按计划推进".to_owned()
            }
        );
    }

    #[test]
    fn the_cap_stops_continuation() {
        let config = Config { max: 4, ..config() };
        assert_eq!(
            decide(&config, &progress(4, 1, 0)),
            Decision::Stop(StopReason::CapReached { count: 4, max: 4 })
        );
    }

    /// 上限是「单次人工 turn 的预算」，因此刚好差一次时仍然续跑。
    #[test]
    fn one_below_the_cap_still_continues() {
        let config = Config { max: 4, ..config() };
        assert!(matches!(
            decide(&config, &progress(3, 1, 0)),
            Decision::Continue { .. }
        ));
    }

    #[test]
    fn the_idle_limit_stops_continuation() {
        let config = Config {
            idle_stop: 3,
            ..config()
        };
        assert_eq!(
            decide(&config, &progress(1, 0, 3)),
            Decision::Stop(StopReason::Idle {
                streak: 3,
                limit: 3
            })
        );
    }

    #[test]
    fn a_zero_idle_limit_disables_the_breaker() {
        let config = Config {
            idle_stop: 0,
            ..config()
        };
        assert!(matches!(
            decide(&config, &progress(1, 0, 99)),
            Decision::Continue { .. }
        ));
    }

    /// 两者同时命中时空转优先：它是更值得人看一眼的原因。
    #[test]
    fn the_idle_breaker_outranks_the_cap() {
        let config = Config {
            max: 4,
            idle_stop: 2,
            ..config()
        };
        assert_eq!(
            decide(&config, &progress(9, 0, 2)),
            Decision::Stop(StopReason::Idle {
                streak: 2,
                limit: 2
            })
        );
    }

    #[test]
    fn every_stop_reason_has_a_non_empty_description() {
        for reason in [
            StopReason::CapReached { count: 1, max: 2 },
            StopReason::Idle {
                streak: 3,
                limit: 3,
            },
            StopReason::InjectFailed {
                detail: "no active turn".to_owned(),
            },
        ] {
            assert!(!reason.text().is_empty());
        }
    }

    #[test]
    fn answer_tool_matching_is_exact() {
        let tools = vec!["askUser".to_owned()];
        assert!(is_answer_tool("askUser", &tools));
        assert!(!is_answer_tool("ask_user", &tools));
        assert!(!is_answer_tool("askUserQuestion", &tools));
        assert!(!is_answer_tool("askUser", &[]));
    }

    #[test]
    fn the_recommended_option_wins_over_the_first() {
        let choices = recommended_choices(&json!({
            "questions": [{
                "question": "用哪个方案？",
                "header": "方案",
                "options": [
                    { "label": "方案 A", "description": "先做 A" },
                    { "label": "方案 B", "description": "先做 B", "recommended": true }
                ]
            }]
        }))
        .expect("应能解析");
        assert_eq!(
            choices,
            vec![RecommendedChoice {
                question: "用哪个方案？".to_owned(),
                answer: "方案 B".to_owned(),
            }]
        );
    }

    #[test]
    fn the_first_option_is_the_fallback() {
        let choices = recommended_choices(&json!({
            "questions": [{
                "question": "继续吗？",
                "options": [
                    { "label": "继续", "description": "接着做" },
                    { "label": "停止", "description": "收工" }
                ]
            }]
        }))
        .expect("应能解析");
        assert_eq!(choices[0].answer, "继续");
    }

    /// 多选题的语义是「可以同时选几项」，推荐项就一起勾上。
    #[test]
    fn a_multi_select_question_takes_every_recommended_option() {
        let choices = recommended_choices(&json!({
            "questions": [{
                "question": "要改哪些文件？",
                "multiSelect": true,
                "options": [
                    { "label": "a.rs", "description": "", "recommended": true },
                    { "label": "b.rs", "description": "", "recommended": true },
                    { "label": "c.rs", "description": "" }
                ]
            }]
        }))
        .expect("应能解析");
        assert_eq!(choices[0].answer, "a.rs, b.rs");
    }

    #[test]
    fn a_value_field_is_accepted_as_a_label() {
        let choices = recommended_choices(&json!({
            "questions": [{
                "prompt": "选一个",
                "options": [{ "value": "甲" }]
            }]
        }))
        .expect("应能解析");
        assert_eq!(choices[0].question, "选一个");
        assert_eq!(choices[0].answer, "甲");
    }

    #[test]
    fn several_questions_are_all_answered() {
        let choices = recommended_choices(&json!({
            "questions": [
                { "question": "一？", "options": [{ "label": "a" }] },
                { "question": "二？", "options": [{ "label": "b" }] }
            ]
        }))
        .expect("应能解析");
        assert_eq!(choices.len(), 2);
    }

    /// 入参不合预期时返回 `None`，调用方据此退回「不改写」而不是编一个答案出来。
    #[test]
    fn malformed_inputs_yield_nothing() {
        for input in [
            json!(null),
            json!("ask"),
            json!({}),
            json!({ "questions": [] }),
            json!({ "questions": [{}] }),
            json!({ "questions": [{ "question": "问？", "options": [] }] }),
            json!({ "questions": [{ "question": "", "options": [{ "label": "a" }] }] }),
            json!({ "questions": [{ "options": [{ "label": "a" }] }] }),
        ] {
            assert!(
                recommended_choices(&input).is_none(),
                "不该从 {input} 里解析出答案"
            );
        }
    }

    #[test]
    fn the_reason_lists_every_choice() {
        let reason = block_reason(
            "askUser",
            &[
                RecommendedChoice {
                    question: "一？".to_owned(),
                    answer: "a".to_owned(),
                },
                RecommendedChoice {
                    question: "二？".to_owned(),
                    answer: "b".to_owned(),
                },
            ],
        );
        assert!(reason.contains("`askUser`"), "{reason}");
        assert!(reason.contains("-「一？」→ 已选「a」"), "{reason}");
        assert!(reason.contains("-「二？」→ 已选「b」"), "{reason}");
    }

    /// 模型必须知道「真需要人」时怎么退出，否则被拦住提问就只能干等。
    #[test]
    fn the_reason_gives_the_model_an_escape_hatch() {
        let reason = block_reason("askUser", &[]);
        assert!(reason.contains("需要人工确认"), "{reason}");
        assert!(reason.contains("不要死循环追问"), "{reason}");
    }
    // ─── 失败分类与重试 ───────────────────────────────────────────────────

    /// 四条真实错误文本的分类。前三条是传输层/上游抖动，重试有意义；第四条是模型名不对，
    /// 重试一万次也一样。
    #[test]
    fn the_real_failures_are_classified() {
        let transient = [
            "transport error: read streaming response body failed for \
             https://api.r4.codes/v1/chat/completions: status=200, content-type=text/event-stream, \
             content-encoding=<missing>, bytes-read=1329670: error decoding response body; \
             Connection timed out (os error 110)",
            "transport error: read streaming response body failed for \
             https://api.r4.codes/v1/chat/completions: status=200, bytes-read=267384: \
             error decoding response body; Connection reset by peer (os error 104)",
            "The stream was terminated by the server. Please retry.",
        ];
        for message in transient {
            assert_eq!(
                classify_failure(message),
                FailureClass::Transient,
                "{message}"
            );
        }

        assert_eq!(
            classify_failure("model not found (404): 404 page not found"),
            FailureClass::Fatal
        );
    }

    /// 字节数不是状态码：`1329670` 里藏着 `296`、`967` 这些三位数字，按词边界匹配才不会被
    /// 当成 4xx/5xx 误判。
    #[test]
    fn byte_counts_are_not_mistaken_for_status_codes() {
        for message in [
            "bytes-read=1329670",
            "bytes-read=4000000",
            "status=200 bytes-read=1404222",
            "read 4012 bytes",
        ] {
            assert_eq!(
                classify_failure(message),
                FailureClass::Transient,
                "{message}"
            );
        }

        // 同一个数字，出现在该出现的位置就认。
        assert_eq!(classify_failure("http 400"), FailureClass::Fatal);
        assert_eq!(
            classify_failure("model not found (404)"),
            FailureClass::Fatal
        );
    }

    /// 认不出来的错误按可重试处理：无人值守下再试一次的成本远低于卡死。
    #[test]
    fn unknown_failures_are_retried() {
        assert_eq!(
            classify_failure("something nobody has seen before"),
            FailureClass::Transient
        );
    }

    #[test]
    fn the_backoff_doubles_and_is_capped() {
        let config = Config {
            retry_base_delay_ms: 1_000,
            retry_max_delay_ms: 5_000,
            ..config()
        };
        assert_eq!(retry_delay_ms(&config, 1), 1_000);
        assert_eq!(retry_delay_ms(&config, 2), 2_000);
        assert_eq!(retry_delay_ms(&config, 3), 4_000);
        // 到顶之后不再增长。
        assert_eq!(retry_delay_ms(&config, 4), 5_000);
        assert_eq!(retry_delay_ms(&config, 40), 5_000);
    }

    #[test]
    fn a_fatal_failure_is_never_retried() {
        let config = config();
        let decision = decide_retry(
            &config,
            &progress_with(0, 0),
            FailureClass::Fatal,
            "model not found",
        );
        assert!(
            matches!(decision, RetryDecision::Stop(StopReason::FatalError { .. })),
            "{decision:?}"
        );
    }

    #[test]
    fn the_retry_budget_bounds_attempts() {
        let config = Config {
            retry_max: 2,
            ..config()
        };
        assert!(matches!(
            decide_retry(&config, &progress_with(0, 0), FailureClass::Transient, "x"),
            RetryDecision::Retry { delay_ms: 1_000 }
        ));
        assert!(matches!(
            decide_retry(&config, &progress_with(0, 1), FailureClass::Transient, "x"),
            RetryDecision::Retry { delay_ms: 2_000 }
        ));
        assert_eq!(
            decide_retry(&config, &progress_with(0, 2), FailureClass::Transient, "x"),
            RetryDecision::Stop(StopReason::RetryExhausted { count: 2, max: 2 })
        );
    }

    /// `retryMax = 0` 是「关掉失败重试」：即使真被调到这里也不该投递。
    #[test]
    fn a_zero_retry_budget_never_retries() {
        let config = Config {
            retry_max: 0,
            ..config()
        };
        assert_eq!(
            decide_retry(&config, &progress_with(0, 0), FailureClass::Transient, "x"),
            RetryDecision::Stop(StopReason::RetryExhausted { count: 0, max: 0 })
        );
    }

    // ─── 复读判定 ─────────────────────────────────────────────────────────

    #[test]
    fn repeated_and_empty_replies_are_no_progress() {
        // 空回复：什么都没说。
        assert!(is_no_progress(None, "   \n ", 0));
        // 与上一步完全相同。
        let previous = text_fingerprint("Let me output.");
        assert!(is_no_progress(Some(previous), "Let me output.", 0));
        // 只差空白与换行也算相同。
        assert!(is_no_progress(Some(previous), "  Let me\noutput.  ", 0));
    }

    #[test]
    fn new_content_or_tool_activity_is_progress() {
        let previous = text_fingerprint("Let me output.");
        // 说了新东西。
        assert!(!is_no_progress(Some(previous), "我改完了 a.rs。", 0));
        // 第一次停下时没有「上一步」可比，只按「空不空」判。
        assert!(!is_no_progress(None, "我改完了 a.rs。", 0));
        // 有工具调用就不算复读，哪怕回复一模一样。
        assert!(!is_no_progress(Some(previous), "Let me output.", 1));
    }

    #[test]
    fn the_no_progress_limit_stops_continuation() {
        let config = Config {
            no_progress_stop: 2,
            ..config()
        };
        assert!(matches!(
            decide(&config, &progress_with(1, 0)),
            Decision::Continue { .. }
        ));
        assert_eq!(
            decide(&config, &progress_with(2, 0)),
            Decision::Stop(StopReason::NoProgress {
                streak: 2,
                limit: 2
            })
        );
    }

    /// 复读比空转与到顶更值得报告：模型在打转是唯一需要人看一眼的那种。
    #[test]
    fn the_most_specific_reason_wins() {
        let config = Config {
            max: 0,
            idle_stop: 1,
            no_progress_stop: 1,
            ..config()
        };
        assert_eq!(
            decide(&config, &progress_with(1, 0)),
            Decision::Stop(StopReason::NoProgress {
                streak: 1,
                limit: 1
            })
        );
    }

    /// 上一步没干活时改喂纠正提示，而不是裸的「继续」。
    #[test]
    fn a_step_without_activity_gets_the_nudge_text() {
        let defaults = config();
        let Decision::Continue { text } = decide(&defaults, &progress(0, 0, 1)) else {
            panic!("空转链 1 不该触发熔断");
        };
        assert_eq!(text, defaults.nudge_text);

        // 阈值调大时，没到熔断线的那一步同样改喂纠正提示。
        let patient = Config {
            no_progress_stop: 2,
            ..config()
        };
        let Decision::Continue { text } = decide(&patient, &progress_with(1, 0)) else {
            panic!("复读链 1 低于阈值 2，应当继续");
        };
        assert_eq!(text, patient.nudge_text);
    }
}
