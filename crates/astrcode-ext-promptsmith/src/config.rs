//! 全局配置：结构、归一化与持久化。
//!
//! 落在 `<astrcode_dir>/extension_data/astrcode-promptsmith/config.json`，是**全局**配置。
//! 待发送的改写结果不在这里，那是按会话存的 `session_state`，见 [`crate::pending`]。
//!
//! 归一化对残缺或类型错误的输入一律回落默认值，因此用户手改坏的配置文件不会让插件失效——
//! 与本仓库 `astrcode-ext-weneed` 的配置层保持同一套约定。
//!
//! 上游 pi-promptsmith 的 `targetFamily` / `map` / `enhancer-model fixed` 三组配置**没有移植**：
//! 前者是给 OpenAI 与 Anthropic 两系 prompt 风格做微调的，本仓库使用者的激活模型
//! （deepseek-* / glm-* / qwen-*）全都落不进那两条规则；后两者依赖能枚举的模型注册表，
//! 宿主没有把这个能力暴露给磁盘扩展。

use std::path::PathBuf;

use astrcode_ext_common::config::{ConfigStore, PluginConfig};
use astrcode_ext_common::paths::extension_data_dir;
use astrcode_extension_sdk::hostpaths;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::intent::RewriteMode;

/// 配置文件在插件数据目录下的文件名。
pub const CONFIG_FILE_NAME: &str = "config.json";

/// 改写力度，透传给 enhancer 模型作为上下文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strength {
    Light,
    Balanced,
    Strong,
}

impl Strength {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Balanced => "balanced",
            Self::Strong => "strong",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "balanced" => Some(Self::Balanced),
            "strong" => Some(Self::Strong),
            _ => None,
        }
    }
}

/// 改写请求打到哪个模型档位。
///
/// 宿主只暴露 main / small 两个档位（`HostLlmChatRequest` 没有模型覆盖字段），因此上游的
/// `enhancer-model fixed <provider>/<id>` 在这里塌缩成二选一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enhancer {
    /// 小模型：快且省，默认。宿主没配小模型时自动回落 [`Self::Main`]。
    Small,
    /// 主模型：改写质量更稳，但每次多付一轮主模型延迟。
    Main,
}

impl Enhancer {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Main => "main",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "small" => Some(Self::Small),
            "main" => Some(Self::Main),
            _ => None,
        }
    }
}

/// 插件全局配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// 改写模式：auto 由意图决定，其余两个强制。
    pub mode: RewriteMode,
    /// 改写力度。
    pub strength: Strength,
    /// 改写请求用的模型档位。
    pub enhancer: Enhancer,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: RewriteMode::Auto,
            strength: Strength::Balanced,
            // 默认走小模型：改写是每条提示词都要付的一次串行调用，主模型太贵。
            // 判错档位时 `/smith enhancer main` 一行改回来。
            enhancer: Enhancer::Small,
        }
    }
}

impl Config {
    /// 把任意 JSON 归一化成合法配置。无法识别的字段一律回落默认值。
    pub fn normalize(raw: &Value) -> Self {
        let defaults = Self::default();
        let source = raw.as_object();

        Self {
            mode: enum_field(source, "mode", RewriteMode::parse, defaults.mode),
            strength: enum_field(source, "strength", Strength::parse, defaults.strength),
            enhancer: enum_field(source, "enhancer", Enhancer::parse, defaults.enhancer),
        }
    }
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

/// 在指定路径上打开配置。
pub fn store_at(path: impl Into<PathBuf>) -> ConfigStore {
    ConfigStore::new(path, hostpaths::write_file_atomic)
}

/// 默认位置：`<astrcode_dir>/extension_data/<插件 id>/config.json`。
pub fn default_path() -> PathBuf {
    extension_data_dir(hostpaths::astrcode_dir(), crate::EXTENSION_ID).join(CONFIG_FILE_NAME)
}

/// 默认位置的配置。
pub fn store() -> ConfigStore {
    store_at(default_path())
}

/// 取一个字符串枚举字段；缺失、类型不符或值不认识都回落默认值。
fn enum_field<T: Copy>(
    source: Option<&Map<String, Value>>,
    key: &str,
    parse: impl Fn(&str) -> Option<T>,
    default: T,
) -> T {
    source
        .and_then(|map| map.get(key))
        .and_then(Value::as_str)
        .and_then(parse)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_are_auto_balanced_small() {
        let config = Config::default();
        assert_eq!(config.mode, RewriteMode::Auto);
        assert_eq!(config.strength, Strength::Balanced);
        assert_eq!(config.enhancer, Enhancer::Small);
    }

    #[test]
    fn normalize_accepts_valid_values() {
        let config = Config::normalize(&json!({
            "mode": "execution-contract",
            "strength": "strong",
            "enhancer": "main",
        }));
        assert_eq!(config.mode, RewriteMode::ExecutionContract);
        assert_eq!(config.strength, Strength::Strong);
        assert_eq!(config.enhancer, Enhancer::Main);
    }

    #[test]
    fn normalize_falls_back_on_unknown_or_wrong_typed_values() {
        let config = Config::normalize(&json!({
            "mode": "turbo",
            "strength": 42,
            "enhancer": "main",
        }));
        assert_eq!(config.mode, RewriteMode::Auto, "未知取值回落默认");
        assert_eq!(config.strength, Strength::Balanced, "类型错误回落默认");
        assert_eq!(config.enhancer, Enhancer::Main);
    }

    #[test]
    fn normalize_survives_non_object_document() {
        assert_eq!(Config::normalize(&json!("nope")), Config::default());
        assert_eq!(Config::normalize(&Value::Null), Config::default());
    }

    /// 上游遗留的家族/映射字段必须被**忽略**而不是报错：用户从旧配置抄来的键不该让整份配置作废。
    #[test]
    fn legacy_family_keys_are_ignored_not_fatal() {
        let config = Config::normalize(&json!({
            "family": "claude",
            "targetFamilyMode": "auto",
            "enhancerModelMode": "family-linked",
            "mode": "plain",
        }));
        assert_eq!(config.mode, RewriteMode::Plain);
        assert_eq!(config.strength, Strength::Balanced);
    }

    #[test]
    fn render_is_normalized_and_round_trips() {
        let rendered = Config::render(&Config::default()).unwrap();
        assert!(rendered.ends_with('\n'), "落盘正文应有结尾换行，方便手改");
        assert_eq!(Config::decode(rendered.into_bytes()).unwrap(), Config::default());
    }

    #[test]
    fn store_at_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("promptsmith-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = store_at(dir.join("config.json"));
        let config = Config {
            mode: RewriteMode::Plain,
            strength: Strength::Light,
            enhancer: Enhancer::Main,
        };
        store.save(&config).unwrap();
        let loaded = store.load::<Config>();
        assert_eq!(loaded.config, config);
        assert_eq!(loaded.warning, None, "读回来的配置不该带解析警告");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
