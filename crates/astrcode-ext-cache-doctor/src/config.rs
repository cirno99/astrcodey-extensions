//! 插件自有配置：观测开关与快照里保留的对比条数。
//!
//! # 为什么不用宿主的 session state
//!
//! 配置是**插件级**的，不是会话级的；而且 `before_provider_request` 是每请求都跑的
//! 热路径，读会话状态意味着每请求一次 IPC。配置因此落在插件自己的数据目录里，
//! 进程启动时读一次，之后只读内存。
//!
//! ```text
//! ~/.astrcode/extension_data/astrcode-cache-doctor/config.json
//! ```
//!
//! # 读取是宽松的
//!
//! 配置读不到、解析失败或字段越界时一律回落默认值：诊断插件不该因为一个坏掉的配置
//! 文件而让 provider 请求路径上的钩子报错。**未知字段被忽略**，而不是让整份配置作废——
//! 与 `astrcode-ext-weneed` / `astrcode-ext-rtk-optimizer` 的策略一致：用户多加一个自用
//! 字段，不该把已经写好的设置一起丢掉。写入则如实上抛，让用户知道没落盘。

use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use astrcode_ext_common::config::{ConfigStore as FileStore, PluginConfig};
use astrcode_ext_common::paths::extension_data_dir;
use astrcode_extension_sdk::hostpaths;
use serde::{Deserialize, Serialize};

/// 配置文件名。
const CONFIG_FILE_NAME: &str = "config.json";

/// 当前配置结构版本。
const CONFIG_SCHEMA: u32 = 1;

/// 默认保留的最近对比条数。
pub const DEFAULT_HISTORY: usize = 20;

/// `history` 的上限：快照只用于展示，没有理由无限增长。
pub const MAX_HISTORY: usize = 200;

/// 插件配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// 配置结构版本，便于以后迁移。
    #[serde(default = "default_schema")]
    pub schema: u32,
    /// 是否观测 provider 请求。关闭后钩子立即放行，不做任何指纹计算。
    #[serde(default = "default_watch")]
    pub watch: bool,
    /// 快照里保留的最近对比条数。
    #[serde(default = "default_history")]
    pub history: usize,
}

fn default_schema() -> u32 {
    CONFIG_SCHEMA
}

fn default_watch() -> bool {
    true
}

fn default_history() -> usize {
    DEFAULT_HISTORY
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: CONFIG_SCHEMA,
            watch: default_watch(),
            history: default_history(),
        }
    }
}

impl Config {
    /// 把越界字段夹回合法范围。`history` 为 0 会退化成 1，避免快照永远为空。
    pub fn normalized(mut self) -> Self {
        self.schema = CONFIG_SCHEMA;
        self.history = self.history.clamp(1, MAX_HISTORY);
        self
    }
}

/// 配置的读写入口。
///
/// `current` 是内存里的权威副本：热路径只读它，`set` 先更新它再落盘。
#[derive(Debug)]
pub struct ConfigStore {
    /// 文件生命周期（读 / 归一化 / 原子写）由共享实现承担。
    file: FileStore,
    /// 内存里的权威副本：热路径只读它，`set` 先更新它再落盘。
    current: Mutex<Config>,
}

impl ConfigStore {
    /// 按宿主布局定位插件配置：`<astrcode_dir>/extension_data/<extension_id>/config.json`。
    pub fn for_extension(extension_id: &str) -> Self {
        let path =
            extension_data_dir(hostpaths::astrcode_dir(), extension_id).join(CONFIG_FILE_NAME);
        Self::at(path)
    }

