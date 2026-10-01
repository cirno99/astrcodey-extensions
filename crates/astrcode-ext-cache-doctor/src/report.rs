//! `/cache-doctor` 的文本渲染。
//!
//! 插件只能输出纯文本：命令结果走 `ExtensionCommandResult::Display` 的 `content`，
//! 状态栏走 `StatusItemUpdatePayload` 的 `text`。没有表格、进度条或配色，因此这里
//! 全部按「一行内可读」设计。

use astrcode_ext_common::stats::UsageTotals;
use astrcode_ext_common::text::{format_optional_percent, format_percent, format_tokens};

use crate::{
    config::{Config, MAX_HISTORY},
    prefix::{MessageDigest, PrefixReport},
    usage::UsageScan,
    watch::{BreakRecord, SessionSnapshot},
};

/// 状态栏条目 id。
///
/// 磁盘 s5r 插件**无法注册**状态栏条目（S5R 的 `InitializeManifest` 没有
/// `status_items` 字段），只能在命令结果里携带 `status_update`。宿主不要求该 id
/// 预先注册：前端 `applyDelta` 与 CLI `handle_event` 都会直接按 id 写入渲染表。
pub const STATUS_ITEM_ID: &str = "cache-prefix";

/// 命令输出的统一前缀。
pub const PREFIX: &str = "cache-doctor：";

/// 状态栏一格：只放前缀保留计数，越短越好。
///
/// 还没有可比对的样本时显示 `n/a`，而不是会误导人的 `0/0`。
pub fn status_text(snapshot: Option<&SessionSnapshot>) -> String {
    match snapshot {
        Some(snapshot) if snapshot.preserved + snapshot.broken > 0 => {
            let compared = snapshot.preserved + snapshot.broken;
            format!("prefix {}/{}", snapshot.preserved, compared)
        },
        _ => "prefix n/a".to_string(),
    }
}

/// `/cache-doctor status`。
pub fn status_report(
    model_id: &str,
    config: &Config,
    snapshot: Option<&SessionSnapshot>,
    config_path: &str,
) -> String {
    let mut lines = vec![format!(
        "{PREFIX}模型 {model_id} · 观测{}（保留最近 {} 次对比）",
        if config.watch { "已开启" } else { "已关闭" },
        config.history,
    )];

    match snapshot.filter(|snapshot| !snapshot.is_empty()) {
        Some(snapshot) => {
            lines.push(format!(
                "本会话 {} 次请求 · 前缀完整 {} · 断点 {}（保留率 {}）",
                snapshot.requests,
                snapshot.preserved,
                snapshot.broken,
                format_optional_percent(snapshot.preserved_rate()),
            ));
            lines.push(match &snapshot.last_break {
                Some(last) => format!("最近断点：{}", describe_break(last)),
                None => "最近断点：无。".to_string(),
            });
        },
        None => lines.push(
            "本会话还没有观测到 provider 请求：先发一轮消息，再执行本命令。".to_string(),
        ),
    }

    lines.push(format!("配置文件：{config_path}"));
    lines.join("\n")
}

/// `/cache-doctor doctor`。
pub fn doctor_report(
    model_id: &str,
    config: &Config,
    snapshot: Option<&SessionSnapshot>,
    usage: Option<&UsageScan>,
) -> String {
    let mut lines = vec![format!("{PREFIX}模型 {model_id}")];

    lines.push(match usage {
        Some(scan) if scan.totals.requests > 0 => headline(&scan.totals),
        Some(scan) => format!(
            "缓存用量：本会话还没有模型请求的用量记录（已扫描 {} 条 durable 事件）。",
            scan.events_seen
        ),
        None => "缓存用量：未读取（观测已关闭）。".to_string(),
    });

    lines.push(match snapshot.filter(|snapshot| !snapshot.is_empty()) {
        Some(snapshot) => format!(
            "前缀：{} 次请求中完整保留 {} 次（{}）· 断点 {} 次",
            snapshot.requests,
            snapshot.preserved,
            format_optional_percent(snapshot.preserved_rate()),
            snapshot.broken,
        ),
        None => "前缀：本会话还没有观测到 provider 请求。".to_string(),
    });

    if let Some(last) = snapshot.and_then(|snapshot| snapshot.last_break.as_ref()) {
        lines.push(format!("最近断点：{}", describe_break(last)));
    }

    let hit_rate = usage.and_then(|scan| scan.totals.hit_rate());
    lines.push(format!(
        "诊断：{}",
        diagnose(config, snapshot, hit_rate)
    ));

    lines.join("\n")
}

