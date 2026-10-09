//! AstrCode 的 Anki 牌组生成扩展。
//!
//! **职责切分：语义活归 agent，确定性活归扩展。** 「这段 markdown 里什么值得做成卡」
//! 是判断题，agent 用自带的文件工具读 vault 笔记自行组织；「把卡片规范变成 .apkg」是
//! 工程题，由本扩展的 [`guid`] / [`apkg`] 确定性完成——GUID 由「牌组名 + 卡片稳定 id」
//! 派生，重复生成再导入 Anki 是更新而不是重复卡。
//!
//! # 结构
//!
//! - [`spec`] —— JSON 牌组规范的类型与纯校验（不碰文件系统）。
//! - [`guid`] —— 稳定 GUID 与牌组 id 派生（SHA-256 + Anki 的 base91）。
//! - [`apkg`] —— apkg 写入器：SQLite collection + zip 外壳 + 媒体打包。
//! - [`worker`] —— S5R 装配（注册 `anki_write_apkg` 工具与 `prompt_build` 引导）。

pub mod apkg;
pub mod error;
pub mod guid;
pub mod prompt;
pub mod spec;
pub mod worker;

pub use worker::{EXTENSION_ID, run};
