//! 提示词改写插件的 crate 文档。详见 [`command::run`] 与仓库根 `README.md`。
//!
//! # 它解决什么
//!
//! 用户随手写下的草稿常常缺目标、缺范围、缺验收条件，模型只能猜。本插件在**发送之前**把草稿
//! 交给一个改写模型，加强成目标/约束/验收都写明白的提示词，然后**只预览**：用户复制或编辑后
//! 自己发，或者用 `/smith go` 原样发出。插件自己不替用户发送任何东西。
//!
//! # 分层
//!
//! - [`intent`] 是**纯判定**：草稿 → 任务意图 + 生效模式。不碰宿主，可完整单元测试。
//! - [`prompt`] 是英文工件：发给改写模型的 system / 模式指引 / 上下文区。
//! - [`enhance`] 是**调用与清洗**：选档位、发请求、按哨兵提取正文。
//! - [`pending`] 是**会话级暂存**：把改写结果存进宿主 `session_state`，给 `/smith go` 用。
//! - [`config`] 是全局配置（模式、力度、档位）。
//! - [`command`] 装配 worker，提供 `/smith` 命令面与参数路由。
//!
//! `family` 模块**没有移植**（上游按 OpenAI / Anthropic 两系微调 prompt 风格）。理由见
//! [`prompt`] 模块头：本仓库使用者的激活模型 deepseek-* / glm-* / qwen-* 全都不属于那两系，
//! 分叉永远走兜底，留着只会让人误以为它做了实际没做的事。

pub mod command;
pub mod config;
pub mod enhance;
pub mod intent;
pub mod pending;
pub mod prompt;

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-promptsmith";

/// 命令名，宿主侧解析为 `/smith`。
pub const COMMAND_NAME: &str = "smith";

pub use command::run;
