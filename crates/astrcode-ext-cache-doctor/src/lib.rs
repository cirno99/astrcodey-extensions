//! `astrcode-ext-cache-doctor`：观测 provider 请求的 prompt 前缀，定位缓存断点。
//!
//! 移植自 [pi-cache-optimizer](https://github.com/jiangge/pi-cache-optimizer)（MIT）的
//! **可落地子集**。上游是 pi（TypeScript 宿主）的扩展，能力面与 astrcode 的 s5r 磁盘
//! 插件并不等价，因此这里不是逐条搬运，而是把上游的核心思路（前缀稳定性）落到
//! astrcode 真正存在的那一侧。
//!
//! # 这个插件能做什么
//!
//! - `before_provider_request` 钩子：为每次请求计算 provider 可见消息的逐消息指纹，
//!   与同一会话的上一次请求求最长公共前缀，从而指出**历史被追溯改写**的位置。
//! - `/cache-doctor` 命令族：`status` / `doctor` / `stats` / `reset` / `enable` /
//!   `disable` / `config` / `help`。
//! - 状态栏一格（`cache-prefix`）：随 `status` / `doctor` / `stats` 的结果携带
//!   `status_update` 下发。
//!
//! # 这个插件做不到什么
//!
//! 这些是宿主能力边界，不是实现取舍：
//!
//! 1. **不能改写请求**。`before_provider_request` 只能返回
//!    `Allow` / `Block` / `ReplaceMessages` / `AppendMessages`，没有 tools、没有
//!    provider 参数、没有请求体。因此上游的 `prompt_cache_key` 兜底、长缓存保留、
//!    Anthropic TTL 顺序修正、工具重排在这里都无处落地——而且 astrcode 宿主已经
//!    内建了前两项（见 README 的能力对照表）。
//! 2. **拿不到 provider 身份**。宿主组装钩子上下文时走 `ModelSelection::simple`，
//!    `profile_name` 与 `provider_kind` 恒为空串，因此无法按 provider/api 给出
//!    「代理未开启会话亲和」这类兼容建议。
//! 3. **不能注册常驻状态栏条目**。S5R 的 `InitializeManifest` 没有 `status_items`
//!    字段，那一格只在第一次执行 `/cache-doctor status`（或 `doctor` / `stats`）
//!    之后才出现。
//! 4. **不能改宿主配置**。扩展被圈禁在 workspace 内，读不到也写不了
//!    `~/.astrcode/config.toml`，因此上游的 `fix` / `rollback` 无法移植。
//!
//! # 为什么不重排 system prompt
//!
//! 上游最核心的一步是「把稳定的 system prompt 内容提到动态内容之前」。astrcode 的
//! system prompt 由 `astrcode-context::prompt_engine` 按固定 section 顺序组装，稳定段
//! （Identity / System / Task Guidelines / Communication）本来就在最前，重排是空操作。
//! 实测同一会话内 system prompt 逐字节稳定（`cached_input_tokens` 单调跟随上一次
//! `input_tokens`，从未重置），因此把「重排」搬过来只会得到一段永不生效的死代码。
//! 真正会毁掉前缀缓存的是**历史消息被追溯改写**，本插件做的就是把它指出来。

pub mod command;
pub mod config;
pub mod prefix;
pub mod report;
pub mod usage;
pub mod watch;

pub use command::{EXTENSION_ID, run};
