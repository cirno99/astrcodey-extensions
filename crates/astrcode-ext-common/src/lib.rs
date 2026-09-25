//! AstrCode 扩展共用工具库。
//!
//! 本 crate 不依赖宿主 SDK，只提供纯逻辑与分配设施，因此可以被单元测试完整覆盖。
//! 每个扩展 crate 通过本库共享同一套 JSON、竞技场与格式化实现。
//!
//! # 与宿主数据路径的关系
//!
//! 宿主经 S5R 交给插件的 session 事件是**已经解析好的** `serde_json::Value`
//! （见 `astrcode_extension_sdk::wire::session::HostSessionEvent::payload`），
//! 线缆上没有任何原始字节暴露给插件。因此 [`json`] 服务的是**插件自己拥有的
//! 字节缓冲**（会话状态账本、本地配置文件），而不是宿主回包——那条路径上的
//! JSON 由 worker SDK 解析，插件无法介入。

pub mod arena;
pub mod config;
pub mod json;
pub mod paths;
pub mod stats;
pub mod text;