/// `/cache-doctor stats`。
pub fn stats_report(
    config: &Config,
    snapshot: Option<&SessionSnapshot>,
    usage: Option<&UsageScan>,
) -> String {
    let mut lines = Vec::new();

    if let Some(scan) = usage.filter(|scan| scan.totals.requests > 0) {
        lines.push(headline(&scan.totals));
    }

    let Some(snapshot) = snapshot.filter(|snapshot| !snapshot.recent.is_empty()) else {
        lines.push(format!(
            "{PREFIX}本会话还没有可比对的前缀样本（首次观测之后才有对比）。"
        ));
        return lines.join("\n");
    };

    lines.push(format!(
        "本会话最近 {} 次前缀对比（最旧 → 最新，保留上限 {}）：",
        snapshot.recent.len(),
        config.history,
    ));
    for (index, report) in snapshot.recent.iter().enumerate() {
        lines.push(format!("  {} · {}", index + 1, describe_report(report)));
    }

    lines.join("\n")
}

/// `/cache-doctor config`。
pub fn config_report(config: &Config, config_path: &str, note: Option<&str>) -> String {
    let mut lines = vec![format!("{PREFIX}配置（{config_path}）")];
    lines.push(format!(
        "  watch = {}      观测 provider 请求；`/cache-doctor disable` 关闭",
        if config.watch { "on" } else { "off" },
    ));
    lines.push(format!(
        "  history = {}  快照保留的最近对比条数（1..={MAX_HISTORY}）",
        config.history,
    ));
    if let Some(note) = note {
        lines.push(note.to_string());
    }
    lines.join("\n")
}

/// `/cache-doctor help`。
pub fn help_report() -> String {
    [
        format!("{PREFIX}观测 provider 请求的 prompt 前缀，定位缓存断点。本插件只读不改写。"),
        "  /cache-doctor status              当前模型、观测开关与本会话的前缀保留情况".to_string(),
        "  /cache-doctor doctor              命中率 + 前缀断点 + 诊断建议".to_string(),
        "  /cache-doctor stats               最近若干次请求的前缀对比明细".to_string(),
        "  /cache-doctor reset               清空本会话的观测计数（保留上一次请求基线）".to_string(),
        "  /cache-doctor enable | disable    开启 / 关闭观测（持久化，重载后仍生效）".to_string(),
        "  /cache-doctor config              查看配置".to_string(),
        "  /cache-doctor config watch on|off 设置观测开关".to_string(),
        "  /cache-doctor config history <n>  设置快照保留的对比条数（1..=200）".to_string(),
        "  /cache-doctor help                本说明".to_string(),
    ]
    .join("\n")
}

/// 一行用量汇总。口径与 `/usage` 一致（共享 `astrcode-ext-common::stats` 的归一化）。
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

fn describe_report(report: &PrefixReport) -> String {
    let common = format!(
        "上次 {} 条 → 本次 {} 条 · 新增 {} 条",
        report.previous_len,
        report.current_len,
        report.added(),
    );
    match report.break_index() {
        None => format!("{common} · 前缀完整"),
        Some(index) => format!(
            "{common} · 断点 #{index} · 保留 {}",
            format_percent(report.preserved_ratio()),
        ),
    }
}

fn describe_break(last: &BreakRecord) -> String {
    let mut text = format!("第 {} 次请求，位置 #{}", last.at_request, last.index);
    text.push_str(&format!(
        "（{} → {}）",
        describe_digest(last.previous.as_ref()),
        describe_digest(last.current.as_ref()),
    ));
    if last.truncated {
        text.push_str("；本次请求比上一次短，历史被裁剪");
    }
    text
}

fn describe_digest(digest: Option<&MessageDigest>) -> String {
    match digest {
        Some(digest) => format!(
            "{} · {} 字符",
            digest.label.as_str(),
            format_tokens(digest.bytes as u64)
        ),
        None => "不存在".to_string(),
    }
}

