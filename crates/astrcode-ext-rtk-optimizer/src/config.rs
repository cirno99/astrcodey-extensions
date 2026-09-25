//! 配置结构、归一化与持久化。
//!
//! 字段名、默认值、取值范围与上游 `pi-rtk-optimizer` 的 `types.ts` / `config-store.ts`
//! 逐条对齐：归一化对残缺、越界、类型错误的输入一律回落默认值而不是报错，因此用户手改
//! 坏的 `config.json` 不会让插件失效。
//!
//! 序列化字段顺序也按上游归一化后的键序排列，落盘文件因此可以与上游逐字节对照。

use std::path::PathBuf;

use astrcode_ext_common::paths::extension_data_dir;
use astrcode_extension_sdk::hostpaths;
use serde::Serialize;
use serde_json::{Map, Value};
/// 配置文件在插件数据目录下的文件名。
pub const CONFIG_FILE_NAME: &str = "config.json";

/// 硬截断上限的合法区间（字符数）。
pub const TRUNCATE_MAX_CHARS_RANGE: (usize, usize) = (1_000, 200_000);
/// smart truncate 上限的合法区间（行数）。
pub const SMART_TRUNCATE_MAX_LINES_RANGE: (usize, usize) = (40, 4_000);

/// 改写模式：自动改写，或只提示不替换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Rewrite,
    Suggest,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rewrite => "rewrite",
            Self::Suggest => "suggest",
        }
    }

    fn parse(value: Option<&Value>) -> Option<Self> {
        match value.and_then(Value::as_str) {
            Some("rewrite") => Some(Self::Rewrite),
            Some("suggest") => Some(Self::Suggest),
            _ => None,
        }
    }
}

/// `read` 输出的源码过滤强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceFilterLevel {
    None,
    Minimal,
    Aggressive,
}

impl SourceFilterLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Aggressive => "aggressive",
        }
    }

    fn parse(value: Option<&Value>) -> Option<Self> {
        match value.and_then(Value::as_str) {
            Some("none") => Some(Self::None),
            Some("minimal") => Some(Self::Minimal),
            Some("aggressive") => Some(Self::Aggressive),
            _ => None,
        }
    }
}

/// `read` 压缩开关。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadCompaction {
    pub enabled: bool,
}

/// 硬字符截断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Truncate {
    pub enabled: bool,
    pub max_chars: usize,
}

/// 按行智能截断（保留签名、导入、常量等关键行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartTruncate {
    pub enabled: bool,
    pub max_lines: usize,
}

/// 输出压缩管线配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputCompaction {
    pub enabled: bool,
    pub strip_ansi: bool,
    pub read_compaction: ReadCompaction,
    pub source_code_filtering_enabled: bool,
    pub preserve_exact_skill_reads: bool,
    pub truncate: Truncate,
    pub source_code_filtering: SourceFilterLevel,
    pub smart_truncate: SmartTruncate,
    pub aggregate_test_output: bool,
    pub filter_build_output: bool,
    pub compact_git_output: bool,
    pub aggregate_linter_output: bool,
    pub group_search_output: bool,
    pub track_savings: bool,
}

/// 插件总配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub enabled: bool,
    pub mode: Mode,
    pub guard_when_rtk_missing: bool,
    pub show_rewrite_notifications: bool,
    pub output_compaction: OutputCompaction,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Mode::Rewrite,
            guard_when_rtk_missing: true,
            show_rewrite_notifications: true,
            output_compaction: OutputCompaction {
                enabled: true,
                strip_ansi: true,
                // 默认关闭：有损压缩会把 `read` 的整行丢掉，后续编辑可能因此匹配失败。
                read_compaction: ReadCompaction { enabled: false },
                source_code_filtering_enabled: false,
                preserve_exact_skill_reads: false,
                truncate: Truncate {
                    enabled: true,
                    max_chars: 12_000,
                },
                source_code_filtering: SourceFilterLevel::None,
                smart_truncate: SmartTruncate {
                    enabled: false,
                    max_lines: 220,
                },
                aggregate_test_output: true,
                filter_build_output: true,
                compact_git_output: true,
                aggregate_linter_output: true,
                group_search_output: true,
                track_savings: true,
            },
        }
    }
}

