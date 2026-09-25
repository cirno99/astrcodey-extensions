//! `astrcode-rtk-optimizer`：把 shell 命令改写成 rtk 等价命令，并压缩工具输出。
//!
//! 上游 [pi-rtk-optimizer](https://github.com/MasuRii/pi-rtk-optimizer)（Pi 扩展，MIT）
//! 到 AstrCode 磁盘扩展的移植。
//!
//! # 这个插件能做什么
//!
//! - `tool_input_transform` 钩子：`shell` 工具调用前，把命令交给外部 `rtk rewrite`
//!   改写（`rewrite` 模式）或只记录建议（`suggest` 模式）。
//! - `post_tool_use` 钩子：`shell`/`read`/`grep` 的工具结果走多级压缩管线，降低上下文占用。
//! - `prompt_build` 钩子：有损 `read` 压缩开启时注入排查提示。
//! - `/rtk` 命令：查看与在线修改配置、探测 rtk 可用性、查看压缩节省统计。
//!
//! # 这个插件做不到什么
//!
//! 1. **无法推送 TUI 通知。** S5R 钩子的返回值里没有面向用户的文本通道
//!    （`Replace` 只换工具入参，`ModifyResult` 只换工具结果正文），因此「命令被改写」
//!    与「建议改写」只能记进运行期状态，由 `/rtk show` 事后查询。
//! 2. **无法注册状态栏条目。** S5R 的 `InitializeManifest` 没有 `status_items` 字段，
//!    统计只能靠 `/rtk stats` 拉取。
//! 3. **没有流式输出清洗。** 上游监听 `tool_execution_update` 清洗流式 bash 输出；
//!    宿主没有对应钩子，shell 工具也不向钩子暴露部分输出。
//! 4. **`read` 压缩是有损的。** 它会丢掉整行，后续 `edit` 的 `oldText` 匹配可能因此失败。
//!    与上游一致默认关闭；开启且同时启用源码过滤与截断时会注入排查提示。
//!
//! # 配置
//!
//! 落在 `<astrcode_dir>/extension_data/astrcode-rtk-optimizer/config.json`，是**全局**
//! 配置而非按会话。字段名、默认值与取值范围与上游一致，用户手改坏的文件会按字段回落
//! 默认值而不是让插件失效。

pub mod anchored_read;
pub mod command;
pub mod compact;
pub mod config;
pub mod hook;
pub mod metrics;
pub mod rewrite;
pub mod rtk;
pub mod shell;
pub mod techniques;

/// 扩展 ID，必须与 `extension.json` 的 `extension_id` 一致。
pub const EXTENSION_ID: &str = "astrcode-rtk-optimizer";

/// 命令名，宿主侧解析为 `/rtk`。
pub const COMMAND_NAME: &str = "rtk";

pub use command::run;