/// 诊断结论。
///
/// 只根据**本插件真正观测到的事实**下结论，并明确区分「已证实」与「可能来源」。
fn diagnose(config: &Config, snapshot: Option<&SessionSnapshot>, hit_rate: Option<f64>) -> String {
    if !config.watch {
        return "观测已关闭（`/cache-doctor enable` 重新开启），无法判断前缀是否稳定。".to_string();
    }

    let Some(snapshot) = snapshot.filter(|snapshot| !snapshot.is_empty()) else {
        return "还没有观测到 provider 请求：先发一轮消息再看。".to_string();
    };

    if snapshot.broken > 0 {
        let index = snapshot
            .last_break
            .as_ref()
            .map(|last| last.index.to_string())
            .unwrap_or_else(|| "?".to_string());
        return format!(
            "检测到 {} 次历史被追溯改写（最近一次在第 {} 条消息）。provider 的前缀缓存从该位置起全部失效，\
             其后的 prompt 每次都要重算。常见来源：上下文压缩、扩展改写历史消息、deferred tools 列表变化。\
             用 `/cache-doctor stats` 看每次请求的对比。",
            snapshot.broken, index,
        );
    }

    match hit_rate {
        None => "前缀完整，但 provider 没有上报缓存计数（`token_usage_recorded` 缺 `cached_input_tokens`），\
                 无法判断命中率。"
            .to_string(),
        Some(rate) if rate >= 0.9 => format!(
            "前缀完整、命中率 {}，无需处理。",
            format_percent(rate)
        ),
        Some(rate) => format!(
            "前缀完整，但命中率只有 {}。失效不在历史改写这一侧，可能的原因：provider 或网关未启用 prompt 缓存；\
             或者缓存分片每次都在变——宿主按 model + system + tools 派生 `prompt_cache_key`，工具集变化会改变它。",
            format_percent(rate),
        ),
    }
}

#[cfg(test)]
mod tests {
    use astrcode_ext_common::stats::{UsageSample, UsageTotals};

    use super::*;
    use crate::{
        prefix::{Label, MessageDigest},
        usage::UsageScan,
    };

    fn digest(label: Label, bytes: usize) -> MessageDigest {
        MessageDigest {
            label,
            bytes,
            fingerprint: 0,
        }
    }

    fn snapshot(requests: u64, preserved: u64, broken: u64) -> SessionSnapshot {
        SessionSnapshot {
            requests,
            preserved,
            broken,
            last_break: None,
            recent: Vec::new(),
        }
    }

    fn scan(samples: &[UsageSample]) -> UsageScan {
        let mut totals = UsageTotals::default();
        for sample in samples {
            totals.record(sample);
        }
        UsageScan {
            totals,
            events_seen: 3,
        }
    }

    #[test]
    fn status_without_observations_says_so() {
        let text = status_report("m", &Config::default(), None, "/tmp/config.json");
        assert!(text.contains("还没有观测到 provider 请求"), "{text}");
        assert!(text.contains("/tmp/config.json"));
        assert!(text.contains("观测已开启"));
    }

    #[test]
    fn status_reports_counts_and_the_last_break() {
        let mut snapshot = snapshot(12, 11, 1);
        snapshot.last_break = Some(BreakRecord {
            index: 7,
            previous: Some(digest(Label::AssistantToolCalls, 3_100)),
            current: Some(digest(Label::Tool, 8_200)),
            truncated: false,
            at_request: 3,
        });

        let text = status_report("m", &Config::default(), Some(&snapshot), "/tmp/config.json");
        assert!(text.contains("本会话 12 次请求 · 前缀完整 11 · 断点 1（保留率 91.7%）"), "{text}");
        assert!(
            text.contains("位置 #7（assistant(tool_calls) · 3.1K 字符 → tool · 8.2K 字符）"),
            "{text}"
        );
    }

    #[test]
    fn a_truncating_break_is_called_out() {
        let mut snapshot = snapshot(2, 0, 1);
        snapshot.last_break = Some(BreakRecord {
            index: 1,
            previous: Some(digest(Label::Tool, 100)),
            current: None,
            truncated: true,
            at_request: 2,
        });
        let text = status_report("m", &Config::default(), Some(&snapshot), "/tmp/c.json");
        assert!(text.contains("（tool · 100 字符 → 不存在）"), "{text}");
        assert!(text.contains("历史被裁剪"), "{text}");
    }

    #[test]
    fn stats_without_samples_explains_why() {
        let text = stats_report(&Config::default(), Some(&snapshot(1, 0, 0)), None);
        assert!(text.contains("还没有可比对的前缀样本"), "{text}");
    }

