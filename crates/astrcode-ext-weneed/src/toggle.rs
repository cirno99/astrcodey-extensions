//! 每会话开关：内存缓存 + `session_state` 持久化。
//!
//! 开关状态由宿主按「扩展 × 会话」命名空间保存，因此扩展重载或宿主重启后仍然有效。
//! 读路径带内存缓存：`prompt_build` 每轮都会触发，缓存命中时不再产生宿主往返。
//!
//! 开关是**三态**而不是布尔：`/weneed off` 写入的是显式关闭，必须与「从未设置过」区分开，
//! 后者才回落到全局配置（`config.enabled`）。全局关是硬闸门——会话开关只能在全局开启时
//! 进一步关闭本会话，不能反向打开。

use std::collections::HashMap;
use std::sync::Mutex;

use astrcode_extension_worker::worker_prelude::*;

/// `session_state` 中的键名。键只允许 ASCII 字母数字与 `-`、`_`、`.`。
const STATE_KEY: &str = "weneed";
const STATE_ON: &str = "on";
const STATE_OFF: &str = "off";

/// 会话级开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSwitch {
    /// 从未设置过：跟随全局配置。
    #[default]
    Unset,
    /// `/weneed`：开启本会话（覆盖此前写入的关闭）。
    On,
    /// `/weneed off`：关闭本会话。
    Off,
}

impl SessionSwitch {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// 会话是否允许注入。
    ///
    /// 全局关是硬闸门：只有 `Off` 能额外关掉一个全局已开启的会话。
    pub const fn allows(self, global_enabled: bool) -> bool {
        global_enabled && !matches!(self, Self::Off)
    }

    fn parse(value: Option<&str>) -> Self {
        match value {
            Some(STATE_ON) => Self::On,
            Some(STATE_OFF) => Self::Off,
            _ => Self::Unset,
        }
    }
}

/// 各会话的注入开关。
#[derive(Default)]
pub struct Toggles {
    cache: Mutex<HashMap<String, SessionSwitch>>,
}

impl Toggles {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取会话开关。首次访问该会话时从 `session_state` 载入并缓存。
    ///
    /// 载入失败回落 [`SessionSwitch::Unset`] 而非报错：读不到开关状态不该阻断本轮 prompt 组装。
    pub async fn switch(&self, session_id: &str) -> SessionSwitch {
        if let Some(cached) = self.cached(session_id) {
            return cached;
        }
        let loaded = self.load().await.unwrap_or_default();
        self.cache_value(session_id, loaded);
        loaded
    }

    /// 写入会话开关并持久化。
    ///
    /// 内存缓存先更新，因此即使持久化失败，本次会话内的开关也立即生效；
    /// 错误仍然上抛，让调用方把持久化失败如实告诉用户。
    pub async fn set(&self, session_id: &str, switch: SessionSwitch) -> Result<(), ErrorPayload> {
        self.cache_value(session_id, switch);
        HostClient::session_state()
            .write(HostSessionStateWriteRequest {
                key: STATE_KEY.to_owned(),
                // `Unset` 写空串而不是提前返回：读路径把「文件缺失」与「空串」都解析成
                // `Unset`，写空串因此等价于清除，且让宿主的落盘状态与内存缓存保持一致。
                content: match switch {
                    SessionSwitch::Unset => String::new(),
                    SessionSwitch::On => STATE_ON.to_owned(),
                    SessionSwitch::Off => STATE_OFF.to_owned(),
                },
            })
            .await
    }

    /// 测试专用：直接写内存缓存，绕开宿主往返。
    ///
    /// 跨进程的 `session_state` 往返由集成测试（`tests/injection.rs`）用模拟宿主覆盖；
    /// 这里是给 `src/` 内部只关心判定逻辑的单元测试用的缝隙。
    #[cfg(test)]
    pub(crate) fn cache_for_tests(&self, session_id: &str, switch: SessionSwitch) {
        self.cache_value(session_id, switch);
    }

    /// 读缓存。锁中毒说明此前有 panic 穿过临界区，此时退化为「无缓存」而非再次 panic。
    fn cached(&self, session_id: &str) -> Option<SessionSwitch> {
        self.cache.lock().ok()?.get(session_id).copied()
    }

    fn cache_value(&self, session_id: &str, switch: SessionSwitch) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(session_id.to_owned(), switch);
        }
    }

    async fn load(&self) -> Result<SessionSwitch, ErrorPayload> {
        let output = HostClient::session_state()
            .read(HostSessionStateReadRequest {
                key: STATE_KEY.to_owned(),
            })
            .await?;
        Ok(SessionSwitch::parse(output.content.as_deref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态键必须落在宿主的键名校验内：非空、≤128 字节、仅 ASCII 字母数字与 `-_.`。
    #[test]
    fn the_state_key_passes_host_validation() {
        assert!(!STATE_KEY.is_empty());
        assert!(STATE_KEY.len() <= 128);
        assert!(
            STATE_KEY
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "键名含宿主会拒绝的字符：{STATE_KEY}"
        );
    }

    #[test]
    fn cached_values_round_trip_per_session() {
        let toggles = Toggles::new();
        assert_eq!(toggles.cached("s1"), None);

        toggles.cache_value("s1", SessionSwitch::Off);
        toggles.cache_value("s2", SessionSwitch::On);

        assert_eq!(toggles.cached("s1"), Some(SessionSwitch::Off));
        assert_eq!(toggles.cached("s2"), Some(SessionSwitch::On));
        assert_eq!(toggles.cached("s3"), None);
    }

    /// 缓存命中时不应触碰宿主；没有 host api 作用域也能返回缓存值。
    #[tokio::test]
    async fn a_cached_session_does_not_need_the_host() {
        let toggles = Toggles::new();
        toggles.cache_value("s1", SessionSwitch::Off);
        assert_eq!(toggles.switch("s1").await, SessionSwitch::Off);
    }

    /// 未设置过的会话跟随全局；显式关闭才关得掉。
    #[test]
    fn only_an_explicit_off_beats_the_global_default() {
        assert!(SessionSwitch::Unset.allows(true));
        assert!(SessionSwitch::On.allows(true));
        assert!(!SessionSwitch::Off.allows(true));
    }

    /// 全局关是硬闸门：会话开关不能反向打开。
    #[test]
    fn a_globally_disabled_plugin_stays_disabled() {
        for switch in [SessionSwitch::Unset, SessionSwitch::On, SessionSwitch::Off] {
            assert!(!switch.allows(false), "{}", switch.as_str());
        }
    }

    #[test]
    fn unknown_stored_values_read_back_as_unset() {
        assert_eq!(SessionSwitch::parse(None), SessionSwitch::Unset);
        assert_eq!(SessionSwitch::parse(Some("")), SessionSwitch::Unset);
        assert_eq!(SessionSwitch::parse(Some("yes")), SessionSwitch::Unset);
        assert_eq!(SessionSwitch::parse(Some(STATE_ON)), SessionSwitch::On);
        assert_eq!(SessionSwitch::parse(Some(STATE_OFF)), SessionSwitch::Off);
    }
}
