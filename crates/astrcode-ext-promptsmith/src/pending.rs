//! 待发送草稿的按会话暂存。
//!
//! # 为什么要有这一层
//!
//! 用户要的是「默认只预览，可复制或留步骤给自己编辑后再发」。改写出来的正文若不落地，
//! `/smith go` 就只能让用户从渲染后的气泡里手工复制——中文与代码混排时还会丢格式。因此把
//! 正文按会话存进宿主的 `session_state`（与本插件配置分属不同介质：配置是**全局**文件，
//! 这个是宿主按「扩展 × 会话」命名空间存的**会话级**状态，重载宿主后仍在）。
//!
//! # 为什么不清除
//!
//! `/smith go` 发送后**保留**暂存：发送失败（上游 5xx、上下文压缩重试）时用户可以再 `/smith go`
//! 一次；想改口重打也随时能。代价只是一段旧正文留在会话状态里，下一次 `/smith <草稿>` 会覆盖它。

use astrcode_extension_worker::worker_prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    config::Config,
    enhance::Outcome,
};

/// `session_state` 的键名。只允许 ASCII 字母数字与 `-`、`_`、`.`。
const STATE_KEY: &str = "pending";

/// 一条暂存的改写结果。
///
/// 意图/模式/档位/提取形态都存**字符串**而不是枚举：这里只需要把它们如实显示给用户，
/// 不参与任何判定，因此存字符串既避免给 [`crate::intent`] 的枚举加 serde 依赖，也让旧版本
/// 写入的值在新版本里读得回来（不认识就照原样显示）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// 清洗后的正文，`/smith go` 原样作为用户消息发出。
    pub prompt: String,
    /// 改写前的草稿，供 `/smith show` 对照。
    pub draft: String,
    pub intent: String,
    pub mode: String,
    pub strength: String,
    /// 实际使用的档位。
    pub enhancer: String,
    /// 是否发生了 small → main 回落。
    pub enhancer_fell_back: bool,
    pub extraction: String,
    /// 改写时面向的激活模型 id。模型换了就说明这份正文可能不再贴合，预览里提示重打。
    pub target_model: String,
}

impl Pending {
    pub fn from_outcome(
        draft: &str,
        outcome: &Outcome,
        config: &Config,
        target_model: &str,
    ) -> Self {
        Self {
            prompt: outcome.prompt.clone(),
            draft: draft.to_owned(),
            intent: outcome.intent.as_str().to_owned(),
            mode: outcome.mode.as_str().to_owned(),
            strength: config.strength.as_str().to_owned(),
            enhancer: outcome.enhancer_used.to_owned(),
            enhancer_fell_back: outcome.enhancer_fell_back,
            extraction: outcome.extraction.as_str().to_owned(),
            target_model: target_model.to_owned(),
        }
    }

    /// 预览里标注这份正文是不是为当前模型改写的。
    pub fn targets(&self, model_id: &str) -> bool {
        self.target_model == model_id
    }
}

/// 写入（覆盖）本会话的暂存。
pub async fn save(pending: &Pending) -> Result<(), ErrorPayload> {
    HostClient::session_state()
        .write(HostSessionStateWriteRequest {
            key: STATE_KEY.to_owned(),
            content: serde_json::to_string(pending).unwrap_or_default(),
        })
        .await
}

/// 读取本会话的暂存。
///
/// 读失败与「没有暂存」合并成 [`Option::None`]：预览取不到旧草稿不是错误，不该因此打断用户；
/// 真正需要区分的只有「有没有正文可发」，两者答案都是「没有」。
pub async fn load() -> Option<Pending> {
    let output = HostClient::session_state()
        .read(HostSessionStateReadRequest {
            key: STATE_KEY.to_owned(),
        })
        .await
        .ok()?;
    let content = output.content?;
    if content.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&content).ok()
}

/// 清空暂存。
pub async fn clear() -> Result<(), ErrorPayload> {
    HostClient::session_state()
        .write(HostSessionStateWriteRequest {
            key: STATE_KEY.to_owned(),
            content: String::new(),
        })
        .await
}

#[cfg(test)]
mod tests {
    use crate::enhance::Extraction;
    use crate::intent::{EffectiveMode, TaskIntent};

    use super::*;

    fn outcome() -> Outcome {
        Outcome {
            prompt: "目标：把开关做成会话级".to_owned(),
            intent: TaskIntent::Implement,
            mode: EffectiveMode::ExecutionContract,
            enhancer_used: "small",
            enhancer_fell_back: true,
            extraction: Extraction::Sentinel,
        }
    }

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
    fn pending_flattens_the_outcome_into_display_strings() {
        let config = Config::default();
        let pending = Pending::from_outcome("加个开关", &outcome(), &config, "glm-5.3-flash");
        assert_eq!(pending.prompt, "目标：把开关做成会话级");
        assert_eq!(pending.draft, "加个开关");
        assert_eq!(pending.intent, "implement");
        assert_eq!(pending.mode, "execution-contract");
        assert_eq!(pending.strength, "balanced");
        assert_eq!(pending.enhancer, "small");
        assert!(pending.enhancer_fell_back);
        assert_eq!(pending.extraction, "sentinel");
        assert_eq!(pending.target_model, "glm-5.3-flash");
    }

    #[test]
    fn round_trips_through_json() {
        let pending = Pending::from_outcome("d", &outcome(), &Config::default(), "qwen-3.8-flash");
        let encoded = serde_json::to_string(&pending).unwrap();
        let decoded: Pending = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, pending);
    }

    /// 旧版本或多写的字段不该让整条暂存读不回来——`load` 用 `from_str(..).ok()`，这里钉住
    /// 「缺字段读不回」而不是「多余字段读不回」：多余的键必须被忽略。
    #[test]
    fn unknown_extra_fields_are_tolerated() {
        let encoded = serde_json::json!({
            "prompt": "p", "draft": "d", "intent": "general", "mode": "plain",
            "strength": "balanced", "enhancer": "small", "enhancer_fell_back": false,
            "extraction": "sentinel", "target_model": "m", "from_the_future": 1,
        })
        .to_string();
        let decoded: Pending = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.prompt, "p");
    }

    #[test]
    fn target_model_match_is_case_sensitive_on_purpose() {
        let pending = Pending::from_outcome("d", &outcome(), &Config::default(), "GLM-5.3-Flash");
        assert!(pending.targets("GLM-5.3-Flash"));
        assert!(!pending.targets("glm-5.3-flash"));
    }
}
