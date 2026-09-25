//! 「we need」思维链引导规范的 AstrCode 磁盘插件。
//!
//! 把 <https://github.com/Nwflower/dsh-weneed>（DSH 插件，MIT）移植到 AstrCode，并参考
//! phi-deepseek-enhanced 补齐「注入规范」之外提高遵循率的手段。
//!
//! # 四个钩子
//!
//! 1. **`prompt_build`** —— 在 DeepSeek 会话上把完整规范注入 system prompt 的静态前缀区
//!    （宿主映射为 `ExtensionSection::PlatformInstructions`）。
//! 2. **`provider_contribution`** —— 需要时往请求尾部追加一条极简风格提醒。
//! 3. **`after_provider_response`** —— 读 assistant 的推理通道判定风格漂移，决定下一次请求
//!    是否需要提醒。
//! 4. **`pre_tool_use`** —— 黑名单工具守卫（默认关闭）。
//!
//! # 与原版 dsh-weneed 的差异
//!
//! 1. **触发条件是模型，不是手动开关**。原版靠 `autoEnable` 配置或 agent-preset 常驻；
//!    本插件按模型 id 自动判定，只在 DeepSeek 会话注入。
//! 2. **开关按会话持久化**。原版把状态放在进程内存里；本插件写宿主的 `session_state`，
//!    扩展重载后仍然有效。
//! 3. **不移植 agent-preset**。AstrCode 没有 DSH 那套预设装配接口，规范注入改由
//!    `prompt_build` 钩子承担。
//!
//! # 刻意不移植 phi-deepseek-enhanced 的两件事
//!
//! 1. **「首轮完整锚点 + 之后每轮极简提醒」的轮次节奏**。那是 phi 的架构补丁：phi 只能把
//!    锚点追加到**当前用户消息末尾**，注入一次就随历史滚远，所以必须反复补。AstrCode 的
//!    `prompt_build` **每轮**都会重新贡献规范，宿主把它放进 system prompt 的静态前缀区，
//!    既不会被淹没也不会被压缩丢掉。反过来，按轮次变换贡献内容会**每轮击穿 provider
//!    前缀缓存**，代价远大于收益。
//! 2. **「压缩后重注入」**。同上：规范在 system prompt 里，不参与 transcript 重写，
//!    压缩不会让它失效。
//!
//! phi 真正值得移植的是 **recency 杠杆**，但它在 AstrCode 里的对应物不是「改 system prompt
//! 的内容」，而是 `provider_contribution` 的请求局部追加，见 [`hook::plan_reminder`]。

pub mod command;
pub mod config;
pub mod drift;
pub mod guard;
pub mod hook;
pub mod model;
pub mod reminder;
pub mod spec;
pub mod state;
pub mod toggle;

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-weneed";

/// 命令名，宿主侧解析为 `/weneed`。
pub const COMMAND_NAME: &str = "weneed";

pub use command::run;
