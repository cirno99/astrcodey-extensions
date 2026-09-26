//! 纯判定逻辑：给定配置与运行期状态，决定续跑还是停下、生成给模型看的拦截原因。
//!
//! 本模块**不碰宿主**，因此可以逐条单元测试。宿主交互全部在 [`crate::hook`]。
//!
//! 判定拆成两半是刻意的：`plan` 回答「该不该继续」，`hook` 回答「怎么把它做出来」。
//! 混在一起的话，最需要证据的那些边界（到顶、空转、注入失败回落）就得靠集成测试才能覆盖。

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
}

impl StopReason {
    /// 中文描述，用于 `/sleep status`。
    pub fn text(&self) -> String {
        match self {
            Self::CapReached { count, max } => format!(
                "已达续跑上限 {count}/{max}——再输入任意消息即可重置预算"
            ),
            Self::Idle { streak, limit } => format!(
                "连续 {streak} 次续跑都没有产生工具调用（空转阈值 {limit}），已判定为空转"
            ),
            Self::InjectFailed { detail } => format!("注入续跑消息失败：{detail}"),
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
/// 判定顺序是先空转后到顶：两者同时命中时，空转是更值得报告的原因——到顶只是预算走完，
/// 空转意味着模型不干活了，是真正需要人看一眼的那种。
pub fn decide(config: &Config, progress: &Progress) -> Decision {
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
        text: config.continue_text.clone(),
    }
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
        let text = string_field(question, "question").or_else(|| string_field(question, "prompt"))?;
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
            stop_reason: None,
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
}
