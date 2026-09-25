//! 领域提示词模块：数据表 + 静态注册表。
//!
//! 上游 `templates.ts` 用 `jiti` 动态 `require("./prompts/...")` 加载 27 个模块，
//! 编译版改成 `prompt-registry.ts` 的静态路由表。移植版是同一套结构：
//! [`corpus`] 是数据表，[`load_prompt_module`] 按 kebab 路径查表，查找顺序与原版逐位一致。
//!
//! 模块正文见 [`corpus`]；组装规则见 [`assemble`]。

pub mod assemble;
pub mod corpus;

use crate::types::{MasterTaskType, SubTaskType};

/// 单个领域提示词模块。
///
/// 字段与上游 `buildPrompt(diff, state)` 的 `switch` 一一对应：四个状态各取一段正文，
/// 其中 `design`/`execute`/`verify` 按四档难度再分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Module {
    /// 模块中文名（上游的 `LABEL`），用于 DEEP_UNDERSTAND 首行。
    pub label: &'static str,
    /// 领域要点（上游的 `HINT`），同时用于 DEEP_UNDERSTAND 正文。
    pub hint: &'static str,
    /// DEEP_UNDERSTAND 首行之后的固定正文（含起首的两个换行）。
    pub understand_tail: &'static str,
    pub design: Tiered,
    pub execute: Tiered,
    pub verify: Tiered,
}

/// 按难度分档的正文。
///
/// 上游 `switch (diff)` 把六档压成四支：`TRIVIAL` / `SIMPLE` / `HARD | EXTREME` / 默认
/// （`MODERATE | COMPLEX`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiered {
    pub trivial: &'static str,
    pub simple: &'static str,
    pub standard: &'static str,
    pub hard: &'static str,
}

/// `MasterTaskType` → kebab 段。
fn master_kebab(master: MasterTaskType) -> String {
    master.as_str().to_ascii_lowercase()
}

/// `SubTaskType` → kebab 段（下划线换连字符）。
fn sub_kebab(sub: SubTaskType) -> String {
    sub.as_str().to_ascii_lowercase().replace('_', "-")
}

/// 按 kebab 路径查模块。
fn lookup(path: &str) -> Option<&'static Module> {
    corpus::MODULES
        .iter()
        .find(|(candidate, _)| *candidate == path)
        .map(|(_, module)| *module)
}

/// 按 master/sub 查找提示词模块。
///
/// 查找顺序（与上游动态 `require` 版逐位一致）：
///
/// 1. 精确匹配：`{master}/{sub}`
/// 2. 同大类通用：`{master}/general`
/// 3. 全通用兜底：`general/general`
///
/// 当前语料里没有任何 `{master}/general`，第 2 步恒不命中；保留这条分支是为了
/// 上游补上同大类通用模块时不必改代码。
pub fn load_prompt_module(
    master: MasterTaskType,
    sub: SubTaskType,
) -> Option<&'static Module> {
    let master = master_kebab(master);

    let exact = format!("{master}/{}", sub_kebab(sub));
    if let Some(module) = lookup(&exact) {
        return Some(module);
    }

    if sub != SubTaskType::GENERAL {
        let general_sub = format!("{master}/general");
        if let Some(module) = lookup(&general_sub) {
            return Some(module);
        }
    }

    lookup("general/general")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_master_sub_pair_resolves_to_a_module() {
        for master in MasterTaskType::ALL {
            for sub in crate::types::master_to_sub(*master) {
                let module = load_prompt_module(*master, *sub)
                    .unwrap_or_else(|| panic!("{master}/{sub} 查不到模块"));
                assert!(!module.label.is_empty());
                assert!(!module.hint.is_empty());
            }
        }
    }

    /// 精确路径命中时不应落到兜底模块上。
    #[test]
    fn an_exact_path_wins_over_the_fallback() {
        let rust = load_prompt_module(MasterTaskType::CODING, SubTaskType::RUST_DEV).unwrap();
        let general = load_prompt_module(MasterTaskType::GENERAL, SubTaskType::GENERAL).unwrap();
        assert_eq!(rust.label, "编程类Rust开发");
        assert_eq!(general.label, "通用类通用");
        assert_ne!(rust, general);
    }

    /// `general/general` 是兜底：任何查不到的路径都落到它上面。
    #[test]
    fn the_fallback_module_is_always_reachable() {
        assert!(lookup("coding/nonexistent").is_none());
        // 任何一对 master/sub 都能查到模块，兜底永远不会返回 None。
        for master in MasterTaskType::ALL {
            for sub in SubTaskType::ALL {
                assert!(load_prompt_module(*master, *sub).is_some());
            }
        }
    }

    #[test]
    fn kebab_segments_follow_the_upstream_naming() {
        assert_eq!(master_kebab(MasterTaskType::ENTERTAINMENT), "entertainment");
        assert_eq!(sub_kebab(SubTaskType::RUST_DEV), "rust-dev");
        assert_eq!(sub_kebab(SubTaskType::CODE_REVIEW), "code-review");
        assert_eq!(sub_kebab(SubTaskType::CICD), "cicd");
    }

    #[test]
    fn the_corpus_holds_all_twenty_seven_modules() {
        assert_eq!(corpus::MODULES.len(), 27);
    }
}