    #[test]
    fn stats_numbers_the_recent_comparisons() {
        let mut snapshot = snapshot(3, 1, 1);
        snapshot.recent = vec![
            PrefixReport {
                previous_len: 12,
                current_len: 14,
                common: 12,
            },
            PrefixReport {
                previous_len: 14,
                current_len: 15,
                common: 9,
            },
        ];

        let text = stats_report(&Config::default(), Some(&snapshot), None);
        assert!(text.contains("最近 2 次前缀对比"), "{text}");
        assert!(text.contains("1 · 上次 12 条 → 本次 14 条 · 新增 2 条 · 前缀完整"), "{text}");
        assert!(
            text.contains("2 · 上次 14 条 → 本次 15 条 · 新增 1 条 · 断点 #9 · 保留 64.3%"),
            "{text}"
        );
    }

    #[test]
    fn doctor_without_usage_does_not_claim_a_hit_rate() {
        let text = doctor_report(
            "m",
            &Config::default(),
            Some(&snapshot(2, 2, 0)),
            Some(&scan(&[])),
        );
        assert!(text.contains("还没有模型请求的用量记录"), "{text}");
        assert!(text.contains("无法判断命中率"), "{text}");
    }

    #[test]
    fn doctor_flags_a_broken_prefix_over_the_hit_rate() {
        let mut snapshot = snapshot(5, 3, 2);
        snapshot.last_break = Some(BreakRecord {
            index: 4,
            previous: Some(digest(Label::Tool, 10)),
            current: Some(digest(Label::Tool, 20)),
            truncated: false,
            at_request: 5,
        });
        let text = doctor_report(
            "m",
            &Config::default(),
            Some(&snapshot),
            Some(&scan(&[UsageSample {
                input_tokens: Some(100),
                cached_input_tokens: Some(99),
                ..UsageSample::default()
            }])),
        );
        assert!(text.contains("缓存命中 99.0%"), "{text}");
        assert!(text.contains("检测到 2 次历史被追溯改写"), "{text}");
        assert!(text.contains("最近一次在第 4 条消息"), "{text}");
    }

    #[test]
    fn doctor_is_positive_when_everything_holds() {
        let text = doctor_report(
            "m",
            &Config::default(),
            Some(&snapshot(9, 8, 0)),
            Some(&scan(&[UsageSample {
                input_tokens: Some(1_000),
                cached_input_tokens: Some(950),
                ..UsageSample::default()
            }])),
        );
        assert!(text.contains("前缀完整、命中率 95.0%，无需处理。"), "{text}");
    }

    #[test]
    fn doctor_separates_a_low_hit_rate_from_prefix_stability() {
        let text = doctor_report(
            "m",
            &Config::default(),
            Some(&snapshot(9, 8, 0)),
            Some(&scan(&[UsageSample {
                input_tokens: Some(1_000),
                cached_input_tokens: Some(100),
                ..UsageSample::default()
            }])),
        );
        assert!(text.contains("前缀完整，但命中率只有 10.0%"), "{text}");
        assert!(text.contains("工具集变化会改变它"), "{text}");
    }

    #[test]
    fn doctor_respects_a_disabled_watch() {
        let text = doctor_report(
            "m",
            &Config {
                watch: false,
                ..Config::default()
            },
            Some(&snapshot(9, 8, 0)),
            None,
        );
        assert!(text.contains("观测已关闭"), "{text}");
        assert!(text.contains("缓存用量：未读取"), "{text}");
    }

    #[test]
    fn status_text_stays_short_and_reports_missing_data() {
        assert_eq!(status_text(None), "prefix n/a");
        assert_eq!(
            status_text(Some(&snapshot(1, 0, 0))),
            "prefix n/a",
            "只有一次请求时还没有可比对的样本"
        );

        assert_eq!(status_text(Some(&snapshot(12, 11, 1))), "prefix 11/12");
        assert!(status_text(Some(&snapshot(12, 11, 1))).len() <= 16);
    }

    #[test]
    fn config_report_lists_the_editable_fields() {
        let text = config_report(
            &Config {
                watch: false,
                history: 7,
                ..Config::default()
            },
            "/tmp/config.json",
            Some("已写入。"),
        );
        assert!(text.contains("watch = off"), "{text}");
        assert!(text.contains("history = 7"), "{text}");
        assert!(text.contains("已写入。"), "{text}");
    }

    #[test]
    fn help_lists_every_subcommand() {
        let text = help_report();
        for subcommand in [
            "status", "doctor", "stats", "reset", "enable", "disable", "config", "help",
        ] {
            assert!(text.contains(subcommand), "帮助缺少 {subcommand}：{text}");
        }
    }
}