    /// 在显式路径上打开配置；测试与工具函数用它避开真实用户目录。
    ///
    /// 构造即读盘（宽松解析），因此热路径上不再有文件 IO。
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let file = FileStore::new(path, hostpaths::write_file_atomic);
        let current = Mutex::new(file.load::<Config>().config);
        Self { file, current }
    }

    /// 当前配置。锁中毒时返回默认值而不是 panic。
    pub fn get(&self) -> Config {
        self.current
            .lock()
            .map(|config| *config)
            .unwrap_or_default()
    }

    /// 写入配置：先更新内存，再原子落盘。
    pub fn set(&self, config: Config) -> io::Result<()> {
        let config = config.normalized();
        if let Ok(mut current) = self.current.lock() {
            *current = config;
        }
        self.file.save(&config)
    }

    /// 配置文件路径，供 `/cache-doctor config` 展示。
    pub fn path(&self) -> &Path {
        self.file.path()
    }
}

impl PluginConfig for Config {
    /// 解析并归一化。未知字段被忽略——用户多加一个自用字段，不该把已经写好的设置一起丢掉。
    fn decode(bytes: Vec<u8>) -> Result<Self, String> {
        let config: Config =
            astrcode_ext_common::json::parse_owned(bytes).map_err(|error| error.to_string())?;
        Ok(config.normalized())
    }

    /// 归一化后渲染成落盘正文。
    fn render(&self) -> Result<String, String> {
        astrcode_ext_common::json::to_string_pretty(&self.normalized())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-cache-doctor-config-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join(CONFIG_FILE_NAME)
    }

    #[test]
    fn defaults_watch_and_bound_the_history() {
        let config = Config::default();
        assert!(config.watch);
        assert_eq!(config.history, DEFAULT_HISTORY);
        assert_eq!(config.schema, CONFIG_SCHEMA);
    }

    #[test]
    fn normalization_clamps_the_history_into_range() {
        assert_eq!(
            Config {
                history: 0,
                ..Config::default()
            }
            .normalized()
            .history,
            1
        );
        assert_eq!(
            Config {
                history: usize::MAX,
                ..Config::default()
            }
            .normalized()
            .history,
            MAX_HISTORY
        );
    }

    #[test]
    fn a_missing_file_yields_defaults() {
        let store = ConfigStore::at(temp_path("missing"));
        assert_eq!(store.get(), Config::default());
    }

    #[test]
    fn a_corrupt_file_yields_defaults_instead_of_failing() {
        let path = temp_path("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not json").unwrap();

        let store = ConfigStore::at(&path);
        assert_eq!(store.get(), Config::default());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let path = temp_path("unknown-field");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"watch": false, "surprise": 1}"#).unwrap();

        let store = ConfigStore::at(&path);
        let config = store.get();
        assert!(!config.watch, "已知字段必须生效");
        assert_eq!(config.history, DEFAULT_HISTORY, "缺失字段回落默认值");
        assert_eq!(config.schema, CONFIG_SCHEMA);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn writes_round_trip_through_disk() {
        let path = temp_path("round-trip");
        let store = ConfigStore::at(&path);
        store
            .set(Config {
                watch: false,
                history: 3,
                ..Config::default()
            })
            .expect("写入应成功");

        assert_eq!(store.get().history, 3);
        assert!(!store.get().watch);

        // 新实例（模拟扩展重载）应读到落盘的值。
        let reloaded = ConfigStore::at(&path);
        assert_eq!(reloaded.get().history, 3);
        assert!(!reloaded.get().watch);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_write_normalizes_before_persisting() {
        let path = temp_path("normalize-on-write");
        let store = ConfigStore::at(&path);
        store
            .set(Config {
                history: MAX_HISTORY * 10,
                ..Config::default()
            })
            .expect("写入应成功");

        assert_eq!(store.get().history, MAX_HISTORY);
        assert_eq!(ConfigStore::at(&path).get().history, MAX_HISTORY);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_default_location_follows_the_host_layout() {
        let store = ConfigStore::for_extension("astrcode-cache-doctor");
        assert!(
            store.path().ends_with("extension_data/astrcode-cache-doctor/config.json"),
            "意外的配置路径：{:?}",
            store.path()
        );
    }
}
