//! 缓存命中率统计：把 provider 的 token 计数归一化成可累加的用量合计。
//!
//! # 为什么需要归一化
//!
//! 不同 provider 的 input 计数语义不同：
//! - OpenAI 风格（`Inclusive`）：`input_tokens` **已包含**缓存读取，缓存计数只是描述性子集。
//! - Anthropic 风格（`Components`）：常规输入、缓存读取、缓存写入是三个独立组成部分。
//!
//! 直接相加会算错分母，因此这里在**单个样本**上归一化，再累加。
//! 归一化规则与宿主 `astrcode_core::llm::LlmTokenUsage::non_cached_tokens`
//! 保持一致，确保插件的「未命中」口径与宿主自身的 token 预算统计逐位对齐。

use serde::{Deserialize, Serialize};

/// provider 的 input 计数语义，镜像 `astrcode_core::llm::LlmInputTokenAccounting`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAccounting {
    /// `input_tokens` 已包含缓存读取；缓存计数是描述性子集。
    Inclusive,
    /// 常规输入、缓存读取、缓存写入是彼此独立的组成部分。
    Components,
}

/// 单次模型请求的 token 计数事实。
///
/// 字段全部可缺省：不同 provider 与不同 fallback 路径上报的计数器并不一致。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageSample {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub input_accounting: Option<InputAccounting>,
    pub model_context_window: Option<u64>,
}

/// 归一化后的 prompt 计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizedPrompt {
    /// 完整 prompt token 数（含缓存读取与缓存写入）。
    pub full: u64,
    /// 其中命中缓存的 token 数。
    pub cached: u64,
}

impl NormalizedPrompt {
    /// 未命中缓存的 prompt token 数。
    pub fn uncached(&self) -> u64 {
        self.full.saturating_sub(self.cached)
    }
}

impl UsageSample {
    /// 该样本是否带有任何可用计数。
    pub fn has_usage(&self) -> bool {
        self.input_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.cache_creation_input_tokens.is_some()
            || self.total_tokens.is_some()
            || self.output_tokens.is_some()
    }

    /// 是否按「组成部分」语义解释 input 计数。
    ///
    /// 与宿主一致：显式声明 `Components`，或在未声明语义时出现了缓存写入计数
    /// （历史数据里只有组成部分语义的 provider 才会写入该计数器）。
    fn component_accounting(&self) -> bool {
        matches!(self.input_accounting, Some(InputAccounting::Components))
            || (self.input_accounting.is_none() && self.cache_creation_input_tokens.is_some())
    }

    /// 归一化成完整 prompt 与其中命中缓存的 token 数。
    pub fn normalized(&self) -> NormalizedPrompt {
        let cached = self.cached_input_tokens.unwrap_or(0);
        if self.component_accounting() {
            let uncached = self
                .input_tokens
                .unwrap_or(0)
                .saturating_add(self.cache_creation_input_tokens.unwrap_or(0));
            return NormalizedPrompt {
                full: uncached.saturating_add(cached),
                cached,
            };
        }

        // Inclusive：input 已包含缓存读取。缺少 input 时用 total - output 反推 prompt，
        // 因为 total 含生成量，不能直接当作 prompt。
        let full = match self.input_tokens {
            Some(input) => input,
            None => self
                .total_tokens
                .map(|total| total.saturating_sub(self.output_tokens.unwrap_or(0)))
                .unwrap_or(0),
        };
        NormalizedPrompt {
            full,
            cached: cached.min(full),
        }
    }
}

/// 一次会话内累计的用量合计。
///
/// `prompt_tokens` 是归一化后的完整 prompt；`cached_tokens` 是其中命中缓存的部分。
/// 命中率 = `cached_tokens / prompt_tokens`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageTotals {
    /// 上报过用量的模型请求数。
    pub requests: u64,
    /// 归一化后的完整 prompt token 数。
    pub prompt_tokens: u64,
    /// 其中命中缓存读取的 token 数。
    pub cached_tokens: u64,
    /// 写入缓存的 token 数（仅组成部分语义的 provider 会非零）。
    pub cache_creation_tokens: u64,
    /// 生成 token 数。
    pub output_tokens: u64,
    /// 其中推理 token 数。
    pub reasoning_output_tokens: u64,
    /// provider 上报的上下文窗口大小；多次上报取最大值。
    pub context_window: u64,
}

impl UsageTotals {
    /// 累加一个样本。不带任何计数的样本被忽略，不计入 `requests`。
    pub fn record(&mut self, sample: &UsageSample) {
        if !sample.has_usage() {
            return;
        }
        let prompt = sample.normalized();
        self.requests = self.requests.saturating_add(1);
        self.prompt_tokens = self.prompt_tokens.saturating_add(prompt.full);
        self.cached_tokens = self.cached_tokens.saturating_add(prompt.cached);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(sample.cache_creation_input_tokens.unwrap_or(0));
        self.output_tokens = self
            .output_tokens
            .saturating_add(sample.output_tokens.unwrap_or(0));
        self.reasoning_output_tokens = self
            .reasoning_output_tokens
            .saturating_add(sample.reasoning_output_tokens.unwrap_or(0));
        if let Some(window) = sample.model_context_window {
            self.context_window = self.context_window.max(window);
        }
    }

