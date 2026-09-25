//! 六态状态机核心：转移校验、流转方向分类、三级轮次警告。
//!
//! 逐条对照上游 `src/state-machine.ts` 的纯逻辑部分；持久化与内存注册表在
//! [`crate::state`]。上游的 `getAllowedTargets(from: ThinkingState | null, ..)` 把
//! `null` 与 `"START"` 视作同一分支，移植版在载入时就把未初始化归一成
//! [`ThinkingState::START`]，因此这里的 `from` 不再可空。

use crate::types::{
    Difficulty, SessionState, TOOL_TASK_INFO, ThinkingState, get_max_state_turns, state_label,
};

/// 状态在自然流程中的顺序索引，用于判断流转方向。
pub fn state_order(state: ThinkingState) -> u8 {
    match state {
        ThinkingState::START => 0,
        ThinkingState::DEEP_UNDERSTAND => 1,
        ThinkingState::DESIGN => 2,
        ThinkingState::EXECUTE => 3,
        ThinkingState::VERIFY => 4,
        ThinkingState::END => 5,
    }
}

/// 根据当前状态 + 难度，返回允许转移的目标状态集合。
///
/// START 时按难度开放不同路径：TRIVIAL 可直达 EXECUTE，SIMPLE 可到 DESIGN/EXECUTE，
/// 其余必须先 DEEP_UNDERSTAND。END 无合法出口（只能靠 `TurnStart` 复位回 START）。
pub fn get_allowed_targets(from: ThinkingState, difficulty: Option<Difficulty>) -> Vec<ThinkingState> {
    use ThinkingState::*;
    match from {
        START => match difficulty {
            Some(Difficulty::TRIVIAL) => vec![DEEP_UNDERSTAND, EXECUTE],
            Some(Difficulty::SIMPLE) => vec![DEEP_UNDERSTAND, DESIGN, EXECUTE],
            _ => vec![DEEP_UNDERSTAND],
        },
        DEEP_UNDERSTAND => vec![DESIGN, EXECUTE, VERIFY],
        DESIGN => vec![EXECUTE, DEEP_UNDERSTAND, VERIFY],
        EXECUTE => vec![VERIFY, DESIGN, DEEP_UNDERSTAND],
        VERIFY => vec![END, EXECUTE, DEEP_UNDERSTAND, DESIGN],
        END => Vec::new(),
    }
}

/// 允许的目标按自然流程拆成「向前」与「回退」两组。
///
/// 向前：目标顺序大于当前状态（含完成任务进入 END）；回退：目标顺序小于当前状态。
pub fn classify_targets(
    from: ThinkingState,
    difficulty: Option<Difficulty>,
) -> (Vec<ThinkingState>, Vec<ThinkingState>) {
    let current_order = state_order(from);
    let mut forward = Vec::new();
    let mut backward = Vec::new();

    for target in get_allowed_targets(from, difficulty) {
        // 上游把 END 单独列一支（`target === "END" && from !== "END"`）。END 的顺序索引
        // 本就是最大的 5，只有从 END 出发时才不满足「顺序更大」——而 END 没有任何出口，
        // 因此那一支永远走不到。合并成一条判定，行为逐位等价。
        let is_forward = target == ThinkingState::END && from != ThinkingState::END
            || state_order(target) > current_order;
        if is_forward {
            forward.push(target);
        } else {
            backward.push(target);
        }
    }

    (forward, backward)
}

/// 格式化状态流转提示文本。
///
/// 格式：`本阶段完成可前移至[xxx(XXX)、yyy(YYY)]状态；本阶段不足可回退至[zzz(ZZZ)]状态。`
pub fn format_next_state_hint(from: ThinkingState, difficulty: Option<Difficulty>) -> String {
    let (forward, backward) = classify_targets(from, difficulty);

    let fmt = |states: &[ThinkingState]| {
        states
            .iter()
            .map(|state| format!("{}({})", state_label(*state), state))
            .collect::<Vec<_>>()
            .join("、")
    };

    let mut parts = Vec::new();
    if !forward.is_empty() {
        parts.push(format!("本阶段完成可前移至[{}]状态；", fmt(&forward)));
    }
    if !backward.is_empty() {
        parts.push(format!("本阶段不足可回退至[{}]状态。", fmt(&backward)));
    }
    parts.join("")
}

