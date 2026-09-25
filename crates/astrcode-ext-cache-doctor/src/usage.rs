//! 会话用量扫描：把 `token_usage_recorded` 事件折叠成缓存用量合计。
//!
//! # 为什么必须读原始事件
//!
//! 宿主的省事接口 `astrcode.session.history.token_usage` 只返回
//! `non-cached input + output` 与上下文窗口，**不含缓存字段**，反推不出命中率。
//! 完整的 `LlmTokenUsage`（含 `cached_input_tokens` / `cache_creation_input_tokens`
//! / `input_accounting`）只出现在 durable 事件 `TokenUsageRecorded` 的载荷里。
//!
//! # 与 `astrcode-ext-cache-usage` 的关系
//!
//! 本模块的事件形状解析与该插件同源。真正会随 provider 语义漂移的部分——命中率的
//! 归一化口径——已经在 `astrcode-ext-common::stats` 里共享；这里重复的只是「事件
//! 载荷 → `UsageSample`」这一层薄映射，目的是让每个扩展都能独立安装、独立演进。

use astrcode_ext_common::stats::{InputAccounting, UsageSample, UsageTotals};
use astrcode_extension_worker::worker_prelude::*;
use serde_json::Value;

/// 宿主对 `astrcode.session.read_events` 的单页上限
/// （`astrcode_extensions::host_router::session::MAX_READ_EVENTS_LIMIT`）。
const PAGE_LIMIT: usize = 500;

/// 一次扫描的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageScan {
    pub totals: UsageTotals,
    /// 已检查的 durable 事件条数。
    pub events_seen: u64,
}

/// 扫描当前会话的全部 durable 事件并汇总缓存用量。
pub async fn scan_session(session_id: &str) -> Result<UsageScan, ErrorPayload> {
    let mut totals = UsageTotals::default();
    let mut events_seen = 0u64;
    let mut cursor: Option<String> = None;

    loop {
        let page = HostClient::session_history()
            .events_page(HostSessionEventsPageRequest {
                session_id: session_id.to_owned(),
                cursor: cursor.clone(),
                limit: PAGE_LIMIT,
            })
            .await?;

        for event in &page.events {
            events_seen += 1;
            if let Some(sample) = sample_from_payload(&event.payload) {
                totals.record(&sample);
            }
        }

        // 宿主声明还有后续页却返回空页时停止，避免无进展的死循环。
        if !page.has_more || page.events.is_empty() {
            break;
        }
        cursor = Some(page.next_cursor.clone());
    }

    Ok(UsageScan {
        totals,
        events_seen,
    })
}

/// 从 durable 事件载荷中提取用量样本；非 `token_usage_recorded` 事件返回 `None`。
///
/// 载荷形状来自 `astrcode_core::event::DurableEventPayload`
/// （`#[serde(tag = "type", rename_all = "snake_case")]`）：
///
/// ```json
/// {
///   "type": "token_usage_recorded",
///   "usage": { "input_tokens": 100, "cached_input_tokens": 80, "input_accounting": "inclusive" },
///   "model_context_window": 1000000
/// }
/// ```
pub fn sample_from_payload(payload: &Value) -> Option<UsageSample> {
    let event = payload.as_object()?;
    if event.get("type").and_then(Value::as_str)? != "token_usage_recorded" {
        return None;
    }
    let usage = event.get("usage").and_then(Value::as_object)?;

    Some(UsageSample {
        input_tokens: u64_field(usage, "input_tokens"),
        cached_input_tokens: u64_field(usage, "cached_input_tokens"),
        cache_creation_input_tokens: u64_field(usage, "cache_creation_input_tokens"),
        total_tokens: u64_field(usage, "total_tokens"),
        output_tokens: u64_field(usage, "output_tokens"),
        reasoning_output_tokens: u64_field(usage, "reasoning_output_tokens"),
        input_accounting: usage
            .get("input_accounting")
            .and_then(Value::as_str)
            .and_then(parse_input_accounting),
        model_context_window: u64_field(event, "model_context_window"),
    })
}

