//! DeepSeek 模型判定。
//!
//! 判定只看模型 id，不看 `ModelSelection::provider_kind`：宿主组装 hook 上下文时
//! 走的是 `ModelSelection::simple(model_id)`（`astrcode-session::turn_context` 与
//! `session_prompt`），`profile_name` 与 `provider_kind` 恒为空串。用 provider_kind
//! 判定会让插件永远不触发。

/// 判定模型 id 是否属于 DeepSeek 家族。
///
/// 按非字母数字边界切词后逐词匹配前缀，因此 `deepseek-chat`、`deepseek-v4-flash`、
/// `deepseek/deepseek-r1`、`accounts/x/models/deepseek-v3` 都命中，而 `notdeepseek`
/// 这类只是把词嵌在中间的 id 不命中——避免把规范注入到无关模型。
pub fn is_deepseek(model_id: &str) -> bool {
    model_id
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token.to_ascii_lowercase().starts_with("deepseek"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_deepseek_model_ids_match() {
        for model in [
            "deepseek-chat",
            "deepseek-reasoner",
            "deepseek-v4-flash",
            "deepseek-coder",
        ] {
            assert!(is_deepseek(model), "{model} 应当命中");
        }
    }

    #[test]
    fn case_and_separators_do_not_matter() {
        for model in [
            "DeepSeek-Chat",
            "DEEPSEEK_V4_FLASH",
            "deepseek/v3",
            "deepseek.v3",
            "deepseek v3",
        ] {
            assert!(is_deepseek(model), "{model} 应当命中");
        }
    }

    #[test]
    fn nested_provider_paths_match_on_the_model_segment() {
        for model in [
            "accounts/fireworks/models/deepseek-v3",
            "openrouter/deepseek/deepseek-r1",
            "my-deepseek",
            // 无分隔符的写法靠词首匹配覆盖。
            "deepseekv3",
        ] {
            assert!(is_deepseek(model), "{model} 应当命中");
        }
    }

    #[test]
    fn other_models_do_not_match() {
        for model in [
            "gpt-4.1",
            "claude-sonnet-4-5",
            "qwen3-coder",
            "glm-4.6",
            "gemini-2.5-pro",
            "",
            "deep",
        ] {
            assert!(!is_deepseek(model), "{model} 不应命中");
        }
    }

    /// 只有词首才是判定位置：deepseek 夹在词中间不算 DeepSeek 模型。
    #[test]
    fn an_embedded_token_does_not_match() {
        for model in ["notdeepseek-chat", "xdeepseek", "mydeepseek"] {
            assert!(!is_deepseek(model), "{model} 不应命中");
        }
    }

    /// 判定是词首前缀而非全词相等，因此 deepseek 开头的自建模型 id 也会命中。
    /// 这是有意的：它们仍然跑在 DeepSeek 权重上。
    #[test]
    fn a_deepseek_prefixed_token_matches() {
        for model in ["deepseekish", "deepseekv3", "deepseek-r1-0528"] {
            assert!(is_deepseek(model), "{model} 应当命中");
        }
    }
}