/// `set-task-info` 被拒绝的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTaskInfoError;

impl SetTaskInfoError {
    /// 上游的拒绝文案。
    pub fn reason(&self) -> &'static str {
        "仅可在 START 阶段或状态超限时设定任务信息"
    }
}

/// `transition` 被拒绝的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// 目标就是当前状态。
    SameState(ThinkingState),
    /// START 出发但任务画像未设定。
    ProfileMissing,
    /// 目标不在当前状态的可用集合里。
    NotAllowed {
        from: ThinkingState,
        to: ThinkingState,
        allowed: Vec<ThinkingState>,
    },
}

impl TransitionError {
    /// 上游的拒绝文案（`NotAllowed` 在调用方另附 `formatNextStateHint`）。
    pub fn reason(&self) -> String {
        match self {
            Self::SameState(state) => format!("不能转移到自身（{state}→{state}）"),
            Self::ProfileMissing => format!(
                "START 流转前必须调用 {TOOL_TASK_INFO} 工具设定任务画像（难度/大类型/小类型），当前画像未设定"
            ),
            Self::NotAllowed { from, to, allowed } => {
                let allowed = if allowed.is_empty() {
                    "无（END 需用户发消息自动转换）".to_owned()
                } else {
                    allowed
                        .iter()
                        .map(|state| state.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!("{from} 状态下不能转移到 {to}。可转移: {allowed}")
            },
        }
    }
}

/// 当前状态是否已经超过轮次上限（`set-task-info` 的重估前置条件）。
///
/// 上游：`current && state !== null && state !== "END" ? stateTurnCount > maxTurns : false`。
pub fn is_over_limit(current: &SessionState) -> bool {
    if current.state == ThinkingState::END {
        return false;
    }
    current.state_turn_count > get_max_state_turns(current.state, current.difficulty)
}

/// 设定任务画像：难度 + 大类型 + 小类型。
///
/// 允许条件：START 状态（正常设定）或当前状态超限（防御难度虚高，允许重估）。
/// 设定后 `state_turn_count` 归零（新难度新起点），`task_turn_count` 保持不变。
pub fn set_task_info(
    current: &mut SessionState,
    difficulty: Difficulty,
    master: crate::types::MasterTaskType,
    sub: crate::types::SubTaskType,
) -> Result<(), SetTaskInfoError> {
    if current.state != ThinkingState::START && !is_over_limit(current) {
        return Err(SetTaskInfoError);
    }

    current.difficulty = Some(difficulty);
    current.master_task_type = Some(master);
    current.sub_task_type = Some(sub);
    current.state_turn_count = 0;
    current.last_transition_time = now_millis();
    Ok(())
}

/// 状态流转。
///
/// - 校验转移合法性（[`get_allowed_targets`]）
/// - END 状态拒绝任何手动流转（只能由 `TurnStart` 复位）
/// - 转入 END 时清空任务画像与 `visited`
/// - 从 START 出发时 `task_turn_count` +1
/// - `state_turn_count` 每次转移归零
/// - `visited` 记录每次流转的目标状态（全链路路径感知）
pub fn transition(current: &mut SessionState, to: ThinkingState) -> Result<(), TransitionError> {
    let from = current.state;
    if from == to {
        return Err(TransitionError::SameState(from));
    }

    if from == ThinkingState::START && !current.has_profile() {
        return Err(TransitionError::ProfileMissing);
    }

    let allowed = get_allowed_targets(from, current.difficulty);
    if !allowed.contains(&to) {
        return Err(TransitionError::NotAllowed { from, to, allowed });
    }

    let ending = to == ThinkingState::END;
    if ending {
        current.difficulty = None;
        current.master_task_type = None;
        current.sub_task_type = None;
        current.visited.clear();
    } else {
        current.visited.push(to);
    }
    if from == ThinkingState::START {
        current.task_turn_count += 1;
    }
    current.state = to;
    current.state_turn_count = 0;
    current.last_transition_time = now_millis();
    Ok(())
}

/// 当前毫秒时间戳。上游用 `Date.now()`。
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 难度档位（文案表按它分档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarnTier {
    /// TRIVIAL / SIMPLE：温和，引导重估难度。
    Simple,
    /// MODERATE：标准。
    Standard,
    /// COMPLEX 及以上：严厉，强调流程纪律。
    Hard,
}

/// 难度 → 档位。难度未设定时按标准档。
pub fn warn_tier(difficulty: Option<Difficulty>) -> WarnTier {
    match difficulty {
        Some(Difficulty::TRIVIAL | Difficulty::SIMPLE) => WarnTier::Simple,
        Some(Difficulty::COMPLEX | Difficulty::HARD | Difficulty::EXTREME) => WarnTier::Hard,
        _ => WarnTier::Standard,
    }
}

/// 警告级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarnLevel {
    /// 接近上限。
    Soft,
    /// 已超过上限。
    Over,
    /// 严重超时，强制停止。
    HardStop,
}

impl WarnLevel {
    /// 文案表里该级别的三档措辞。
    fn detail(self, tier: WarnTier) -> &'static str {
        match (self, tier) {
            (Self::Soft, WarnTier::Simple) => {
                "若任务比预期简单，可调用 asymptotic-think_set-task-info 工具 重估难度；否则请尽快完成并流转状态。"
            },
            (Self::Soft, WarnTier::Standard) => {
                "可调用 asymptotic-think_set-task-info 工具 重新评估难度，或尽快完成当前阶段并流转状态，以顺利推进任务。"
            },
            (Self::Soft, WarnTier::Hard) => {
                "请遵循流程纪律，专注推进，完成后立即流转状态；若难度评估有误，可调用 asymptotic-think_set-task-info 工具 重新评估难度。"
            },
            (Self::Over, WarnTier::Simple) => {
                "任务可能被误评高难度——调用 asymptotic-think_set-task-info 工具重估为更低难度，或立即调用 asymptotic-think_transition 工具流转状态。"
            },
            (Self::Over, WarnTier::Standard) => {
                "请调用 asymptotic-think_transition 工具流转状态，或调用 asymptotic-think_set-task-info 工具 重新评估难度，及时推进任务。"
            },
            (Self::Over, WarnTier::Hard) => {
                "请遵循流程纪律，立即调用 asymptotic-think_transition 工具流转状态；若难度评估有误，可调用 asymptotic-think_set-task-info 工具 重新评估难度。"
            },
            (Self::HardStop, WarnTier::Simple) => {
                "请立即调用 asymptotic-think_transition 工具流转状态；若任务实际简单，调用 asymptotic-think_set-task-info 工具 重估难度，以高效完成。"
            },
            (Self::HardStop, WarnTier::Standard) => {
                "请停止当前操作，立即调用 asymptotic-think_transition 工具流转状态，或调用 asymptotic-think_set-task-info 工具 重新评估难度，或等待用户新指令。"
            },
            (Self::HardStop, WarnTier::Hard) => {
                "请遵循流程纪律，停止当前操作并立即调用 asymptotic-think_transition 工具流转状态；若难度评估有误，可调用 asymptotic-think_set-task-info 工具 重新评估难度。"
            },
        }
    }
}

