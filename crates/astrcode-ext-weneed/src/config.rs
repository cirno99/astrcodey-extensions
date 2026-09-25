//! 全局配置：结构、归一化与持久化。
//!
//! 落在 `<astrcode_dir>/extension_data/astrcode-weneed/config.json`，是**全局**配置而非按
//! 会话——对应 phi-deepseek-enhanced 的 `config.json`。归一化对残缺、类型错误的输入一律
//! 回落默认值，因此用户手改坏的配置文件不会让插件失效。
//!
//! 会话级开关**不在这里**：`/weneed off` 写的是宿主的 `session_state`，见 [`crate::toggle`]。
//! 两者是「全局默认 + 会话覆盖」的关系，全局关是硬闸门。

use std::path::PathBuf;

use astrcode_ext_common::config::{ConfigStore, PluginConfig};
use astrcode_ext_common::paths::extension_data_dir;
use astrcode_extension_sdk::hostpaths;
use serde::Serialize;
use serde_json::{Map, Value};
/// 配置文件在插件数据目录下的文件名。
pub const CONFIG_FILE_NAME: &str = "config.json";

/// 默认被守卫拦截的工具。
///
/// 取 `edit`：本仓库的编辑链路要求「用 `hashline_read` 读、用 `replace` 定点改」，
/// 字符串匹配的 `edit` 在大文件上会因 `read` 压缩而反复匹配失败。`replace` 无法建新文件，
/// 所以 `write` 不在默认名单里。
pub const DEFAULT_BLOCKED_TOOL: &str = "edit";

/// 贴尾风格提醒的触发节奏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReminderMode {
    /// 不贴提醒，只靠 system prompt 里的完整规范。
    Off,
    /// 只在检测到推理漂移时贴；每会话的首次请求额外贴一次作为引导。
    OnDrift,
    /// 每个 provider 请求都贴。
    Always,
}

impl ReminderMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::OnDrift => "on-drift",
            Self::Always => "always",
        }
    }

    fn parse(value: Option<&Value>) -> Option<Self> {
        match value.and_then(Value::as_str) {
            Some("off") => Some(Self::Off),
            Some("on-drift") => Some(Self::OnDrift),
            Some("always") => Some(Self::Always),
            _ => None,
        }
    }
}

/// `pre_tool_use` 工具守卫。
///
/// 语义是**黑名单**而不是 phi 的白名单：只拦显式列出的工具，其余一律放行。白名单在
/// AstrCode 里会把别的扩展注册的工具一起拦掉，而模型没有任何自救手段——只能靠用户手改
/// 配置恢复，是最坏的死锁。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Guard {
    /// 守卫总开关。默认关闭：拦错工具会让模型彻底用不了那个能力。
    pub enabled: bool,
    /// 被拦截的工具名。
    pub blocked_tools: Vec<String>,
}

/// 插件全局配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// 总开关。关闭后规范不再注入，贴尾提醒与守卫也都不生效。
    pub enabled: bool,
    /// 贴尾风格提醒的触发节奏。
    pub reminder: ReminderMode,
    /// 是否读 assistant 的推理通道做漂移判定。
    ///
    /// 关掉之后 `OnDrift` 退化成「只在每会话首次请求贴一次」——没有观测就没有漂移。
    pub drift_check: bool,
    /// 工具守卫。
    pub guard: Guard,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            reminder: ReminderMode::OnDrift,
            drift_check: true,
            guard: Guard {
                enabled: false,
                blocked_tools: vec![DEFAULT_BLOCKED_TOOL.to_owned()],
            },
        }
    }
}

impl Config {
    /// 把任意 JSON 归一化成合法配置。无法识别的字段一律回落默认值。
    pub fn normalize(raw: &Value) -> Self {
        let defaults = Self::default();
        let source = raw.as_object();
        let guard = child_object(source, "guard");

        Self {
            enabled: bool_at(source, "enabled", defaults.enabled),
            reminder: ReminderMode::parse(field(source, "reminder")).unwrap_or(defaults.reminder),
            drift_check: bool_at(source, "driftCheck", defaults.drift_check),
            guard: Guard {
                enabled: bool_at(guard, "enabled", defaults.guard.enabled),
                // 列表只保留字符串项；键缺失才回落默认值，显式空数组是合法配置
                // （守卫开着但不拦任何工具），不该被当成「没配」。
                blocked_tools: match field(guard, "blockedTools").and_then(Value::as_array) {
                    Some(items) => items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    None => defaults.guard.blocked_tools,
                },
            },
        }
    }

