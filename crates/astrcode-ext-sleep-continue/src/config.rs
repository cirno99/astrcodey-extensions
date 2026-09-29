//! 全局配置：结构、归一化与持久化。
//!
//! 落在 `<astrcode_dir>/extension_data/astrcode-sleep-continue/config.json`，是**全局**配置
//! 而非按会话。归一化对残缺、类型错误的输入一律回落默认值，因此用户手改坏的配置文件不会
//! 让插件失效。
//!
//! 会话级开关**不在这里**：`/sleep on` 写的是宿主的 `session_state`，见 [`crate::state`]。
//! 两者是「全局默认 + 会话覆盖」的关系，全局关是硬闸门。
//!
//! 上游 pi 插件（`sleep-continue`）把配置放在环境变量与进程内存里，本插件改成落盘：
//! 无人值守场景下扩展进程会被宿主重载，内存态会在你最需要它的时候消失。

use std::path::PathBuf;

use astrcode_ext_common::config::{ConfigStore, PluginConfig};
use astrcode_ext_common::paths::extension_data_dir;
use astrcode_extension_sdk::hostpaths;
use serde::Serialize;
use serde_json::{Map, Value};

/// 配置文件在插件数据目录下的文件名。
pub const CONFIG_FILE_NAME: &str = "config.json";

/// 默认续跑文本。
///
/// 用中文而不是英文：这条文本既给模型看，也**原样落进转录**给人复盘，跟着用户的语言走。
pub const DEFAULT_CONTINUE_TEXT: &str = "继续";

/// 默认迭代上限：单次人工 turn 最多续跑多少次。到顶只停止续跑，开关保持开启。
pub const DEFAULT_MAX: u32 = 100;

/// 默认空转熔断阈值：连续多少次续跑都没有产生工具调用就停下。0 表示关闭熔断。
pub const DEFAULT_IDLE_STOP: u32 = 3;

/// 默认复读熔断阈值：连续多少次续跑都「没有新内容」（没有工具调用，且回复与上一次重复或
/// 为空）就停下。0 表示关闭。
///
/// 默认 1 比空转阈值严：复读是模型在输出里打转，多喂几轮几乎不会自己走出来，而每一轮都要
/// 烧掉整段上下文。宁可早点停下让人看一眼。
pub const DEFAULT_NO_PROGRESS_STOP: u32 = 1;

/// 上一步没有新内容时改用的纠正提示。
///
/// **这段文本模型可见**（它会作为用户消息落进转录），改动必须同步 `AGENTS.md`。
/// 措辞刻意把两种情形都点出来（没有工具调用 / 回复重复或为空）：模型看得到自己上一步
/// 干了什么，把判据说清楚才有可能让它换一种动作，而不是再复述一遍计划。
pub const DEFAULT_NUDGE_TEXT: &str = "上一步没有产生可继续的动作：没有工具调用，或回复与上一次重复、为空。请直接给出下一步具体动作或结论，不要复述计划。";

/// 默认重试上限：单次人工 turn 因失败而自动重起的次数。0 表示关闭失败重试。
pub const DEFAULT_RETRY_MAX: u32 = 3;

/// 默认首次重试前的等待时长（毫秒）。
pub const DEFAULT_RETRY_BASE_DELAY_MS: u32 = 1_000;

/// 默认退避上限（毫秒）。
pub const DEFAULT_RETRY_MAX_DELAY_MS: u32 = 5_000;

/// 默认被自动应答的提问工具。宿主内建的提问工具叫 `askUser`。
pub const DEFAULT_ANSWER_TOOLS: [&str; 1] = ["askUser"];
/// 续跑文本的长度上限，按 UTF-8 字节计。
///
/// 宿主侧的上限是 1 MiB（`MAX_PROMPT_TEXT_BYTES`），这里收紧到 8 KiB：续跑文本是一句
/// 提醒，不是说明书；在命令边界就拦住超长输入，比让它每次续跑都失败要诚实。
pub const MAX_CONTINUE_TEXT_BYTES: usize = 8 * 1024;

/// 提问自动应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    /// 是否拦截提问类工具调用。
    pub enabled: bool,
    /// 被拦截的工具名。名单之外的调用一律放行——误伤面等于零。
    pub tools: Vec<String>,
}