/// 判定警告级别；未触发任何阈值时返回 `None`。
///
/// 三级阈值：软提醒（`nextTurn >= maxTurns - 1` 且 `maxTurns > 2`）/
/// 超限警告（`nextTurn > maxTurns`）/ hardStop 强制停止（`nextTurn > maxTurns + max(1, ceil(maxTurns/3))`）。
pub fn warn_level(next_turn: u32, max_turns: u32) -> Option<WarnLevel> {
    let excess_threshold = max_turns + (max_turns / 3).max(1);
    if next_turn > excess_threshold {
        return Some(WarnLevel::HardStop);
    }
    if next_turn > max_turns {
        return Some(WarnLevel::Over);
    }
    // 上游写的是 `maxTurns - 1`；`maxTurns` 可能是 0（难度未设定且状态非 START），
    // 因此先判 `maxTurns > 2` 再算下界，避免无符号下溢。
    if max_turns > 2 && next_turn >= max_turns - 1 {
        return Some(WarnLevel::Soft);
    }
    None
}

/// 渲染轮次警告文本（含上游的 `<turnWarning>` / `<hardStop>` 标签与行首换行）。
pub fn render_warning(
    level: WarnLevel,
    state: ThinkingState,
    turn: u32,
    max_turns: u32,
    difficulty: Option<Difficulty>,
) -> String {
    let label = state_label(state);
    let detail = level.detail(warn_tier(difficulty));
    match level {
        WarnLevel::Soft => format!(
            "\n<turnWarning>你已处于【{label}】状态 {turn} 轮（上限 {max_turns}），接近上限。{detail}</turnWarning>"
        ),
        WarnLevel::Over => format!(
            "\n<turnWarning>你已处于【{label}】状态 {turn} 轮（上限 {max_turns}），已超过上限。{detail}</turnWarning>"
        ),
        WarnLevel::HardStop => format!(
            "\n<hardStop>⛔ 强制停止：你已在【{label}】状态停留 {turn} 轮（上限 {max_turns}），严重超时。{detail}</hardStop>"
        ),
    }
}

