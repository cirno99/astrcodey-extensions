//! `/usage` 的文本渲染与状态栏文案。
//!
//! 插件只能输出纯文本：命令结果走 `ExtensionCommandResult::Display` 的 `content`，
//! 状态栏走 `StatusItemUpdatePayload` 的 `text`。没有表格、进度条或配色，
//! 因此这里全部按「一行内可读」设计。

use astrcode_ext_common::stats::UsageTotals;
use astrcode_ext_common::text::{format_optional_percent, format_tokens};

use crate::scan::ScanOutcome;

/// 状态栏条目 id。
///
/// 磁盘 s5r 插件**无法注册**状态栏条目（S5R 的 `InitializeManifest` 没有
/// `status_items` 字段），只能在命令结果里携带 `status_update`。宿主不要求该 id
/// 预先注册：前端 `applyDelta` 与 CLI `handle_event` 都会直接按 id 写入渲染表。
pub const STATUS_ITEM_ID: &str = "cache-hit";

/// 状态栏一格：只放命中率，越短越好。
pub fn status_text(outcome: &ScanOutcome) -> String {
    format!(
        "cache {}",
        format_optional_percent(outcome.totals.hit_rate())
    )
}

/// 命令输出：一行汇总，必要时追加一行明细。
pub fn render(outcome: &ScanOutcome) -> String {
    let totals = &outcome.totals;

    if totals.requests == 0 {
        return format!(
            "缓存用量：本会话还没有模型请求的用量记录（已扫描 {} 条 durable 事件）。",
            outcome.events_seen
        );
    }

    let mut lines = vec![headline(totals)];

    let mut detail = Vec::new();
    if totals.cache_creation_tokens > 0 {
        detail.push(format!(
            "缓存写入 {}",
            format_tokens(totals.cache_creation_tokens)
        ));
    }
    if totals.reasoning_output_tokens > 0 {
        detail.push(format!(
            "推理 {}",
            format_tokens(totals.reasoning_output_tokens)
        ));
    }
    if totals.context_window > 0 {
        detail.push(format!(
            "上下文窗口 {}",
            format_tokens(totals.context_window)
        ));
    }
    detail.push(format!("状态栏已更新为 {}", status_text(outcome)));
    lines.push(format!("明细：{}", detail.join(" · ")));

    lines.join("\n")
}

fn headline(totals: &UsageTotals) -> String {
    format!(
        "缓存命中 {} · 输入 {}（命中 {} / 未命中 {}） · 输出 {} · {} 次请求",
        format_optional_percent(totals.hit_rate()),
        format_tokens(totals.prompt_tokens),
        format_tokens(totals.cached_tokens),
        format_tokens(totals.uncached_tokens()),
        format_tokens(totals.output_tokens),
        totals.requests,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use astrcode_ext_common::stats::UsageSample;

    fn outcome(samples: &[UsageSample], events_seen: u64) -> ScanOutcome {
        let mut totals = UsageTotals::default();
        for sample in samples {
            totals.record(sample);
        }
        ScanOutcome {
            totals,
            events_seen,
        }
    }

    #[test]
    fn empty_session_reports_that_there_is_no_usage() {
        let text = render(&outcome(&[], 12));
        assert!(text.contains("还没有模型请求的用量记录"));
        assert!(text.contains("12 条 durable 事件"));
        assert_eq!(status_text(&outcome(&[], 12)), "cache n/a");
    }

    #[test]
    fn headline_reports_rate_totals_and_request_count() {
        let text = render(&outcome(
            &[UsageSample {
                input_tokens: Some(1_000),
                cached_input_tokens: Some(800),
                output_tokens: Some(200),
                ..UsageSample::default()
            }],
            5,
        ));
        let headline = text.lines().next().unwrap();
        assert_eq!(
            headline,
            "缓存命中 80.0% · 输入 1.0K（命中 800 / 未命中 200） · 输出 200 · 1 次请求"
        );
    }

    #[test]
    fn detail_line_always_reports_the_status_bar_update() {
        let text = render(&outcome(
            &[UsageSample {
                input_tokens: Some(100),
                cached_input_tokens: Some(50),
                ..UsageSample::default()
            }],
            1,
        ));
        assert!(text.contains("状态栏已更新为 cache 50.0%"));
    }

    #[test]
    fn optional_counters_only_appear_when_non_zero() {
        let base = UsageSample {
            input_tokens: Some(100),
            cached_input_tokens: Some(50),
            ..UsageSample::default()
        };

        let plain = render(&outcome(&[base], 1));
        assert!(!plain.contains("缓存写入"));
        assert!(!plain.contains("推理"));
        assert!(!plain.contains("上下文窗口"));

        let rich = render(&outcome(
            &[UsageSample {
                cache_creation_input_tokens: Some(7),
                reasoning_output_tokens: Some(9),
                model_context_window: Some(1_000_000),
                ..base
            }],
            1,
        ));
        assert!(rich.contains("缓存写入 7"));
        assert!(rich.contains("推理 9"));
        assert!(rich.contains("上下文窗口 1.00M"));
    }

    #[test]
    fn components_accounting_keeps_the_denominator_consistent() {
        let text = render(&outcome(
            &[UsageSample {
                input_tokens: Some(20),
                cached_input_tokens: Some(70),
                cache_creation_input_tokens: Some(10),
                ..UsageSample::default()
            }],
            1,
        ));
        let headline = text.lines().next().unwrap();
        assert!(
            headline.starts_with("缓存命中 70.0% · 输入 100"),
            "{headline}"
        );
    }

    #[test]
    fn status_text_is_short_enough_for_one_cell() {
        let text = status_text(&outcome(
            &[UsageSample {
                input_tokens: Some(100),
                cached_input_tokens: Some(100),
                ..UsageSample::default()
            }],
            1,
        ));
        assert_eq!(text, "cache 100.0%");
        assert!(text.len() <= 16);
    }
}
