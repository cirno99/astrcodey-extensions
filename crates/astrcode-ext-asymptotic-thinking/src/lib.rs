//! 渐近式思考状态机的 AstrCode 磁盘插件。
//!
//! 把 <https://github.com/cirno99/pi-backup> 里 pi 扩展 `asymptotic-thinking`
//! （快照 `pi-config-20260916-191214`，MIT）移植到 AstrCode：用一个六态状态机
//! （START → DEEP_UNDERSTAND → DESIGN → EXECUTE → VERIFY → END）约束模型的推理节奏，
//! 并按「任务大类型 × 子类型 × 难度」注入对应的领域提示词。
//!
//! # 与 `astrcode-ext-weneed` 的关系
//!
//! 本插件**不设模型闸门**，适用于所有模型。weneed 注入的是 DeepSeek 特化的推理句式规范，
//! 本插件注入的是状态机框架与领域提示词，两者内容不重叠，在 DeepSeek 上可以同时启用。
//!
//! # 与原版的差异
//!
//! 上游是 pi（TypeScript 宿主）的扩展，能力面与 S5R 磁盘插件并不等价。逐条差异与
//! 不可达的能力记录在 README 的 `astrcode-asymptotic-thinking` 小节，此处只列要点：
//!
//! 1. **推理参数调优整层删除。** 上游按「大类型基础值 + 小类型微调 + 难度偏移」三层叠加
//!    `temperature`/`top_p`；AstrCode 没有请求体钩子（`ProviderResult` 只有四个变体，
//!    `astrcode-core` 里根本不存在 `temperature` 字段），这一层不可达。
//! 2. **动态引导改走请求级注入。** 上游把引导作为 durable 的隐藏消息塞进历史；AstrCode
//!    没有隐藏消息通道，这里在 `before_provider_request` 里 `AppendMessages`，每个 LLM
//!    请求重算一次（同一 turn 内逐字节稳定，追加在消息列表末尾，不破坏前缀缓存）。
//! 3. **持久化改为按会话 JSON 文件**，不用上游的 SQLite 全局库。
//! 4. **END/空 → START 的复位合并到 `TurnStart`**，不再分两处处理。
//! 5. **不做陈旧会话清理**：磁盘扩展没有廉价的会话枚举通道。
//!
//! # 提示词工件的出处
//!
//! 注入给模型的正文有两处，都是逐字移植的调优工件，改动会改变模型行为：
//!
//! - [`framework_rules::FRAMEWORK_RULES`]：上游 `SYSTEM.md` 全文（静态框架规则）；
//! - [`prompts::corpus`]：上游 27 个领域提示词模块（`src/prompts/**/*.ts`）。
//!
//! 两者的逐字一致性由 `tests/prompts.rs` 对照 `tests/golden/` 强制。
//! 工作区根 `AGENTS.md` 记录了它们的源码位置与注入时机。

pub mod clock;
pub mod command;
pub mod framework_rules;
pub mod hook;
pub mod machine;
pub mod prompts;
pub mod state;
pub mod templates;
pub mod tools;
pub mod types;
pub mod worker;

pub use worker::run;
