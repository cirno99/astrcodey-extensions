//! 状态机类型定义（双层任务类型 v4），逐条对照上游 `src/types.ts`。
//!
//! 枚举一律以**大写线缆名**序列化（`"RUST_DEV"`），与上游 TypeScript 的字符串枚举、
//! 以及工具参数里模型填写的取值保持一致；反序列化放宽到大小写不敏感，因为上游
//! `prepareArguments` 正是靠转大写来容忍模型传小写枚举。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 状态机工具名（逐字照抄上游 `types.ts` 的常量）。
pub const TOOL_TRANSITION: &str = "asymptotic-think_transition";
/// 设定任务画像的工具名。
pub const TOOL_TASK_INFO: &str = "asymptotic-think_set-task-info";
/// 查询状态机状态的工具名。
pub const TOOL_STATUS: &str = "asymptotic-think_status";

/// 生成「大写线缆名 + 全量清单 + 大小写不敏感解析 + serde」的字符串枚举。
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),* $(,)? }) => {
        $(#[$meta])*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($variant),*
        }

        impl $name {
            /// 全量变体，顺序与上游 `Object.keys(labels)` 一致。
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// 线缆名（大写）。
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),*
                }
            }

            /// 大小写不敏感解析；未知取值返回 `None`。
            pub fn parse(raw: &str) -> Option<Self> {
                let upper = raw.trim().to_ascii_uppercase();
                match upper.as_str() {
                    $($wire => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::parse(&raw).ok_or_else(|| {
                    serde::de::Error::custom(format!("未知的 {}：{raw}", stringify!($name)))
                })
            }
        }
    };
}

string_enum! {
    /// 六态状态机：START 起点 + 四业务态 + END 终态。
    ThinkingState {
        START => "START",
        DEEP_UNDERSTAND => "DEEP_UNDERSTAND",
        DESIGN => "DESIGN",
        EXECUTE => "EXECUTE",
        VERIFY => "VERIFY",
        END => "END",
    }
}

string_enum! {
    /// 任务难度。
    Difficulty {
        TRIVIAL => "TRIVIAL",
        SIMPLE => "SIMPLE",
        MODERATE => "MODERATE",
        COMPLEX => "COMPLEX",
        HARD => "HARD",
        EXTREME => "EXTREME",
    }
}

string_enum! {
    /// 大任务类型（6 类）。
    MasterTaskType {
        CODING => "CODING",
        RETRIEVAL => "RETRIEVAL",
        ANALYTICS => "ANALYTICS",
        DEVOPS => "DEVOPS",
        ENTERTAINMENT => "ENTERTAINMENT",
        GENERAL => "GENERAL",
    }
}

string_enum! {
    /// 小任务类型（27 类）。
    SubTaskType {
        JAVA_DEV => "JAVA_DEV",
        RUST_DEV => "RUST_DEV",
        PYTHON_DEV => "PYTHON_DEV",
        JS_DEV => "JS_DEV",
        GO_DEV => "GO_DEV",
        CRUD_DEV => "CRUD_DEV",
        BUG_FIX => "BUG_FIX",
        CODE_REFACTOR => "CODE_REFACTOR",
        TESTING => "TESTING",
        ARCHITECT => "ARCHITECT",
        CODE_REVIEW => "CODE_REVIEW",
        PERF_OPTIMIZE => "PERF_OPTIMIZE",
        PAPER_RETRIEVAL => "PAPER_RETRIEVAL",
        DAILY_RETRIEVAL => "DAILY_RETRIEVAL",
        DOC_RETRIEVAL => "DOC_RETRIEVAL",
        CODE_RETRIEVAL => "CODE_RETRIEVAL",
        DATA_ANALYSIS => "DATA_ANALYSIS",
        CODE_ANALYSIS => "CODE_ANALYSIS",
        LOG_ANALYSIS => "LOG_ANALYSIS",
        REQUIREMENT_ANALYSIS => "REQUIREMENT_ANALYSIS",
        DEPLOY => "DEPLOY",
        MONITOR => "MONITOR",
        CICD => "CICD",
        CONFIG => "CONFIG",
        FUN_CHAT => "FUN_CHAT",
        CREATIVE_WRITING => "CREATIVE_WRITING",
        GENERAL => "GENERAL",
    }
}

