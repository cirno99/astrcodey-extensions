//! AstrCode 的哈希锚点编辑扩展。
//!
//! 把 <https://github.com/sleepinginsummer/dsh-hashline-edit-pro>（MIT，DSH 插件）
//! 移植为磁盘 s5r 扩展。上游是 Pi Coding Agent 生态的 `pi-hashline-edit-pro`。
//!
//! **每行文本携带一个唯一的 3 字符内容哈希作为地址；编辑用哈希定位，绝不依赖行号
//! 或字符串匹配。** 文件在读取后被修改时，过期锚点会在写入前被拦下并返回新锚点反馈，
//! 因此不会出现「改错行」的静默损坏。
//!
//! # 结构
//!
//! - [`hashline`] —— 纯核心，不碰文件系统也不碰宿主，可完整单元测试；也是与原版逐行
//!   对齐的部分（含用原版 JS 实测得到的金标准）。
//! - [`fsops`] —— 读写、嗅探、BOM/行尾保真、原子写。
//! - [`state`] —— 按会话持久化的锚点快照、served 集合与 undo 记录。
//! - [`tools`] —— 三个工具的请求处理。
//! - [`worker`] —— S5R 装配。

pub mod fsops;
pub mod hashline;
pub mod prompt;
pub mod state;
pub mod tools;
pub mod worker;

pub use worker::{EXTENSION_ID, run};