    /// 走一遍归一化，让内存缓存与「从磁盘读回来」得到的结果一致。
    ///
    /// 复用 [`Config::normalize`] 的同一套规则，因此内存与磁盘上的归一化永远同源。
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
    use std::fs;

    use astrcode_ext_common::config::EnsureOutcome;

    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_match_the_documented_behaviour() {
        let config = Config::default();
        assert!(config.enabled);
        assert_eq!(config.reminder, ReminderMode::OnDrift);
        assert!(config.drift_check);
        // 守卫默认关闭：拦错工具会让模型彻底用不了那个能力。
        assert!(!config.guard.enabled);
        assert_eq!(config.guard.blocked_tools, vec!["edit"]);
    }

    #[test]
    fn normalize_reads_explicit_values() {
        let config = Config::normalize(&json!({
            "enabled": false,
            "reminder": "always",
            "driftCheck": false,
            "guard": { "enabled": true, "blockedTools": ["edit", "patch"] }
        }));
        assert!(!config.enabled);
        assert_eq!(config.reminder, ReminderMode::Always);
        assert!(!config.drift_check);
        assert!(config.guard.enabled);
        assert_eq!(config.guard.blocked_tools, vec!["edit", "patch"]);
    }

    #[test]
    fn normalize_falls_back_on_bad_types() {
        let config = Config::normalize(&json!({
            "enabled": "yes",
            "reminder": "sometimes",
            "driftCheck": 1,
            "guard": "on"
        }));
        assert_eq!(config, Config::default());
    }

    /// 显式空数组是「守卫开着但不拦任何工具」，不该被当成键缺失而回落默认名单。
    #[test]
    fn an_explicit_empty_blocklist_is_not_the_default_blocklist() {
        let config = Config::normalize(&json!({ "guard": { "blockedTools": [] } }));
        assert!(config.guard.blocked_tools.is_empty());
    }

    #[test]
    fn non_string_list_items_are_dropped() {
        let config = Config::normalize(&json!({ "guard": { "blockedTools": ["edit", 42, null] } }));
        assert_eq!(config.guard.blocked_tools, vec!["edit"]);
    }

    #[test]
    fn reminder_mode_round_trips_through_its_wire_names() {
        for mode in [
            ReminderMode::Off,
            ReminderMode::OnDrift,
            ReminderMode::Always,
        ] {
            assert_eq!(
                ReminderMode::parse(Some(&json!(mode.as_str()))),
                Some(mode),
                "{}",
                mode.as_str()
            );
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("round-trip");
        let store = store_at(dir.join(CONFIG_FILE_NAME));
        let config = Config {
            reminder: ReminderMode::Always,
            guard: Guard {
                enabled: true,
                blocked_tools: vec!["edit".to_owned()],
            },
            ..Config::default()
        };

        store.save(&config).expect("保存应成功");
        let loaded = store.load::<Config>();
        assert!(loaded.warning.is_none(), "{:?}", loaded.warning);
        assert_eq!(loaded.config, config);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_not_a_warning() {
        let dir = temp_dir("missing");
        let loaded = store_at(dir.join(CONFIG_FILE_NAME)).load::<Config>();
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn a_broken_file_warns_and_falls_back_to_defaults() {
        let dir = temp_dir("broken");
        fs::create_dir_all(&dir).expect("建目录应成功");
        let path = dir.join(CONFIG_FILE_NAME);
        fs::write(&path, "{ oops").expect("写入应成功");

        let loaded = store_at(&path).load::<Config>();
        assert!(loaded.warning.is_some());
        assert_eq!(loaded.config, Config::default());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_exists_creates_the_file_once() {
        let dir = temp_dir("ensure");
        let store = store_at(dir.join(CONFIG_FILE_NAME));

        assert_eq!(store.ensure_exists::<Config>(), EnsureOutcome::Created);
        assert_eq!(store.ensure_exists::<Config>(), EnsureOutcome::Exists);
        assert_eq!(store.load::<Config>().config, Config::default());

        let _ = fs::remove_dir_all(&dir);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-weneed-config-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }
}
