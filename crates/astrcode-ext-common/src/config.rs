//! 插件自有 JSON 配置的读写生命周期。
//!
//! 三个扩展（`astrcode-ext-weneed`、`astrcode-ext-rtk-optimizer`、
//! `astrcode-ext-cache-doctor`）原本各写了一份配置读写：定位
//! `<astrcode_dir>/extension_data/<id>/config.json` → 读 → 归一化 → 原子写。
//! 这几份实现对「未知字段」的策略还分成了宽松与严格两派，其中严格的那份造成过真实
//! 缺陷：用户多加一个自用字段，已经写好的设置会被整份作废。
//!
//! 因此这里收敛**生命周期与告警策略**，把「怎么解析、怎么归一化」留给各 crate——
//! 它们的 JSON 库与字段语义不同，强行统一反而会引入行为差异。
//!
//! # 为什么不依赖宿主 SDK
//!
//! 本 crate 刻意不依赖宿主 SDK（见 crate 文档），而原子写原语
//! `hostpaths::write_file_atomic` 恰是宿主提供的。因此 [`ConfigStore`] 把写原语作为
//! 函数指针注入：`fn(&Path, &str) -> io::Result<()>` 与宿主签名完全吻合，各 crate
//! 直接传 `hostpaths::write_file_atomic` 即可，本 crate 仍是纯逻辑、可完整单元测试。
//!
//! # 不持有内存缓存
//!
//! 热路径上的缓存形态各 crate 不同（`RwLock` + 克隆、`Mutex` + `Copy`），且缓存失效
//! 时机与各自的命令语义绑定，硬塞进来只会让调用方多绕一层。本模块只管文件生命周期。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// 一次配置读取的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadResult<T> {
    /// 归一化后的配置；任何失败都回落到默认值。
    pub config: T,
    /// 读取或解析失败时的可展示警告。**文件缺失不是错误**，此时为 `None`。
    pub warning: Option<String>,
}

/// [`ConfigStore::ensure_exists`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// 文件此前不存在，已写入默认配置。
    Created,
    /// 文件已存在，未改动。
    Exists,
    /// 写入失败，附带可展示原因。
    Failed(String),
}

/// 插件自有 JSON 配置的类型契约。
///
/// 共享层只吃字节，解析与归一化由各 crate 实现：这样它们的归一化代码可以逐字保留，
/// 收敛的只是文件生命周期。
pub trait PluginConfig: Sized + Default {
    /// 解析字节并归一化。`Err` 携带可展示的原因。
    ///
    /// 两条约定，所有实现都应当遵守：
    ///
    /// 1. **残缺、类型错误的输入回落默认值**，而不是报错——用户手改坏的配置文件
    ///    不该让插件失效；
    /// 2. **未知字段被忽略**，而不是让整份配置作废——多加一个自用字段不该把已经
    ///    写好的设置一起丢掉。
    fn decode(bytes: Vec<u8>) -> Result<Self, String>;

    /// 渲染成落盘正文。实现内部负责先归一化再序列化。
    fn render(&self) -> Result<String, String>;
}

/// 一个配置文件的读写入口：路径 + 原子写原语。
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
    write: fn(&Path, &str) -> io::Result<()>,
}

impl ConfigStore {
    /// 绑定配置文件路径与原子写原语。
    ///
    /// `write` 由调用方注入 `hostpaths::write_file_atomic`，本 crate 因此不依赖宿主 SDK。
    pub fn new(path: impl Into<PathBuf>, write: fn(&Path, &str) -> io::Result<()>) -> Self {
        Self {
            path: path.into(),
            write,
        }
    }

