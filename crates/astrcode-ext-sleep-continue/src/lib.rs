//! 无人值守续跑插件的 crate 文档。详见 [`command::run`] 与仓库根 `README.md`。
//!
//! # 它解决什么
//!
//! 长任务在无人看守时停在哪三种地方：模型以为干完了、模型问了个选择题在等人点、模型空转。
//! 本插件分别用 `continue_after_stop` + `defer_context`、`pre_tool_use` 拦截提问、以及
//! 空转熔断处理这三件事。
//!
//! # 分层
//!
//! - [`plan`] 是**纯判定**：给定载荷与运行期状态，决定续跑还是停下、生成拦截原因。
//!   不碰宿主，可完整单元测试。
//! - [`hook`] 是**适配层**：取参 → 调 [`plan`] → 落状态 → 映射回 S5R 结果。
//! - [`state`] 收敛配置缓存、会话开关、运行期表与统计。
//! - [`command`] 装配 worker 并提供 `/sleep` 命令面。

pub mod command;
pub mod config;
pub mod hook;
pub mod plan;
pub mod state;

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-sleep-continue";

/// 命令名，宿主侧解析为 `/sleep`。
pub const COMMAND_NAME: &str = "sleep";

pub use command::run;