/// 任务难度中文标签。
pub fn difficulty_label(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::TRIVIAL => "微不足道",
        Difficulty::SIMPLE => "简单",
        Difficulty::MODERATE => "中等",
        Difficulty::COMPLEX => "复杂",
        Difficulty::HARD => "困难",
        Difficulty::EXTREME => "极难",
    }
}

/// 大任务类型中文标签。
pub fn master_task_type_label(master: MasterTaskType) -> &'static str {
    match master {
        MasterTaskType::CODING => "编程类",
        MasterTaskType::RETRIEVAL => "检索类",
        MasterTaskType::ANALYTICS => "分析类",
        MasterTaskType::DEVOPS => "运维类",
        MasterTaskType::ENTERTAINMENT => "娱乐类",
        MasterTaskType::GENERAL => "通用类",
    }
}

/// 小任务类型中文标签。
pub fn sub_task_type_label(sub: SubTaskType) -> &'static str {
    match sub {
        SubTaskType::JAVA_DEV => "Java开发",
        SubTaskType::RUST_DEV => "Rust开发",
        SubTaskType::PYTHON_DEV => "Python开发",
        SubTaskType::JS_DEV => "JavaScript开发",
        SubTaskType::GO_DEV => "Go开发",
        SubTaskType::CRUD_DEV => "增删改查",
        SubTaskType::BUG_FIX => "缺陷修复",
        SubTaskType::CODE_REFACTOR => "代码重构",
        SubTaskType::TESTING => "程序测试",
        SubTaskType::ARCHITECT => "架构设计",
        SubTaskType::CODE_REVIEW => "代码审查",
        SubTaskType::PERF_OPTIMIZE => "性能优化",
        SubTaskType::PAPER_RETRIEVAL => "论文检索",
        SubTaskType::DAILY_RETRIEVAL => "日常检索",
        SubTaskType::DOC_RETRIEVAL => "文档检索",
        SubTaskType::CODE_RETRIEVAL => "代码检索",
        SubTaskType::DATA_ANALYSIS => "数据分析",
        SubTaskType::CODE_ANALYSIS => "代码分析",
        SubTaskType::LOG_ANALYSIS => "日志分析",
        SubTaskType::REQUIREMENT_ANALYSIS => "需求分析",
        SubTaskType::DEPLOY => "部署上线",
        SubTaskType::MONITOR => "监控告警",
        SubTaskType::CICD => "CI/CD",
        SubTaskType::CONFIG => "环境配置",
        SubTaskType::FUN_CHAT => "休闲聊天",
        SubTaskType::CREATIVE_WRITING => "创意写作",
        SubTaskType::GENERAL => "通用",
    }
}

/// 状态中文标签。
pub fn state_label(state: ThinkingState) -> &'static str {
    match state {
        ThinkingState::START => "启动",
        ThinkingState::DEEP_UNDERSTAND => "深度理解",
        ThinkingState::DESIGN => "方案设计",
        ThinkingState::EXECUTE => "执行",
        ThinkingState::VERIFY => "自检验证",
        ThinkingState::END => "结束",
    }
}

/// 大类型 → 可用子类型映射（工具参数校验与 `status` 输出用）。
pub fn master_to_sub(master: MasterTaskType) -> &'static [SubTaskType] {
    match master {
        MasterTaskType::CODING => &[
            SubTaskType::JAVA_DEV,
            SubTaskType::RUST_DEV,
            SubTaskType::PYTHON_DEV,
            SubTaskType::JS_DEV,
            SubTaskType::GO_DEV,
            SubTaskType::CRUD_DEV,
            SubTaskType::BUG_FIX,
            SubTaskType::CODE_REFACTOR,
            SubTaskType::TESTING,
            SubTaskType::ARCHITECT,
            SubTaskType::CODE_REVIEW,
            SubTaskType::PERF_OPTIMIZE,
        ],
        MasterTaskType::RETRIEVAL => &[
            SubTaskType::PAPER_RETRIEVAL,
            SubTaskType::DAILY_RETRIEVAL,
            SubTaskType::DOC_RETRIEVAL,
            SubTaskType::CODE_RETRIEVAL,
        ],
        MasterTaskType::ANALYTICS => &[
            SubTaskType::DATA_ANALYSIS,
            SubTaskType::CODE_ANALYSIS,
            SubTaskType::LOG_ANALYSIS,
            SubTaskType::REQUIREMENT_ANALYSIS,
        ],
        MasterTaskType::DEVOPS => &[
            SubTaskType::DEPLOY,
            SubTaskType::MONITOR,
            SubTaskType::CICD,
            SubTaskType::CONFIG,
        ],
        MasterTaskType::ENTERTAINMENT => {
            &[SubTaskType::FUN_CHAT, SubTaskType::CREATIVE_WRITING]
        },
        MasterTaskType::GENERAL => &[SubTaskType::GENERAL],
    }
}

