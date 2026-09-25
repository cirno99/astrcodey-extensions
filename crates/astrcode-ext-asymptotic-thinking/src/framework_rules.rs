//! 静态框架规则：上游 `SYSTEM.md` 全文。
//!
//! 这份正文由 `prompt_build` 钩子作为 `PromptContributions::system_prompts` 注入，
//! 宿主把它映射成 `ExtensionSection::PlatformInstructions`，落在 system prompt 的
//! **静态前缀区**（`astrcode-context/src/prompt_engine.rs`），因此只在贡献变化时让
//! provider 前缀缓存失效。
//!
//! 与上游的三处改动（都由 `tools/port-prompts.mjs` 的 `ADAPTATIONS` 声明，并写进
//! `tests/golden/framework_rules.md`）：
//!
//! 1. 首行注释的注入通道：`resources_discover` → `prompt_build` 钩子；
//! 2. 状态机图的自动转换点：`before_agent_start` → `TurnStart` 生命周期钩子；
//! 3. 同上一行的框线对齐（顺带把 `┘` 对到 `│` 下方）。
//!
//! 正文其余部分逐字保留：它是被调优过的提示词工件，翻译或改写都会改变模型行为。

/// 静态框架规则正文，来自同目录的 `framework_rules.md`。
///
/// `tests/prompts.rs` 会拿它与 `tests/golden/framework_rules.md` 逐字节比对，
/// 防止无意漂移。
pub const FRAMEWORK_RULES: &str = include_str!("framework_rules.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rules_keep_their_anchors() {
        assert!(FRAMEWORK_RULES.starts_with("# 渐近式思考框架 · 静态规则\n"));
        assert!(FRAMEWORK_RULES.contains("## 状态机"));
        assert!(FRAMEWORK_RULES.contains("## 渐近式思考状态机操作规范"));
        assert!(FRAMEWORK_RULES.trim_end().ends_with("这是你的底层行为准则。"));
    }

    /// 注入通道已经换成本插件的钩子，不能还写着 pi 的 `resources_discover`。
    #[test]
    fn the_rules_name_the_astrcode_injection_channel() {
        assert!(FRAMEWORK_RULES.contains("通过 prompt_build 钩子注入系统提示词"));
        assert!(!FRAMEWORK_RULES.contains("resources_discover"));
        assert!(!FRAMEWORK_RULES.contains("before_agent_start"));
    }
}