fn parse_input_accounting(raw: &str) -> Option<InputAccounting> {
    match raw {
        "inclusive" => Some(InputAccounting::Inclusive),
        "components" => Some(InputAccounting::Components),
        // 未来新增语义按「未声明」处理：归一化会退回到缓存写入存在性判定，
        // 与宿主 `LlmTokenUsage::non_cached_tokens` 的兜底路径一致。
        _ => None,
    }
}

fn u64_field(map: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    map.get(key).and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_usage_events_are_ignored() {
        assert!(sample_from_payload(&json!({ "type": "turn_completed" })).is_none());
        assert!(sample_from_payload(&json!({ "type": "user_message", "text": "hi" })).is_none());
        assert!(sample_from_payload(&json!("not an object")).is_none());
    }

    #[test]
    fn a_usage_event_without_a_usage_object_is_ignored() {
        assert!(sample_from_payload(&json!({ "type": "token_usage_recorded" })).is_none());
    }

    #[test]
    fn reads_the_full_usage_shape() {
        let payload = json!({
            "type": "token_usage_recorded",
            "usage": {
                "input_tokens": 100,
                "cached_input_tokens": 80,
                "cache_creation_input_tokens": 5,
                "output_tokens": 20,
                "reasoning_output_tokens": 7,
                "total_tokens": 125,
                "input_accounting": "inclusive"
            },
            "model_context_window": 1_000_000
        });

        let sample = sample_from_payload(&payload).expect("应识别为用量事件");
        assert_eq!(sample.input_tokens, Some(100));
        assert_eq!(sample.cached_input_tokens, Some(80));
        assert_eq!(sample.cache_creation_input_tokens, Some(5));
        assert_eq!(sample.output_tokens, Some(20));
        assert_eq!(sample.reasoning_output_tokens, Some(7));
        assert_eq!(sample.input_accounting, Some(InputAccounting::Inclusive));
        assert_eq!(sample.model_context_window, Some(1_000_000));

        let normalized = sample.normalized();
        assert_eq!(normalized.full, 100);
        assert_eq!(normalized.cached, 80);
    }

    #[test]
    fn reads_the_components_shape() {
        let payload = json!({
            "type": "token_usage_recorded",
            "usage": {
                "input_tokens": 20,
                "cached_input_tokens": 70,
                "cache_creation_input_tokens": 10,
                "input_accounting": "components"
            },
            "model_context_window": 200_000
        });

        let normalized = sample_from_payload(&payload).unwrap().normalized();
        assert_eq!(normalized.full, 100);
        assert_eq!(normalized.cached, 70);
    }

    #[test]
    fn unknown_accounting_falls_back_to_undeclared() {
        assert_eq!(parse_input_accounting("future_style"), None);
        assert_eq!(
            parse_input_accounting("components"),
            Some(InputAccounting::Components)
        );
    }

    #[test]
    fn sparse_usage_only_reports_present_fields() {
        let payload = json!({
            "type": "token_usage_recorded",
            "usage": { "input_tokens": 42 },
            "model_context_window": 1000
        });
        let sample = sample_from_payload(&payload).unwrap();
        assert_eq!(sample.input_tokens, Some(42));
        assert_eq!(sample.cached_input_tokens, None);
        assert_eq!(sample.output_tokens, None);
    }

    #[test]
    fn negative_or_fractional_counts_are_rejected() {
        let payload = json!({
            "type": "token_usage_recorded",
            "usage": { "input_tokens": -5, "cached_input_tokens": 1.5 },
            "model_context_window": 10
        });
        let sample = sample_from_payload(&payload).unwrap();
        assert_eq!(sample.input_tokens, None);
        assert_eq!(sample.cached_input_tokens, None);
    }
}