/// 大任务类型领域提示（注入到 `<instruction>` 块首行）。
pub fn master_task_hint(master: MasterTaskType) -> &'static str {
    match master {
        MasterTaskType::CODING => "编程类任务——注意代码结构、测试覆盖和错误处理",
        MasterTaskType::RETRIEVAL => "检索类任务——多渠道并行搜索、信息去重和来源标注",
        MasterTaskType::ANALYTICS => "分析类任务——数据来源可靠、分析逻辑严谨、结论有据",
        MasterTaskType::DEVOPS => "运维类任务——环境配置验证、操作影响评估、回滚方案",
        MasterTaskType::ENTERTAINMENT => "娱乐类任务——风格一致、内容有趣、安全合规",
        MasterTaskType::GENERAL => "通用任务——根据上下文判定具体侧重点",
    }
}

/// 难度 × 状态 的显式轮次上限表。
///
/// 调整说明（上游 2026-08-07 v3）：全阶段均衡上调——每个阶段模型都可能调用工具
/// （DEEP_UNDERSTAND 需 read/grep/web_fetch；DESIGN 需 read/bash；VERIFY 需 read/bash/grep），
/// 旧值参考（2026-08-05）：TRIVIAL 3 / SIMPLE 6 / MODERATE 10 / COMPLEX 15 / HARD 18 / EXTREME 22。
pub fn state_max_turns(state: ThinkingState, difficulty: Difficulty) -> u32 {
    use Difficulty::*;
    use ThinkingState::*;
    match difficulty {
        TRIVIAL => match state {
            START => 1,
            DEEP_UNDERSTAND => 200,
            DESIGN => 100,
            EXECUTE => 500,
            VERIFY => 100,
            END => 1,
        },
        SIMPLE => match state {
            START => 1,
            DEEP_UNDERSTAND => 500,
            DESIGN => 300,
            EXECUTE => 1500,
            VERIFY => 300,
            END => 1,
        },
        MODERATE => match state {
            START => 1,
            DEEP_UNDERSTAND => 1500,
            DESIGN => 1000,
            EXECUTE => 4000,
            VERIFY => 1000,
            END => 1,
        },
        COMPLEX => match state {
            START => 1,
            DEEP_UNDERSTAND => 3000,
            DESIGN => 2000,
            EXECUTE => 7000,
            VERIFY => 2000,
            END => 1,
        },
        HARD => match state {
            START => 1,
            DEEP_UNDERSTAND => 5000,
            DESIGN => 3500,
            EXECUTE => 9000,
            VERIFY => 3000,
            END => 1,
        },
        EXTREME => match state {
            START => 1,
            DEEP_UNDERSTAND => 7000,
            DESIGN => 5000,
            EXECUTE => 10000,
            VERIFY => 4000,
            END => 1,
        },
    }
}

/// 按状态与难度查表取轮次上限。
///
/// 难度未设定（START 阶段）时 START 最低 1 轮，其余状态防御性返回 0——
/// 与上游 `getMaxStateTurns` 的 `!difficulty` 分支逐位一致。
pub fn get_max_state_turns(state: ThinkingState, difficulty: Option<Difficulty>) -> u32 {
    match difficulty {
        None => u32::from(state == ThinkingState::START),
        Some(difficulty) => state_max_turns(state, difficulty),
    }
}