    /// 未命中缓存的 prompt token 数。
    pub fn uncached_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_sub(self.cached_tokens)
    }

    /// 缓存命中率；没有 prompt 计数时返回 `None`（而不是 0，避免误导）。
    pub fn hit_rate(&self) -> Option<f64> {
        (self.prompt_tokens > 0).then(|| self.cached_tokens as f64 / self.prompt_tokens as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(input: Option<u64>, cached: Option<u64>, creation: Option<u64>) -> UsageSample {
        UsageSample {
            input_tokens: input,
            cached_input_tokens: cached,
            cache_creation_input_tokens: creation,
            ..UsageSample::default()
        }
    }

    #[test]
    fn inclusive_input_already_contains_cached() {
        let got = sample(Some(100), Some(80), None).normalized();
        assert_eq!(got.full, 100);
        assert_eq!(got.cached, 80);
        assert_eq!(got.uncached(), 20);
    }

    #[test]
    fn components_are_summed_into_the_prompt() {
        let got = sample(Some(20), Some(70), Some(10)).normalized();
        assert_eq!(got.full, 100);
        assert_eq!(got.cached, 70);
        assert_eq!(got.uncached(), 30);
    }

    #[test]
    fn missing_accounting_with_cache_creation_is_components() {
        let got = sample(Some(20), Some(70), Some(10)).normalized();
        assert_eq!(got.full, 100);
    }

    #[test]
    fn explicit_inclusive_ignores_cache_creation_in_the_prompt() {
        // 宿主语义：显式 Inclusive 时缓存写入是描述性字段，不进入 prompt 分母。
        let got = UsageSample {
            input_accounting: Some(InputAccounting::Inclusive),
            ..sample(Some(100), Some(80), Some(5))
        }
        .normalized();
        assert_eq!(got.full, 100);
        assert_eq!(got.cached, 80);
    }

    #[test]
    fn total_minus_output_recovers_the_prompt() {
        let got = UsageSample {
            total_tokens: Some(120),
            output_tokens: Some(20),
            ..sample(None, Some(50), None)
        }
        .normalized();
        assert_eq!(got.full, 100);
        assert_eq!(got.cached, 50);
    }

    #[test]
    fn cached_is_clamped_to_the_prompt() {
        let got = sample(Some(10), Some(99), None).normalized();
        assert_eq!(got.full, 10);
        assert_eq!(got.cached, 10);
    }

    #[test]
    fn record_accumulates_across_accounting_styles() {
        let mut totals = UsageTotals::default();
        totals.record(&sample(Some(100), Some(80), None));
        totals.record(&sample(Some(20), Some(70), Some(10)));
        assert_eq!(totals.requests, 2);
        assert_eq!(totals.prompt_tokens, 200);
        assert_eq!(totals.cached_tokens, 150);
        assert_eq!(totals.uncached_tokens(), 50);
        assert_eq!(totals.hit_rate(), Some(0.75));
    }

    #[test]
    fn samples_without_usage_are_not_counted() {
        let mut totals = UsageTotals::default();
        totals.record(&UsageSample::default());
        assert_eq!(totals.requests, 0);
        assert_eq!(totals.prompt_tokens, 0);
    }

    #[test]
    fn hit_rate_is_none_without_prompt_tokens() {
        let mut totals = UsageTotals::default();
        totals.record(&UsageSample {
            output_tokens: Some(500),
            ..UsageSample::default()
        });
        assert_eq!(totals.requests, 1);
        assert_eq!(totals.hit_rate(), None);
    }

    #[test]
    fn context_window_keeps_the_largest_report() {
        let mut totals = UsageTotals::default();
        totals.record(&UsageSample {
            model_context_window: Some(1000),
            input_tokens: Some(1),
            ..UsageSample::default()
        });
        totals.record(&UsageSample {
            model_context_window: Some(4000),
            input_tokens: Some(1),
            ..UsageSample::default()
        });
        assert_eq!(totals.context_window, 4000);
    }

    #[test]
    fn reasoning_tokens_are_tracked_separately_from_output() {
        let mut totals = UsageTotals::default();
        totals.record(&UsageSample {
            input_tokens: Some(10),
            output_tokens: Some(30),
            reasoning_output_tokens: Some(12),
            ..UsageSample::default()
        });
        assert_eq!(totals.output_tokens, 30);
        assert_eq!(totals.reasoning_output_tokens, 12);
    }
}
