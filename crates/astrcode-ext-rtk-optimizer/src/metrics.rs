//! 本插件进程内的压缩节省统计。
//!
//! 上游 `output-metrics.ts` 把记录放在模块级数组里，本 crate 用同构的进程内
//! `Mutex<Vec<_>>`：S5R 扩展进程跨会话常驻，因此统计是**跨会话**的，与上游一致。
//! `/rtk clear-stats` 清空它。
//!
//! 渲染逻辑抽成纯函数 [`render_summary`]，全局容器只是薄包装——统计是进程级共享状态，
//! 而 Rust 测试默认并行跑，把断言压在纯函数上才能得到确定性的测试。

use std::sync::{LazyLock, Mutex};

use astrcode_ext_common::text::format_percent;

/// 一次压缩的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricRecord {
    pub tool: String,
    pub techniques: String,
    pub original_chars: usize,
    pub compacted_chars: usize,
}

static METRICS: LazyLock<Mutex<Vec<MetricRecord>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// 记录一次压缩。锁中毒说明此前有 panic 穿过临界区，此时静默跳过而不是再次 panic。
pub fn record(tool: &str, original_chars: usize, compacted_chars: usize, techniques: &[String]) {
    let record = MetricRecord {
        tool: tool.to_owned(),
        techniques: join_techniques(techniques),
        original_chars,
        compacted_chars,
    };
    if let Ok(mut metrics) = METRICS.lock() {
        metrics.push(record);
    }
}

/// 技术名列表的展示形式：空列表记成 `none`。
fn join_techniques(techniques: &[String]) -> String {
    if techniques.is_empty() {
        "none".to_owned()
    } else {
        techniques.join(",")
    }
}

/// 清空全部记录。
pub fn clear() {
    if let Ok(mut metrics) = METRICS.lock() {
        metrics.clear();
    }
}

/// 当前记录的快照。
fn snapshot() -> Vec<MetricRecord> {
    match METRICS.lock() {
        Ok(metrics) => metrics.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// 千位分隔，与上游 `Number.prototype.toLocaleString()` 在 en-US 下的结果一致。
fn format_chars(value: usize) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// 渲染一份记录集合的摘要。
pub fn render_summary(metrics: &[MetricRecord]) -> String {
    if metrics.is_empty() {
        return "RTK 压缩统计：还没有数据。".to_owned();
    }

    let total_original: usize = metrics.iter().map(|metric| metric.original_chars).sum();
    let total_compacted: usize = metrics.iter().map(|metric| metric.compacted_chars).sum();
    let total_saved = total_original.saturating_sub(total_compacted);

    let mut by_tool: Vec<(String, usize, usize, usize)> = Vec::new();
    for metric in metrics {
        match by_tool.iter_mut().find(|(tool, ..)| tool == &metric.tool) {
            Some((_, count, original, compacted)) => {
                *count += 1;
                *original += metric.original_chars;
                *compacted += metric.compacted_chars;
            },
            None => by_tool.push((
                metric.tool.clone(),
                1,
                metric.original_chars,
                metric.compacted_chars,
            )),
        }
    }

    let mut result = String::from("RTK 输出压缩统计\n");
    result.push_str(&format!(
        "共 {} 次，节省 {} 字符（{}）\n",
        metrics.len(),
        format_chars(total_saved),
        format_percent(percent(total_saved, total_original))
    ));

    for (tool, count, original, compacted) in by_tool {
        let saved = original.saturating_sub(compacted);
        result.push_str(&format!(
            "- {tool}: {count} 次，节省 {} 字符（{}）\n",
            format_chars(saved),
            format_percent(percent(saved, original))
        ));
    }

    result.trim_end().to_owned()
}

/// 渲染当前进程累积的统计摘要。
pub fn summary() -> String {
    render_summary(&snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(tool: &str, original: usize, compacted: usize) -> MetricRecord {
        MetricRecord {
            tool: tool.to_owned(),
            techniques: "build".to_owned(),
            original_chars: original,
            compacted_chars: compacted,
        }
    }
    #[test]
    fn an_empty_set_says_there_is_no_data() {
        assert!(render_summary(&[]).contains("还没有数据"));
    }

    #[test]
    fn the_summary_aggregates_totals_and_per_tool_rows() {
        let metrics = vec![
            metric("shell", 1_000, 400),
            metric("shell", 1_000, 600),
            metric("grep", 500, 400),
        ];
        let summary = render_summary(&metrics);

        assert!(summary.contains("共 3 次"), "{summary}");
        // 2500 → 1400，节省 1100，44.0%
        assert!(summary.contains("节省 1,100 字符（44.0%）"), "{summary}");
        assert!(
            summary.contains("- shell: 2 次，节省 1,000 字符（50.0%）"),
            "{summary}"
        );
        assert!(
            summary.contains("- grep: 1 次，节省 100 字符（20.0%）"),
            "{summary}"
        );
    }

    #[test]
    fn zero_original_chars_report_zero_percent() {
        let summary = render_summary(&[metric("shell", 0, 0)]);
        assert!(summary.contains("（0.0%）"), "{summary}");
    }

    /// 压缩后反而变长时不能下溢成天文数字。
    #[test]
    fn growth_never_underflows_the_saved_count() {
        let summary = render_summary(&[metric("shell", 100, 250)]);
        assert!(summary.contains("节省 0 字符（0.0%）"), "{summary}");
    }

    #[test]
    fn thousands_separators_match_en_us() {
        assert_eq!(format_chars(0), "0");
        assert_eq!(format_chars(999), "999");
        assert_eq!(format_chars(1_000), "1,000");
        assert_eq!(format_chars(1_234_567), "1,234,567");
    }

    #[test]
    fn techniques_are_joined_or_marked_as_none() {
        assert_eq!(join_techniques(&[]), "none");
        assert_eq!(
            join_techniques(&["ansi".to_owned(), "git".to_owned()]),
            "ansi,git"
        );
    }

    /// 全局容器的两个测试用各自独有的工具名做标记，因此彼此以及与其它测试并发时都不会
    /// 互相干扰（只有 `clear` 会移除记录，而它不会移除别人的标记）。
    #[test]
    fn a_recorded_entry_is_visible_in_the_global_summary() {
        record("marker-visible", 10, 4, &["build".to_owned()]);
        assert!(summary().contains("marker-visible"));
    }

    #[test]
    fn clearing_removes_recorded_entries() {
        record("marker-cleared", 10, 4, &[]);
        assert!(summary().contains("marker-cleared"));

        clear();
        assert!(!summary().contains("marker-cleared"));
    }
}