/// 插件全局配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// 总开关。默认开：真正的闸门是**每会话开关**（`/sleep on`），这一项是给「一次关掉
    /// 所有会话」用的。
    ///
    /// 默认必须开，否则 `/sleep on` 会是一个静默无效的命令——新装插件的人第一次用它就
    /// 撞上「命令说开了，但什么都不发生」，这是最坏的第一印象。
    pub enabled: bool,
    /// 续跑时注入的用户消息正文。
    pub continue_text: String,
    /// 迭代上限（单次人工 turn）。
    pub max: u32,
    /// 空转熔断阈值，0 表示关闭。
    pub idle_stop: u32,
    /// 复读熔断阈值，0 表示关闭。
    pub no_progress_stop: u32,
    /// 上一步没有新内容时改用的纠正提示。
    pub nudge_text: String,
    /// 失败重试上限（单次人工 turn），0 表示关闭失败重试。
    pub retry_max: u32,
    /// 首次重试前的等待时长（毫秒）。
    pub retry_base_delay_ms: u32,
    /// 退避上限（毫秒）。
    pub retry_max_delay_ms: u32,
    /// 提问自动应答。
    pub answer: Answer,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            continue_text: DEFAULT_CONTINUE_TEXT.to_owned(),
            max: DEFAULT_MAX,
            idle_stop: DEFAULT_IDLE_STOP,
            no_progress_stop: DEFAULT_NO_PROGRESS_STOP,
            nudge_text: DEFAULT_NUDGE_TEXT.to_owned(),
            retry_max: DEFAULT_RETRY_MAX,
            retry_base_delay_ms: DEFAULT_RETRY_BASE_DELAY_MS,
            retry_max_delay_ms: DEFAULT_RETRY_MAX_DELAY_MS,
            answer: Answer {
                enabled: true,
                tools: DEFAULT_ANSWER_TOOLS
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
            },
        }
    }
}

impl Config {
    /// 把任意 JSON 归一化成合法配置。无法识别的字段一律回落默认值。
    pub fn normalize(raw: &Value) -> Self {
        let defaults = Self::default();
        let source = raw.as_object();
        let answer = child_object(source, "answer");

        Self {
            enabled: bool_at(source, "enabled", defaults.enabled),
            // 空串会让宿主的 `non_empty_session_content` 拒绝注入，所以在归一化阶段就换成
            // 默认文本——把「配置写坏了」表现成「续跑仍然是好的」。
            continue_text: string_at(source, "continueText")
                .filter(|text| !text.is_empty())
                .unwrap_or(defaults.continue_text),
            // 上限 0 与「关闭续跑」语义重叠，交给会话开关表达；这里回落默认值。
            max: u32_at(source, "max")
                .filter(|max| *max > 0)
                .unwrap_or(defaults.max),
            // 熔断阈值 0 是合法配置：显式关闭熔断。
            idle_stop: u32_at(source, "idleStop").unwrap_or(defaults.idle_stop),
            // 同上：0 表示关闭复读熔断。
            no_progress_stop: u32_at(source, "noProgressStop").unwrap_or(defaults.no_progress_stop),
            // 空串与缺省同义：喂给模型一句空话等于没纠正。
            nudge_text: string_at(source, "nudgeText")
                .filter(|text| !text.is_empty())
                .unwrap_or(defaults.nudge_text),
            // 重试上限 0 是合法配置：显式关闭失败重试。
            retry_max: u32_at(source, "retryMax").unwrap_or(defaults.retry_max),
            // 退避时长 0 会被当成「不等」，虽然合法但没有意义；用默认值兜住更实在。
            retry_base_delay_ms: u32_at(source, "retryBaseDelayMs")
                .filter(|delay| *delay > 0)
                .unwrap_or(defaults.retry_base_delay_ms),
            retry_max_delay_ms: u32_at(source, "retryMaxDelayMs")
                .filter(|delay| *delay > 0)
                .unwrap_or(defaults.retry_max_delay_ms),
            answer: Answer {
                enabled: bool_at(answer, "enabled", defaults.answer.enabled),
                // 列表只保留字符串项；键缺失才回落默认值，显式空数组是合法配置
                // （应答开着但不拦任何工具），不该被当成「没配」。
                tools: match field(answer, "tools").and_then(Value::as_array) {
                    Some(items) => items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    None => defaults.answer.tools,
                },
            },
        }
    }

    /// 走一遍归一化，让内存缓存与「从磁盘读回来」得到的结果一致。
    pub fn normalized(&self) -> Result<Self, String> {
        let raw = serde_json::to_value(self).map_err(|error| error.to_string())?;
        Ok(Self::normalize(&raw))
    }
}

fn field<'a>(source: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a Value> {
    source.and_then(|map| map.get(key))
}

