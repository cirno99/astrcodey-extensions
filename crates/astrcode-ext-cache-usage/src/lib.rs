//! `astrcode-ext-cache-usage`：查看当前会话的 prompt 缓存命中率。
//!
//! # 这个插件能做什么
//!
//! - `/usage` 斜杠命令：扫描当前会话的 durable 事件流，汇总每个模型请求的
//!   `TokenUsageRecorded` 载荷，输出一行命中率汇总。
//! - 状态栏一格：命令执行时随结果携带 `status_update`，宿主下发通知后
//!   命令行版与网页版输入栏都会显示。
//!
//! # 这个插件做不到什么
//!
//! - **无法注册状态栏条目**。S5R 的 `InitializeManifest` 没有 `status_items`
//!   字段且 `deny_unknown_fields`，宿主 `S5rExtension::register()` 也不注册任何
//!   状态栏项。因此那一格只有在第一次执行 `/usage` 之后才会出现。
//! - **无法每轮自动刷新**。向外推送状态更新的入口只有「命令结果携带
//!   `status_update`」这一条，钩子返回值没有对应字段。
//!
//! 逻辑放在库里而不是 `main.rs`，是为了让集成测试能通过 worker 的 `testing`
//! 缝隙注入模拟宿主，直接驱动 [`scan::scan_session`] 走完整分页路径。

pub mod command;
pub mod report;
pub mod scan;

pub use command::run;