/// `turn_end` 提醒间隔：`stateTurnCount % interval == 0` 时才发送，避免稀释正常提示词。
pub fn reminder_interval(state: ThinkingState, difficulty: Difficulty) -> u32 {
    use Difficulty::*;
    use ThinkingState::*;
    match difficulty {
        TRIVIAL => match state {
            START | END => 0,
            DEEP_UNDERSTAND => 5,
            DESIGN => 8,
            EXECUTE => 12,
            VERIFY => 8,
        },
        SIMPLE => match state {
            START | END => 0,
            DEEP_UNDERSTAND => 6,
            DESIGN => 10,
            EXECUTE => 15,
            VERIFY => 10,
        },
        MODERATE => match state {
            START | END => 0,
            DEEP_UNDERSTAND => 8,
            DESIGN => 12,
            EXECUTE => 18,
            VERIFY => 12,
        },
        COMPLEX => match state {
            START | END => 0,
            DEEP_UNDERSTAND => 10,
            DESIGN => 15,
            EXECUTE => 20,
            VERIFY => 15,
        },
        HARD => match state {
            START | END => 0,
            DEEP_UNDERSTAND => 12,
            DESIGN => 18,
            EXECUTE => 25,
            VERIFY => 18,
        },
        EXTREME => match state {
            START | END => 0,
            DEEP_UNDERSTAND => 15,
            DESIGN => 20,
            EXECUTE => 30,
            VERIFY => 20,
        },
    }
}

/// 按状态与难度查表取提醒间隔；难度未设定时返回 1（每轮提醒）。
///
/// 上游写的是 `REMINDER_INTERVAL[difficulty][state] || 1`，因此表里的 0 也回落 1。
pub fn get_reminder_interval(state: ThinkingState, difficulty: Option<Difficulty>) -> u32 {
    match difficulty {
        None => 1,
        Some(difficulty) => reminder_interval(state, difficulty).max(1),
    }
}