fn child_object<'a>(
    source: Option<&'a Map<String, Value>>,
    key: &str,
) -> Option<&'a Map<String, Value>> {
    field(source, key).and_then(Value::as_object)
}

fn bool_at(source: Option<&Map<String, Value>>, key: &str, fallback: bool) -> bool {
    field(source, key)
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

/// 取一个非负整数。JSON 里写成浮点但值为整数（`3.0`）也接受，因为手写配置里很常见。
fn u32_at(source: Option<&Map<String, Value>>, key: &str) -> Option<u32> {
    let value = field(source, key)?;
    if let Some(number) = value.as_u64() {
        return u32::try_from(number).ok();
    }
    let number = value.as_f64()?;
    if number < 0.0 || number.fract() != 0.0 || number > f64::from(u32::MAX) {
        return None;
    }
    Some(number as u32)
}

fn string_at(source: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    field(source, key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 默认位置：`<astrcode_dir>/extension_data/<extension_id>/config.json`。
pub fn default_path() -> PathBuf {
    extension_data_dir(hostpaths::astrcode_dir(), crate::EXTENSION_ID).join(CONFIG_FILE_NAME)
}

/// 定位默认位置的配置。
pub fn store() -> ConfigStore {
    ConfigStore::new(default_path(), hostpaths::write_file_atomic)
}

/// 在显式路径上打开配置；测试与工具函数用它避开真实用户目录。
pub fn store_at(path: impl Into<PathBuf>) -> ConfigStore {
    ConfigStore::new(path, hostpaths::write_file_atomic)
}

impl PluginConfig for Config {
    /// 解析并归一化。未知字段被忽略——用户多加一个自用字段，不该把已经写好的设置一起丢掉。
    fn decode(bytes: Vec<u8>) -> Result<Self, String> {
        let raw: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        Ok(Self::normalize(&raw))
    }

    /// 归一化后渲染成落盘正文；末尾补一个换行，让手改过的文件与自动写入的形态一致。
    fn render(&self) -> Result<String, String> {
        let raw = serde_json::to_value(self).map_err(|error| error.to_string())?;
        let normalized = Self::normalize(&raw);
        serde_json::to_string_pretty(&normalized)
            .map(|json| format!("{json}\n"))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_match_the_documented_behaviour() {
        let config = Config::default();
        // 默认开：`/sleep on` 必须开箱即用；闸门在每会话开关上。
        assert!(config.enabled);
        assert_eq!(config.continue_text, DEFAULT_CONTINUE_TEXT);
        assert_eq!(config.max, DEFAULT_MAX);
        assert_eq!(config.idle_stop, DEFAULT_IDLE_STOP);
        assert_eq!(config.no_progress_stop, DEFAULT_NO_PROGRESS_STOP);
        assert_eq!(config.nudge_text, DEFAULT_NUDGE_TEXT);
        assert_eq!(config.retry_max, DEFAULT_RETRY_MAX);
        assert_eq!(config.retry_base_delay_ms, DEFAULT_RETRY_BASE_DELAY_MS);
        assert_eq!(config.retry_max_delay_ms, DEFAULT_RETRY_MAX_DELAY_MS);
        assert!(config.answer.enabled);
        assert_eq!(config.answer.tools, vec!["askUser"]);
    }

    #[test]
    fn normalize_reads_explicit_values() {
        let config = Config::normalize(&json!({
            "enabled": true,
            "continueText": "继续按计划推进",
            "max": 200,
            "idleStop": 5,
            "noProgressStop": 2,
            "nudgeText": "请直接给出下一步动作",
            "retryMax": 1,
            "retryBaseDelayMs": 250,
            "retryMaxDelayMs": 800,
            "answer": { "enabled": false, "tools": ["askUser", "questionnaire"] }
        }));
        assert!(config.enabled);
        assert_eq!(config.continue_text, "继续按计划推进");
        assert_eq!(config.max, 200);
        assert_eq!(config.idle_stop, 5);
        assert_eq!(config.no_progress_stop, 2);
        assert_eq!(config.nudge_text, "请直接给出下一步动作");
        assert_eq!(config.retry_max, 1);
        assert_eq!(config.retry_base_delay_ms, 250);
        assert_eq!(config.retry_max_delay_ms, 800);
        assert!(!config.answer.enabled);
        assert_eq!(config.answer.tools, vec!["askUser", "questionnaire"]);
    }

    #[test]
    fn normalize_falls_back_on_bad_types() {
        let config = Config::normalize(&json!({
            "enabled": "yes",
            "continueText": 7,
            "max": "many",
            "idleStop": [],
            "noProgressStop": {},
            "nudgeText": 7,
            "retryMax": "many",
            "retryBaseDelayMs": [],
            "retryMaxDelayMs": null,
            "answer": "on"
        }));
        assert_eq!(config, Config::default());
    }

    /// 未知字段被忽略：用户多加一个自用字段不该把整份配置作废。
    #[test]
    fn unknown_fields_are_ignored() {
        let config = Config::normalize(&json!({ "enabled": true, "myOwnNote": 42 }));
        assert!(config.enabled);
        assert_eq!(config.continue_text, DEFAULT_CONTINUE_TEXT);
    }

    /// 复读熔断阈值 0 同样是合法配置：显式关闭。
    #[test]
    fn a_zero_no_progress_stop_disables_the_breaker() {
        assert_eq!(
            Config::normalize(&json!({ "noProgressStop": 0 })).no_progress_stop,
            0
        );
    }

    /// 重试上限 0 是「关掉失败重试」。
    #[test]
    fn a_zero_retry_max_disables_retries() {
        assert_eq!(Config::normalize(&json!({ "retryMax": 0 })).retry_max, 0);
    }

    /// 纠正提示是喂给模型的正文，空串等于没纠正，回落默认。
    #[test]
    fn an_empty_nudge_text_falls_back_to_the_default() {
        assert_eq!(
            Config::normalize(&json!({ "nudgeText": "" })).nudge_text,
            DEFAULT_NUDGE_TEXT
        );
    }

    /// 退避时长 0 会被当成「不等」，没有意义，回落默认。
    #[test]
    fn zero_backoff_delays_fall_back_to_the_defaults() {
        let config = Config::normalize(&json!({
            "retryBaseDelayMs": 0,
            "retryMaxDelayMs": 0
        }));
        assert_eq!(config.retry_base_delay_ms, DEFAULT_RETRY_BASE_DELAY_MS);
        assert_eq!(config.retry_max_delay_ms, DEFAULT_RETRY_MAX_DELAY_MS);
    }

    /// 空串会让宿主的 `non_empty_session_content` 拒绝注入，因此归一化阶段换成默认文本。
    #[test]
    fn an_empty_continue_text_falls_back_to_the_default() {
        assert_eq!(
            Config::normalize(&json!({ "continueText": "" })).continue_text,
            DEFAULT_CONTINUE_TEXT
        );
    }

    /// 上限 0 与会话开关语义重叠，回落默认值而不是变成「一次都不续跑」。
    #[test]
    fn a_zero_max_falls_back_to_the_default() {
        assert_eq!(Config::normalize(&json!({ "max": 0 })).max, DEFAULT_MAX);
    }

    /// 熔断阈值 0 是合法配置：显式关闭熔断。
    #[test]
    fn a_zero_idle_stop_disables_the_breaker() {
        assert_eq!(Config::normalize(&json!({ "idleStop": 0 })).idle_stop, 0);
    }

    /// 手写配置里 `3.0` 很常见，值为整数就接受。
    #[test]
    fn integral_floats_are_accepted_for_counts() {
        let config = Config::normalize(&json!({ "max": 5.0, "idleStop": 2.0 }));
        assert_eq!(config.max, 5);
        assert_eq!(config.idle_stop, 2);
    }

    #[test]
    fn negative_and_fractional_counts_fall_back() {
        assert_eq!(Config::normalize(&json!({ "max": -1 })).max, DEFAULT_MAX);
        assert_eq!(Config::normalize(&json!({ "max": 1.5 })).max, DEFAULT_MAX);
    }

    /// 显式空数组是「应答开着但不拦任何工具」，不该被当成键缺失而回落默认名单。
    #[test]
    fn an_explicit_empty_tool_list_is_not_the_default_list() {
        let config = Config::normalize(&json!({ "answer": { "tools": [] } }));
        assert!(config.answer.tools.is_empty());
    }

    #[test]
    fn non_string_tool_items_are_dropped() {
        let config = Config::normalize(&json!({ "answer": { "tools": ["askUser", 42, null] } }));
        assert_eq!(config.answer.tools, vec!["askUser"]);
    }

    #[test]
    fn decode_reports_broken_json() {
        assert!(Config::decode(b"{ not json".to_vec()).is_err());
    }

    #[test]
    fn render_ends_with_a_newline_and_normalizes() {
        let body = Config::normalize(&json!({ "enabled": true }))
            .render()
            .expect("render");
        assert!(body.ends_with("}\n"), "{body}");
        assert!(body.contains("\"continueText\""), "{body}");
    }
}