    /// 配置文件路径，供命令展示。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取并归一化。文件缺失回落默认值且不产生警告。
    pub fn load<T: PluginConfig>(&self) -> LoadResult<T> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return LoadResult {
                    config: T::default(),
                    warning: None,
                };
            },
            Err(error) => {
                return LoadResult {
                    config: T::default(),
                    warning: Some(format!("Failed to read {}: {error}", self.path.display())),
                };
            },
        };

        match T::decode(bytes) {
            Ok(config) => LoadResult {
                config,
                warning: None,
            },
            Err(reason) => LoadResult {
                config: T::default(),
                warning: Some(format!("Failed to parse {}: {reason}", self.path.display())),
            },
        }
    }

    /// 归一化后原子落盘；父目录由写原语按需创建。
    pub fn save<T: PluginConfig>(&self, config: &T) -> io::Result<()> {
        let body = config
            .render()
            .map_err(|error| io::Error::other(format!("serialize config: {error}")))?;
        (self.write)(&self.path, &body)
    }

    /// 文件缺失时写入一份默认配置，让用户有文件可改。
    ///
    /// 返回是否真的新建了；失败返回可展示的原因（由调用方写 stderr，stdout 专用于 S5R 帧）。
    pub fn ensure_exists<T: PluginConfig>(&self) -> EnsureOutcome {
        if self.path.exists() {
            return EnsureOutcome::Exists;
        }
        match self.save(&T::default()) {
            Ok(()) => EnsureOutcome::Created,
            Err(error) => EnsureOutcome::Failed(format!(
                "Failed to create {}: {error}",
                self.path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde::Serialize;

    use super::*;
    use crate::json::{self, ValueAsScalar, ValueObjectAccess};

    /// 测试用的最小配置：归一化把 `count` 夹进 `1..=10`，缺失字段回落默认值，
    /// 未知字段被忽略。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
    struct TestConfig {
        count: u32,
        enabled: bool,
    }

    impl PluginConfig for TestConfig {
        fn decode(bytes: Vec<u8>) -> Result<Self, String> {
            let raw = json::value_owned(bytes).map_err(|error| error.to_string())?;
            Ok(Self {
                count: raw
                    .get("count")
                    .and_then(ValueAsScalar::as_u64)
                    .map_or(1, |value| value.clamp(1, 10) as u32),
                enabled: raw
                    .get("enabled")
                    .and_then(ValueAsScalar::as_bool)
                    .unwrap_or(true),
            })
        }

        fn render(&self) -> Result<String, String> {
            json::to_string_pretty(self).map_err(|error| error.to_string())
        }
    }

    /// 永远渲染失败的配置，用来覆盖序列化失败的分支。
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct Unrenderable;

    impl PluginConfig for Unrenderable {
        fn decode(_bytes: Vec<u8>) -> Result<Self, String> {
            Ok(Self)
        }

        fn render(&self) -> Result<String, String> {
            Err("nope".to_owned())
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "astrcode-ext-common-config-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join("config.json")
    }

    /// 真实落盘的写原语；宿主侧对应 `hostpaths::write_file_atomic`。
    fn write_file(path: &Path, body: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, body)
    }

    fn store(name: &str) -> (ConfigStore, PathBuf) {
        let path = temp_path(name);
        (ConfigStore::new(&path, write_file), path)
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn a_missing_file_yields_defaults_without_a_warning() {
        let (store, path) = store("missing");
        let result = store.load::<TestConfig>();
        assert_eq!(result.config, TestConfig::default());
        assert!(result.warning.is_none(), "{:?}", result.warning);
        cleanup(&path);
    }

    #[test]
    fn a_read_failure_yields_defaults_with_a_warning() {
        let (store, path) = store("read-failure");
        // 目录不是文件：`fs::read` 会以 IsADirectory 失败，且不是 NotFound。
        fs::create_dir_all(&path).expect("创建目录");
        let result = store.load::<TestConfig>();
        assert_eq!(result.config, TestConfig::default());
        assert!(
            result
                .warning
                .as_deref()
                .is_some_and(|warning| warning.starts_with("Failed to read")),
            "{:?}",
            result.warning
        );
        cleanup(&path);
    }

    #[test]
    fn a_parse_failure_yields_defaults_with_a_warning() {
        let (store, path) = store("parse-failure");
        write_file(&path, "{not json").expect("写入");
        let result = store.load::<TestConfig>();
        assert_eq!(result.config, TestConfig::default());
        assert!(
            result
                .warning
                .as_deref()
                .is_some_and(|warning| warning.starts_with("Failed to parse")),
            "{:?}",
            result.warning
        );
        cleanup(&path);
    }

    #[test]
    fn a_valid_file_is_normalized_and_unknown_fields_are_ignored() {
        let (store, path) = store("valid");
        write_file(&path, r#"{"count": 99, "surprise": 1}"#).expect("写入");
        let result = store.load::<TestConfig>();
        assert_eq!(result.config.count, 10, "越界字段被夹回范围");
        assert!(result.config.enabled, "缺失字段回落默认值");
        assert!(result.warning.is_none(), "未知字段不产生警告");
        cleanup(&path);
    }

    #[test]
    fn save_round_trips_through_disk() {
        let (store, path) = store("round-trip");
        store
            .save(&TestConfig {
                count: 7,
                enabled: false,
            })
            .expect("写入应成功");

        let reloaded = store.load::<TestConfig>();
        assert_eq!(reloaded.config.count, 7);
        assert!(!reloaded.config.enabled);
        assert!(reloaded.warning.is_none());
        cleanup(&path);
    }

    #[test]
    fn a_render_failure_becomes_an_io_error() {
        let (store, path) = store("render-failure");
        let error = store.save(&Unrenderable).expect_err("应当失败");
        assert!(error.to_string().contains("serialize config"), "{error}");
        cleanup(&path);
    }

    #[test]
    fn ensure_exists_creates_once_then_reports_the_file_as_present() {
        let (store, path) = store("ensure-exists");
        assert_eq!(store.ensure_exists::<TestConfig>(), EnsureOutcome::Created);
        assert_eq!(store.ensure_exists::<TestConfig>(), EnsureOutcome::Exists);
        assert!(path.exists());
        cleanup(&path);
    }

    #[test]
    fn ensure_exists_does_not_overwrite_user_edits() {
        let (store, path) = store("ensure-exists-keeps-edits");
        write_file(&path, r#"{"count": 4}"#).expect("写入");

        assert_eq!(store.ensure_exists::<TestConfig>(), EnsureOutcome::Exists);
        assert_eq!(store.load::<TestConfig>().config.count, 4);
        cleanup(&path);
    }

    #[test]
    fn ensure_exists_reports_a_failure_with_a_readable_reason() {
        let (_, path) = store("ensure-exists-failure");
        // 父路径是一个普通文件，因此 `config.json` 既不存在也写不进去。
        write_file(&path, "placeholder").expect("写入占位文件");
        let blocker = path.clone();
        fs::remove_file(&blocker).expect("移除");
        fs::create_dir_all(blocker.parent().expect("有父目录")).expect("创建目录");
        fs::write(&blocker, "not a directory").expect("写入占位文件");

        let outcome = ConfigStore::new(blocker.join("config.json"), write_file)
            .ensure_exists::<TestConfig>();
        match outcome {
            EnsureOutcome::Failed(reason) => {
                assert!(reason.starts_with("Failed to create"), "{reason}");
            },
            other => panic!("应当报告失败，实际是 {other:?}"),
        }
        cleanup(&path);
    }
}