impl Config {
    /// 把任意 JSON 归一化成合法配置。无法识别的字段一律回落默认值。
    pub fn normalize(raw: &Value) -> Self {
        let defaults = Self::default();
        let source = raw.as_object();
        let output = child_object(source, "outputCompaction");
        let read_compaction = child_object(output, "readCompaction");
        let truncate = child_object(output, "truncate");
        let smart_truncate = child_object(output, "smartTruncate");

        // 旧版配置没有 `readCompaction` 键，那时 `read` 压缩是默认开启的。保留这条迁移
        // 分支，用户升级插件后行为不会突变。
        let legacy = output.is_none_or(|map| !map.contains_key("readCompaction"));
        let source_filtering_fallback = if legacy {
            true
        } else {
            defaults.output_compaction.source_code_filtering_enabled
        };
        let source_filter_level_fallback = if legacy {
            SourceFilterLevel::Minimal
        } else {
            defaults.output_compaction.source_code_filtering
        };
        let smart_truncate_enabled_fallback = if legacy {
            true
        } else {
            defaults.output_compaction.smart_truncate.enabled
        };

        Self {
            enabled: bool_at(source, "enabled", defaults.enabled),
            mode: Mode::parse(field(source, "mode")).unwrap_or(defaults.mode),
            guard_when_rtk_missing: bool_at(
                source,
                "guardWhenRtkMissing",
                defaults.guard_when_rtk_missing,
            ),
            show_rewrite_notifications: bool_at(
                source,
                "showRewriteNotifications",
                defaults.show_rewrite_notifications,
            ),
            output_compaction: OutputCompaction {
                enabled: bool_at(output, "enabled", defaults.output_compaction.enabled),
                strip_ansi: bool_at(output, "stripAnsi", defaults.output_compaction.strip_ansi),
                read_compaction: ReadCompaction {
                    enabled: if legacy {
                        true
                    } else {
                        bool_at(
                            read_compaction,
                            "enabled",
                            defaults.output_compaction.read_compaction.enabled,
                        )
                    },
                },
                source_code_filtering_enabled: bool_at(
                    output,
                    "sourceCodeFilteringEnabled",
                    source_filtering_fallback,
                ),
                preserve_exact_skill_reads: bool_at(
                    output,
                    "preserveExactSkillReads",
                    defaults.output_compaction.preserve_exact_skill_reads,
                ),
                truncate: Truncate {
                    enabled: bool_at(
                        truncate,
                        "enabled",
                        defaults.output_compaction.truncate.enabled,
                    ),
                    max_chars: int_at(
                        truncate,
                        "maxChars",
                        defaults.output_compaction.truncate.max_chars,
                        TRUNCATE_MAX_CHARS_RANGE,
                    ),
                },
                source_code_filtering: SourceFilterLevel::parse(field(
                    output,
                    "sourceCodeFiltering",
                ))
                .unwrap_or(source_filter_level_fallback),
                smart_truncate: SmartTruncate {
                    enabled: bool_at(
                        smart_truncate,
                        "enabled",
                        smart_truncate_enabled_fallback,
                    ),
                    max_lines: int_at(
                        smart_truncate,
                        "maxLines",
                        defaults.output_compaction.smart_truncate.max_lines,
                        SMART_TRUNCATE_MAX_LINES_RANGE,
                    ),
                },
                aggregate_test_output: bool_at(
                    output,
                    "aggregateTestOutput",
                    defaults.output_compaction.aggregate_test_output,
                ),
                filter_build_output: bool_at(
                    output,
                    "filterBuildOutput",
                    defaults.output_compaction.filter_build_output,
                ),
                compact_git_output: bool_at(
                    output,
                    "compactGitOutput",
                    defaults.output_compaction.compact_git_output,
                ),
                aggregate_linter_output: bool_at(
                    output,
                    "aggregateLinterOutput",
                    defaults.output_compaction.aggregate_linter_output,
                ),
                group_search_output: bool_at(
                    output,
                    "groupSearchOutput",
                    defaults.output_compaction.group_search_output,
                ),
                track_savings: bool_at(
                    output,
                    "trackSavings",
                    defaults.output_compaction.track_savings,
                ),
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

    /// 渲染成 `config.json` 的正文。
    ///
    /// 键序与 2 空格缩进逐行手写，以对齐上游 `JSON.stringify(normalized, null, 2)` 的
    /// 输出：`serde_json` 默认把对象键按字典序排，直接用它会丢掉上游的字段顺序。
    pub fn to_json_text(&self) -> String {
        let compaction = &self.output_compaction;
        let mut lines: Vec<String> = vec![
            "{".to_owned(),
            format!("  \"enabled\": {},", self.enabled),
            format!("  \"mode\": \"{}\",", self.mode.as_str()),
            format!(
                "  \"guardWhenRtkMissing\": {},",
                self.guard_when_rtk_missing
            ),
            format!(
                "  \"showRewriteNotifications\": {},",
                self.show_rewrite_notifications
            ),
            "  \"outputCompaction\": {".to_owned(),
            format!("    \"enabled\": {},", compaction.enabled),
            format!("    \"stripAnsi\": {},", compaction.strip_ansi),
            "    \"readCompaction\": {".to_owned(),
            format!(
                "      \"enabled\": {}",
                compaction.read_compaction.enabled
            ),
            "    },".to_owned(),
            format!(
                "    \"sourceCodeFilteringEnabled\": {},",
                compaction.source_code_filtering_enabled
            ),
            format!(
                "    \"preserveExactSkillReads\": {},",
                compaction.preserve_exact_skill_reads
            ),
            "    \"truncate\": {".to_owned(),
            format!("      \"enabled\": {},", compaction.truncate.enabled),
            format!("      \"maxChars\": {}", compaction.truncate.max_chars),
            "    },".to_owned(),
            format!(
                "    \"sourceCodeFiltering\": \"{}\",",
                compaction.source_code_filtering.as_str()
            ),
            "    \"smartTruncate\": {".to_owned(),
            format!(
                "      \"enabled\": {},",
                compaction.smart_truncate.enabled
            ),
            format!(
                "      \"maxLines\": {}",
                compaction.smart_truncate.max_lines
            ),
            "    },".to_owned(),
            format!(
                "    \"aggregateTestOutput\": {},",
                compaction.aggregate_test_output
            ),
            format!(
                "    \"filterBuildOutput\": {},",
                compaction.filter_build_output
            ),
            format!(
                "    \"compactGitOutput\": {},",
                compaction.compact_git_output
            ),
            format!(
                "    \"aggregateLinterOutput\": {},",
                compaction.aggregate_linter_output
            ),
            format!(
                "    \"groupSearchOutput\": {},",
                compaction.group_search_output
            ),
            format!("    \"trackSavings\": {}", compaction.track_savings),
            "  }".to_owned(),
            "}".to_owned(),
        ];
        lines.push(String::new());
        lines.join("\n")
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
    field(source, key).and_then(Value::as_bool).unwrap_or(fallback)
}

/// 读取整数并夹到 `[min, max]`。
///
/// 非数字、`NaN`/`Infinity` 一律回落 `fallback`；小数四舍五入后夹取，与上游的
/// `Math.round` + `Math.max`/`Math.min` 一致。负值会被夹到 `min`，因此
/// 「Rust 远离零取整、JS 向正无穷取整」的差异在合法区间内不可观测。
fn int_at(
    source: Option<&Map<String, Value>>,
    key: &str,
    fallback: usize,
    (min, max): (usize, usize),
) -> usize {
    let Some(value) = field(source, key).and_then(Value::as_f64) else {
        return fallback;
    };
    if !value.is_finite() {
        return fallback;
    }
    value.round().clamp(min as f64, max as f64) as usize
}

pub use astrcode_ext_common::config::{ConfigStore, EnsureOutcome, LoadResult, PluginConfig};

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
    ///
    /// 解析走 `common::json`（simd-json）：配置是插件自己拥有的字节缓冲，
    /// 直接吃掉刚读进来的 `Vec<u8>`，不必先转成 `String` 再解析。
    fn decode(bytes: Vec<u8>) -> Result<Self, String> {
        let raw: Value =
            astrcode_ext_common::json::parse_owned(bytes).map_err(|error| error.to_string())?;
        Ok(Self::normalize(&raw))
    }

    /// 归一化后渲染成落盘正文。键序由 [`Config::to_json_text`] 手写，以对齐上游的输出。
    fn render(&self) -> Result<String, String> {
        Ok(self.normalized()?.to_json_text())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-rtk-config-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join(CONFIG_FILE_NAME)
    }

    #[test]
    fn defaults_match_upstream() {
        let config = Config::default();
        assert!(config.enabled);
        assert_eq!(config.mode, Mode::Rewrite);
        assert!(config.guard_when_rtk_missing);
        assert!(config.show_rewrite_notifications);

        let compaction = &config.output_compaction;
        assert!(compaction.enabled);
        assert!(compaction.strip_ansi);
        assert!(!compaction.read_compaction.enabled);
        assert!(!compaction.source_code_filtering_enabled);
        assert!(!compaction.preserve_exact_skill_reads);
        assert_eq!(compaction.source_code_filtering, SourceFilterLevel::None);
        assert!(compaction.truncate.enabled);
        assert_eq!(compaction.truncate.max_chars, 12_000);
        assert!(!compaction.smart_truncate.enabled);
        assert_eq!(compaction.smart_truncate.max_lines, 220);
        assert!(compaction.aggregate_test_output);
        assert!(compaction.filter_build_output);
        assert!(compaction.compact_git_output);
        assert!(compaction.aggregate_linter_output);
        assert!(compaction.group_search_output);
        assert!(compaction.track_savings);
    }

    /// 空对象与缺 `outputCompaction` 的配置都会走 legacy 分支（上游 `hasOwnProperty` 的
    /// 直接后果），因此拿到的不是 `Config::default()`。全新安装由 `ensure_exists` 先写入
    /// 完整默认文件，所以这条路径只对「已存在但不含新键」的旧配置生效。
    #[test]
    fn a_missing_output_compaction_section_falls_back_to_legacy_defaults() {
        let legacy = Config::normalize(&json!({}));
        assert!(legacy.output_compaction.read_compaction.enabled);
        assert!(legacy.output_compaction.source_code_filtering_enabled);
        assert_eq!(
            legacy.output_compaction.source_code_filtering,
            SourceFilterLevel::Minimal
        );
        assert!(legacy.output_compaction.smart_truncate.enabled);

        // 其余字段仍是新默认值。
        assert!(legacy.enabled);
        assert_eq!(legacy.mode, Mode::Rewrite);
        assert_eq!(legacy.output_compaction.truncate.max_chars, 12_000);
        assert!(!legacy.output_compaction.preserve_exact_skill_reads);
    }

    #[test]
    fn non_object_roots_are_treated_as_an_empty_config() {
        let expected = Config::normalize(&json!({}));
        for raw in [json!(null), json!([]), json!("nope"), json!(7)] {
            assert_eq!(Config::normalize(&raw), expected, "raw = {raw}");
        }
    }

    #[test]
    fn normalize_ignores_wrongly_typed_values() {
        let config = Config::normalize(&json!({
            "enabled": "yes",
            "mode": "auto",
            "guardWhenRtkMissing": 1,
            "outputCompaction": {
                "enabled": null,
                "stripAnsi": "on",
                "truncate": { "enabled": "yes", "maxChars": "lots" },
                "sourceCodeFiltering": "extreme",
                "smartTruncate": { "enabled": "yes", "maxLines": [] }
            }
        }));

        // `outputCompaction.readCompaction` 缺失会走 legacy 分支，因此这几项取旧默认值；
        // 其余字段仍应各自回落。
        assert!(config.enabled);
        assert_eq!(config.mode, Mode::Rewrite);
        assert!(config.guard_when_rtk_missing);
        assert!(config.output_compaction.enabled);
        assert!(config.output_compaction.strip_ansi);
        assert!(config.output_compaction.truncate.enabled);
        assert_eq!(config.output_compaction.truncate.max_chars, 12_000);
        assert_eq!(
            config.output_compaction.source_code_filtering,
            SourceFilterLevel::Minimal
        );
        assert!(config.output_compaction.smart_truncate.enabled);
        assert_eq!(config.output_compaction.smart_truncate.max_lines, 220);
    }

    #[test]
    fn normalize_clamps_and_rounds_numeric_ranges() {
        let config = Config::normalize(&json!({
            "outputCompaction": {
                "readCompaction": { "enabled": false },
                "truncate": { "maxChars": 999 },
                "smartTruncate": { "maxLines": 12_345 }
            }
        }));
        assert_eq!(config.output_compaction.truncate.max_chars, 1_000);
        assert_eq!(config.output_compaction.smart_truncate.max_lines, 4_000);

        let rounded = Config::normalize(&json!({
            "outputCompaction": {
                "readCompaction": {},
                "truncate": { "maxChars": 12_345.6 },
                "smartTruncate": { "maxLines": 220.4 }
            }
        }));
        assert_eq!(rounded.output_compaction.truncate.max_chars, 12_346);
        assert_eq!(rounded.output_compaction.smart_truncate.max_lines, 220);
    }

    /// 旧版配置没有 `readCompaction` 键，那时 `read` 压缩、源码过滤与 smart truncate
    /// 都是默认开启的。升级插件不该让行为突变。
    #[test]
    fn legacy_config_without_read_compaction_falls_back_to_old_defaults() {
        let config = Config::normalize(&json!({
            "outputCompaction": { "enabled": true }
        }));
        assert!(config.output_compaction.read_compaction.enabled);
        assert!(config.output_compaction.source_code_filtering_enabled);
        assert_eq!(
            config.output_compaction.source_code_filtering,
            SourceFilterLevel::Minimal
        );
        assert!(config.output_compaction.smart_truncate.enabled);
    }

    #[test]
    fn explicit_read_compaction_key_uses_new_defaults() {
        let config = Config::normalize(&json!({
            "outputCompaction": { "readCompaction": { "enabled": true } }
        }));
        assert!(config.output_compaction.read_compaction.enabled);
        assert!(!config.output_compaction.source_code_filtering_enabled);
        assert_eq!(
            config.output_compaction.source_code_filtering,
            SourceFilterLevel::None
        );
        assert!(!config.output_compaction.smart_truncate.enabled);
    }

    #[test]
    fn round_trips_through_json_text() {
        let mut config = Config {
            mode: Mode::Suggest,
            ..Config::default()
        };
        config.output_compaction.read_compaction.enabled = true;
        config.output_compaction.source_code_filtering = SourceFilterLevel::Aggressive;

        let text = config.to_json_text();
        assert!(text.ends_with('\n'));
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(Config::normalize(&parsed), config);
    }

    #[test]
    fn saved_json_uses_the_upstream_key_order_and_shape() {
        let text = Config::default().to_json_text();
        assert_eq!(
            text,
            r#"{
  "enabled": true,
  "mode": "rewrite",
  "guardWhenRtkMissing": true,
  "showRewriteNotifications": true,
  "outputCompaction": {
    "enabled": true,
    "stripAnsi": true,
    "readCompaction": {
      "enabled": false
    },
    "sourceCodeFilteringEnabled": false,
    "preserveExactSkillReads": false,
    "truncate": {
      "enabled": true,
      "maxChars": 12000
    },
    "sourceCodeFiltering": "none",
    "smartTruncate": {
      "enabled": false,
      "maxLines": 220
    },
    "aggregateTestOutput": true,
    "filterBuildOutput": true,
    "compactGitOutput": true,
    "aggregateLinterOutput": true,
    "groupSearchOutput": true,
    "trackSavings": true
  }
}
"#
        );
    }

    #[test]
    fn missing_file_loads_defaults_without_a_warning() {
        let store = store_at(temp_path("missing"));
        let result = store.load::<Config>();
        assert_eq!(result.config, Config::default());
        assert!(result.warning.is_none());
    }

    #[test]
    fn malformed_file_loads_defaults_with_a_warning() {
        let path = temp_path("malformed");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();

        let result = store_at(&path).load::<Config>();
        assert_eq!(result.config, Config::default());
        let warning = result.warning.expect("malformed config must warn");
        assert!(warning.contains("Failed to parse"), "warning = {warning}");
        assert!(warning.contains(CONFIG_FILE_NAME));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_then_load_round_trips_and_creates_parent_directories() {
        let path = temp_path("round-trip");
        let store = store_at(&path);

        let mut config = Config::default();
        config.output_compaction.group_search_output = false;
        store.save(&config).unwrap();

        assert_eq!(store.load::<Config>().config, config);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
    #[test]
    fn save_normalizes_before_writing() {
        let path = temp_path("normalize-on-save");
        let store = store_at(&path);

        let mut config = Config::default();
        config.output_compaction.truncate.max_chars = 5;
        store.save(&config).unwrap();

        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["outputCompaction"]["truncate"]["maxChars"], 1_000);

        // 落盘的是夹取后的值，读回来也应该是一致的。
        let loaded = store.load::<Config>().config;
        assert_eq!(loaded.output_compaction.truncate.max_chars, 1_000);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// 落盘的默认文件能原样读回，不会因为 legacy 分支而漂移。
    #[test]
    fn the_written_default_file_loads_back_as_defaults() {
        let path = temp_path("default-round-trip");
        let store = store_at(&path);
        assert_eq!(store.ensure_exists::<Config>(), EnsureOutcome::Created);
        assert_eq!(store.load::<Config>().config, Config::default());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ensure_exists_reports_creation_then_skips() {
        let path = temp_path("ensure");
        let store = store_at(&path);

        assert_eq!(store.ensure_exists::<Config>(), EnsureOutcome::Created);
        assert_eq!(store.ensure_exists::<Config>(), EnsureOutcome::Exists);
        assert_eq!(store.load::<Config>().config, Config::default());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ensure_exists_does_not_overwrite_user_edits() {
        let path = temp_path("ensure-keeps-edits");
        let store = store_at(&path);

        store
            .save(&Config {
                enabled: false,
                ..Config::default()
            })
            .unwrap();

        assert_eq!(store.ensure_exists::<Config>(), EnsureOutcome::Exists);
        assert!(!store.load::<Config>().config.enabled);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