/// `turn_end` 时调用：`state_turn_count` +1。END 不计数。
///
/// 上游的 `bumpAndWarn` 在这里同时算好轮次警告并把文案发出去；移植版只负责计数，
/// 警告的渲染推迟到 `before_provider_request`（[`crate::hook::reminder_text`]）。
/// 计数器在一个 turn 内不变，因此两处算出来的级别逐位相同，而推迟渲染让注入块在
/// 同一个 turn 内逐字节稳定（provider 前缀缓存不受影响）。
pub fn bump(current: &mut SessionState) {
    if current.state == ThinkingState::END {
        return;
    }
    current.state_turn_count += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MasterTaskType, SubTaskType};

    fn profile(state: ThinkingState, difficulty: Difficulty) -> SessionState {
        SessionState {
            state,
            difficulty: Some(difficulty),
            master_task_type: Some(MasterTaskType::CODING),
            sub_task_type: Some(SubTaskType::RUST_DEV),
            ..SessionState::default()
        }
    }

    /// START 的开放路径随难度变化：TRIVIAL 直达 EXECUTE，SIMPLE 多一个 DESIGN，
    /// MODERATE 及以上必须先 DEEP_UNDERSTAND。
    #[test]
    fn start_opens_paths_by_difficulty() {
        assert_eq!(
            get_allowed_targets(ThinkingState::START, Some(Difficulty::TRIVIAL)),
            vec![ThinkingState::DEEP_UNDERSTAND, ThinkingState::EXECUTE]
        );
        assert_eq!(
            get_allowed_targets(ThinkingState::START, Some(Difficulty::SIMPLE)),
            vec![
                ThinkingState::DEEP_UNDERSTAND,
                ThinkingState::DESIGN,
                ThinkingState::EXECUTE
            ]
        );
        for difficulty in [Difficulty::MODERATE, Difficulty::COMPLEX, Difficulty::HARD, Difficulty::EXTREME] {
            assert_eq!(
                get_allowed_targets(ThinkingState::START, Some(difficulty)),
                vec![ThinkingState::DEEP_UNDERSTAND]
            );
        }
        // 难度未设定时按「其余」处理，与上游 `null` 分支一致。
        assert_eq!(
            get_allowed_targets(ThinkingState::START, None),
            vec![ThinkingState::DEEP_UNDERSTAND]
        );
    }

    #[test]
    fn end_has_no_legal_exit() {
        assert!(get_allowed_targets(ThinkingState::END, Some(Difficulty::HARD)).is_empty());
    }

    /// END 永远归入「向前」，即使它的顺序索引最大——上游对 END 有单独分支。
    #[test]
    fn targets_split_into_forward_and_backward() {
        let (forward, backward) =
            classify_targets(ThinkingState::VERIFY, Some(Difficulty::MODERATE));
        assert_eq!(forward, vec![ThinkingState::END]);
        assert_eq!(
            backward,
            vec![
                ThinkingState::EXECUTE,
                ThinkingState::DEEP_UNDERSTAND,
                ThinkingState::DESIGN
            ]
        );
    }

    #[test]
    fn next_state_hint_matches_the_upstream_wording() {
        assert_eq!(
            format_next_state_hint(ThinkingState::EXECUTE, Some(Difficulty::SIMPLE)),
            "本阶段完成可前移至[自检验证(VERIFY)]状态；本阶段不足可回退至[方案设计(DESIGN)、深度理解(DEEP_UNDERSTAND)]状态。"
        );
        // END 没有任何出口，提示为空串。
        assert_eq!(format_next_state_hint(ThinkingState::END, None), "");
    }

    #[test]
    fn set_task_info_is_rejected_outside_start_and_over_limit() {
        let mut current = SessionState::default();
        assert!(set_task_info(
            &mut current,
            Difficulty::HARD,
            MasterTaskType::CODING,
            SubTaskType::RUST_DEV
        )
        .is_ok());
        assert!(current.has_profile());

        // 已经离开 START 且未超限 → 拒绝。
        let mut current = profile(ThinkingState::DESIGN, Difficulty::MODERATE);
        current.state_turn_count = 1;
        assert_eq!(
            set_task_info(
                &mut current,
                Difficulty::HARD,
                MasterTaskType::CODING,
                SubTaskType::RUST_DEV
            ),
            Err(SetTaskInfoError)
        );
        assert_eq!(current.difficulty, Some(Difficulty::MODERATE));
    }

    /// 超限时允许重估难度，并把状态内轮次归零。
    #[test]
    fn set_task_info_is_allowed_when_over_limit() {
        let mut current = profile(ThinkingState::DESIGN, Difficulty::TRIVIAL);
        current.state_turn_count = 101;
        assert!(set_task_info(
            &mut current,
            Difficulty::EXTREME,
            MasterTaskType::CODING,
            SubTaskType::BUG_FIX
        )
        .is_ok());
        assert_eq!(current.difficulty, Some(Difficulty::EXTREME));
        assert_eq!(current.state_turn_count, 0);
    }

    #[test]
    fn transition_rejects_self_and_unknown_targets() {
        let mut current = profile(ThinkingState::DESIGN, Difficulty::MODERATE);
        assert_eq!(
            transition(&mut current, ThinkingState::DESIGN),
            Err(TransitionError::SameState(ThinkingState::DESIGN))
        );
        assert!(matches!(
            transition(&mut current, ThinkingState::END),
            Err(TransitionError::NotAllowed { .. })
        ));
    }

    #[test]
    fn transition_from_start_requires_a_profile() {
        let mut current = SessionState::default();
        assert_eq!(
            transition(&mut current, ThinkingState::DEEP_UNDERSTAND),
            Err(TransitionError::ProfileMissing)
        );
    }

    /// 从 START 出发 task_turn_count +1，转入 END 清空画像与 visited。
    #[test]
    fn transition_tracks_task_turns_and_clears_on_end() {
        let mut current = profile(ThinkingState::START, Difficulty::TRIVIAL);
        transition(&mut current, ThinkingState::EXECUTE).expect("TRIVIAL 可直达 EXECUTE");
        assert_eq!(current.task_turn_count, 1);
        assert_eq!(current.visited, vec![ThinkingState::EXECUTE]);

        transition(&mut current, ThinkingState::VERIFY).expect("EXECUTE → VERIFY");
        transition(&mut current, ThinkingState::END).expect("VERIFY → END");
        assert_eq!(current.state, ThinkingState::END);
        assert!(!current.has_profile());
        assert!(current.visited.is_empty());
        assert_eq!(current.task_turn_count, 1);
    }

    /// 三级阈值的边界：`maxTurns` / `maxTurns + 1` / `maxTurns + max(1, ceil(maxTurns/3)) + 1`。
    #[test]
    fn warn_level_hits_the_three_thresholds_at_the_right_boundaries() {
        // maxTurns = 9 → 软提醒从 8 起，超限从 10 起，hardStop 从 9 + 3 + 1 = 13 起。
        assert_eq!(warn_level(7, 9), None);
        assert_eq!(warn_level(8, 9), Some(WarnLevel::Soft));
        assert_eq!(warn_level(9, 9), Some(WarnLevel::Soft));
        assert_eq!(warn_level(10, 9), Some(WarnLevel::Over));
        assert_eq!(warn_level(12, 9), Some(WarnLevel::Over));
        assert_eq!(warn_level(13, 9), Some(WarnLevel::HardStop));

        // maxTurns <= 2 时不发软提醒（上游 `maxTurns > 2` 守卫）。
        assert_eq!(warn_level(1, 1), None);
        assert_eq!(warn_level(2, 1), Some(WarnLevel::Over));

        // maxTurns = 0（难度未设定且状态非 START）：只有超限与 hardStop。
        assert_eq!(warn_level(0, 0), None);
        assert_eq!(warn_level(1, 0), Some(WarnLevel::Over));
        assert_eq!(warn_level(2, 0), Some(WarnLevel::HardStop));
    }

    #[test]
    fn bump_counts_turns_and_skips_end() {
        let mut current = profile(ThinkingState::VERIFY, Difficulty::TRIVIAL);
        assert_eq!(current.state_turn_count, 0);
        bump(&mut current);
        assert_eq!(current.state_turn_count, 1);
        bump(&mut current);
        assert_eq!(current.state_turn_count, 2);

        let mut ended = profile(ThinkingState::END, Difficulty::TRIVIAL);
        bump(&mut ended);
        assert_eq!(ended.state_turn_count, 0);
    }

    /// 难度分档决定措辞：TRIVIAL/SIMPLE 温和、MODERATE 标准、COMPLEX+ 严厉。
    #[test]
    fn warning_wording_is_tiered_by_difficulty() {
        assert_eq!(warn_tier(Some(Difficulty::SIMPLE)), WarnTier::Simple);
        assert_eq!(warn_tier(Some(Difficulty::MODERATE)), WarnTier::Standard);
        assert_eq!(warn_tier(Some(Difficulty::EXTREME)), WarnTier::Hard);
        assert_eq!(warn_tier(None), WarnTier::Standard);

        let simple = render_warning(
            WarnLevel::Soft,
            ThinkingState::DESIGN,
            8,
            9,
            Some(Difficulty::SIMPLE),
        );
        assert!(simple.contains("可调用 asymptotic-think_set-task-info 工具 重估难度"));
        let hard = render_warning(
            WarnLevel::HardStop,
            ThinkingState::DESIGN,
            13,
            9,
            Some(Difficulty::HARD),
        );
        assert!(hard.starts_with("\n<hardStop>⛔ 强制停止："));
        assert!(hard.ends_with("</hardStop>"));
    }
}