/// 六态流程图（水平箭头，使用中文标签）。
pub fn flow_diagram() -> String {
    ThinkingState::ALL
        .iter()
        .map(|state| state_label(*state))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// 把 `KEY(中文标签) | KEY(中文标签)` 形式的枚举说明拼出来（工具参数描述用）。
fn build_describe(entries: &[(&str, &str)]) -> String {
    entries
        .iter()
        .map(|(key, label)| format!("{key}({label})"))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// `difficulty` 参数描述。
pub fn tool_describe_difficulty() -> String {
    build_describe(
        &Difficulty::ALL
            .iter()
            .map(|value| (value.as_str(), difficulty_label(*value)))
            .collect::<Vec<_>>(),
    )
}

/// `masterTaskType` 参数描述。
pub fn tool_describe_master() -> String {
    build_describe(
        &MasterTaskType::ALL
            .iter()
            .map(|value| (value.as_str(), master_task_type_label(*value)))
            .collect::<Vec<_>>(),
    )
}

/// `subTaskType` 参数描述。
pub fn tool_describe_sub() -> String {
    build_describe(
        &SubTaskType::ALL
            .iter()
            .map(|value| (value.as_str(), sub_task_type_label(*value)))
            .collect::<Vec<_>>(),
    )
}

/// 每条会话的状态记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionState {
    /// 当前状态（六态之一）。上游允许 `null` 表示「新建会话未初始化」，
    /// 移植版在载入时把它归一成 [`ThinkingState::START`]，因此这里是不可空枚举。
    pub state: ThinkingState,
    /// 任务难度，START 时由 `set-task-info` 设定，转入 END 时清空。
    pub difficulty: Option<Difficulty>,
    /// 大任务类型，与 `difficulty` 同生命周期。
    pub master_task_type: Option<MasterTaskType>,
    /// 小任务类型，与 `difficulty` 同生命周期。
    pub sub_task_type: Option<SubTaskType>,
    /// 任务轮次计数：每次从 START 转移时 +1，即已启动的任务数。
    pub task_turn_count: u32,
    /// 状态内轮次计数：当前状态内已消耗的轮次，转移时归零。
    pub state_turn_count: u32,
    /// 最近一次状态转移的时间戳（毫秒）。
    pub last_transition_time: u64,
    /// 本次任务经过的状态路径（全链路路径感知），转入 END 时清空。
    pub visited: Vec<ThinkingState>,
}

impl Default for SessionState {
    /// 新会话的初始状态：`state = START`，任务画像全空，计数器归零。
    fn default() -> Self {
        Self {
            state: ThinkingState::START,
            difficulty: None,
            master_task_type: None,
            sub_task_type: None,
            task_turn_count: 0,
            state_turn_count: 0,
            last_transition_time: 0,
            visited: Vec::new(),
        }
    }
}

impl SessionState {
    /// 任务画像是否已设定（START 流转的前置条件）。
    pub fn has_profile(&self) -> bool {
        self.difficulty.is_some()
            && self.master_task_type.is_some()
            && self.sub_task_type.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_their_wire_names() {
        assert_eq!(ThinkingState::ALL.len(), 6);
        assert_eq!(Difficulty::ALL.len(), 6);
        assert_eq!(MasterTaskType::ALL.len(), 6);
        assert_eq!(SubTaskType::ALL.len(), 27);

        for state in ThinkingState::ALL {
            assert_eq!(ThinkingState::parse(state.as_str()), Some(*state));
        }
        for difficulty in Difficulty::ALL {
            assert_eq!(Difficulty::parse(difficulty.as_str()), Some(*difficulty));
        }
        for master in MasterTaskType::ALL {
            assert_eq!(MasterTaskType::parse(master.as_str()), Some(*master));
        }
        for sub in SubTaskType::ALL {
            assert_eq!(SubTaskType::parse(sub.as_str()), Some(*sub));
        }
    }

    /// 上游 `prepareArguments` 会把模型传的小写枚举转大写，解析必须同样宽容。
    #[test]
    fn parsing_is_case_insensitive() {
        assert_eq!(Difficulty::parse("rust_dev"), None);
        assert_eq!(SubTaskType::parse("rust_dev"), Some(SubTaskType::RUST_DEV));
        assert_eq!(SubTaskType::parse(" Rust_Dev "), Some(SubTaskType::RUST_DEV));
        assert_eq!(MasterTaskType::parse("coding"), Some(MasterTaskType::CODING));
        assert_eq!(ThinkingState::parse("design"), Some(ThinkingState::DESIGN));
    }

    #[test]
    fn unknown_wire_names_are_rejected() {
        assert_eq!(ThinkingState::parse("IDLE"), None);
        assert_eq!(Difficulty::parse(""), None);
        assert_eq!(SubTaskType::parse("RUST"), None);
    }

    #[test]
    fn the_master_to_sub_table_covers_every_sub_type_exactly_once() {
        let mut seen = Vec::new();
        for master in MasterTaskType::ALL {
            seen.extend_from_slice(master_to_sub(*master));
        }
        seen.sort();
        let mut all = SubTaskType::ALL.to_vec();
        all.sort();
        assert_eq!(seen, all);
    }

    /// START/END 的轮次上限恒为 1；难度未设定时只有 START 有 1 轮。
    #[test]
    fn max_turns_follows_the_upstream_table() {
        assert_eq!(get_max_state_turns(ThinkingState::START, None), 1);
        assert_eq!(get_max_state_turns(ThinkingState::DESIGN, None), 0);
        assert_eq!(
            get_max_state_turns(ThinkingState::EXECUTE, Some(Difficulty::EXTREME)),
            10000
        );
        assert_eq!(
            get_max_state_turns(ThinkingState::START, Some(Difficulty::EXTREME)),
            1
        );
    }

    /// 上游用 `|| 1`，所以表里的 0 必须回落 1，而不是原样返回 0。
    #[test]
    fn reminder_interval_falls_back_to_one() {
        assert_eq!(get_reminder_interval(ThinkingState::START, None), 1);
        assert_eq!(
            get_reminder_interval(ThinkingState::START, Some(Difficulty::HARD)),
            1
        );
        assert_eq!(
            get_reminder_interval(ThinkingState::EXECUTE, Some(Difficulty::EXTREME)),
            30
        );
    }

    #[test]
    fn the_flow_diagram_lists_all_six_states_in_order() {
        assert_eq!(
            flow_diagram(),
            "启动 → 深度理解 → 方案设计 → 执行 → 自检验证 → 结束"
        );
    }

    #[test]
    fn tool_describes_list_every_variant_with_its_label() {
        let describe = tool_describe_difficulty();
        assert!(describe.starts_with("TRIVIAL(微不足道) | SIMPLE(简单)"));
        assert!(describe.ends_with("EXTREME(极难)"));
        assert!(tool_describe_sub().contains("RUST_DEV(Rust开发)"));
        assert!(tool_describe_master().contains("ENTERTAINMENT(娱乐类)"));
    }
}
