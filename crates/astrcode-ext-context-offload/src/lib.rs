//! `astrcode-ext-context-offload`：把过大的工具输出换成可检索的占位符。
//!
//! # 这个插件能做什么
//!
//! - `post_tool_use` 钩子：工具结果超过阈值时，原文落盘、可见内容替换为确定性占位符。
//!   模型仍能看到「哪个工具、多大、头部预览」，据此判断是否值得取回。
//! - `retrieve` 工具：按占位符里的 ref 取回完整原文（支持分页）。
//! - `/offload` 命令：查看本会话已 offload 的条目与合计大小。
//!
//! # 这个插件做不到什么
//!
//! - **无法撤销 transcript 里的替换**。`post_tool_use` 的 `ModifyResult` 改的是即将
//!   落盘的 `ToolResult.content`，宿主会把替换后的文本写进 durable 记录，因此原文
//!   必须由本插件自己保存——这正是 `store` 模块存在的原因。
//! - **无法注册状态栏条目**。S5R 的 `InitializeManifest` 没有 `status_items` 字段，
//!   状态栏那一格只在第一次执行 `/offload` 之后才出现。
//! - **不感知 token 预算**。阈值以字符数为准（见 [`offload::OffloadPolicy`]），
//!   不调用宿主 token 计数，避免在工具热路径上引入额外 IPC。
//!
//! # 为什么不用宿主的 session state
//!
//! `astrcode.session.state` 的单值上限是 1 MiB（`HOST_SESSION_STATE_VALUE_MAX_BYTES`），
//! 而大工具输出经常超过这个量级；且每次读写都要走一次 IPC。原文属于插件自己拥有的
//! 数据，因此按 `hostpaths` 直接落盘，按 session 隔离。

pub mod command;
pub mod hook;
pub mod offload;
pub mod store;
pub mod tool;

pub use command::{EXTENSION_ID, run};
