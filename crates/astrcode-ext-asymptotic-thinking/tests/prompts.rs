//! 逐字对照上游。
//!
//! `tests/golden/prompts.json` 与 `tests/golden/framework_rules.md` 由
//! `tools/port-prompts.mjs` **直接跑上游 TypeScript** 产出（脚本里先复制一份上游 `src`，
//! 只替换掉会在模块加载期打开 SQLite 的 `session-store.ts`），因此这份对照是
//! 「Rust 侧的组装结果 == 上游 `buildPrompt` / `buildTemplate` / `formatNextStateHint`
//! 的真实输出」，而不是手抄的期望值。
//!
//! 语料更新流程：重跑生成器 → review 产物 diff → 跑本测试。

use std::collections::BTreeSet;

use astrcode_ext_asymptotic_thinking::framework_rules::FRAMEWORK_RULES;
use astrcode_ext_asymptotic_thinking::machine::format_next_state_hint;
use astrcode_ext_asymptotic_thinking::prompts::assemble::build_prompt;
use astrcode_ext_asymptotic_thinking::prompts::load_prompt_module;
use astrcode_ext_asymptotic_thinking::templates::build_template;
use astrcode_ext_asymptotic_thinking::types::{
    Difficulty, MasterTaskType, SubTaskType, ThinkingState, master_to_sub,
};
use serde_json::Value;

const GOLDEN_JSON: &str = include_str!("golden/prompts.json");
const GOLDEN_RULES: &str = include_str!("golden/framework_rules.md");

fn golden() -> Value {
    serde_json::from_str(GOLDEN_JSON).expect("golden/prompts.json 必须可解析")
}

/// 与生成器同一套 kebab 规则。
fn kebab(master: MasterTaskType, sub: SubTaskType) -> String {
    format!(
        "{}/{}",
        master.as_str().to_ascii_lowercase(),
        sub.as_str().to_ascii_lowercase().replace('_', "-")
    )
}

/// 业务态（START / END 没有模块正文）。
const BUSINESS_STATES: [ThinkingState; 4] = [
    ThinkingState::DEEP_UNDERSTAND,
    ThinkingState::DESIGN,
    ThinkingState::EXECUTE,
    ThinkingState::VERIFY,
];

/// 27 个模块 × 6 档难度 × 4 个业务态的模块正文，逐字等于上游 `buildPrompt` 的输出。
#[test]
fn the_module_corpus_matches_upstream_verbatim() {
    let golden = golden();
    let prompts = golden["prompts"].as_object().expect("prompts 必须是对象");

    let mut checked = 0usize;
    for master in MasterTaskType::ALL {
        for sub in master_to_sub(*master) {
            let path = kebab(*master, *sub);
            let entry = prompts
                .get(&path)
                .unwrap_or_else(|| panic!("golden 里缺少模块 {path}"));
            let module = load_prompt_module(*master, *sub)
                .unwrap_or_else(|| panic!("Rust 侧查不到模块 {path}"));

            for state in BUSINESS_STATES {
                for difficulty in Difficulty::ALL {
                    let expected = entry[state.as_str()][difficulty.as_str()]
                        .as_str()
                        .unwrap_or_else(|| panic!("{path}/{state}/{difficulty} 必须是字符串"));
                    assert_eq!(
                        build_prompt(module, *difficulty, state),
                        expected,
                        "{path} 在 {state}/{difficulty} 下与上游不一致"
                    );
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 27 * 6 * 4, "覆盖的模块数不对");
}

/// START / END 以及各业务态的整块引导，逐字等于上游 `buildTemplate` 的输出。
#[test]
fn the_assembled_templates_match_upstream_verbatim() {
    let golden = golden();
    let cases = golden["templates"].as_array().expect("templates 必须是数组");
    assert!(cases.len() >= 50, "模板样本太少：{}", cases.len());

    for case in cases {
        let state = ThinkingState::parse(case["state"].as_str().expect("state"))
            .expect("state 必须是已知状态");
        let turn = case["turn"].as_u64().expect("turn") as u32;
        let master = case["master"]
            .as_str()
            .map(|raw| MasterTaskType::parse(raw).expect("master"));
        let sub = case["sub"]
            .as_str()
            .map(|raw| SubTaskType::parse(raw).expect("sub"));
        let difficulty = case["difficulty"]
            .as_str()
            .map(|raw| Difficulty::parse(raw).expect("difficulty"));
        let visited: Vec<ThinkingState> = case["visited"]
            .as_array()
            .expect("visited")
            .iter()
            .map(|value| ThinkingState::parse(value.as_str().expect("visited 项")).expect("状态"))
            .collect();
        let expected = case["output"].as_str().expect("output");

        assert_eq!(
            build_template(state, turn, master, sub, difficulty, &visited),
            expected,
            "模板 {state}/turn={turn}/{master:?}/{sub:?}/{difficulty:?}/visited={visited:?} 与上游不一致"
        );
    }
}

/// 流转提示（`本阶段完成可前移至[…]…`）逐字等于上游 `formatNextStateHint`。
#[test]
fn the_next_state_hints_match_upstream_verbatim() {
    let golden = golden();
    let cases = golden["next_state_hints"]
        .as_array()
        .expect("next_state_hints 必须是数组");
    assert_eq!(cases.len(), 7 * 7, "流转提示样本数不对");

    for case in cases {
        let from = match case["from"].as_str() {
            // 上游的 `null` 与 `"START"` 走同一分支。
            None => ThinkingState::START,
            Some(raw) => ThinkingState::parse(raw).expect("from"),
        };
        let difficulty = case["difficulty"]
            .as_str()
            .map(|raw| Difficulty::parse(raw).expect("difficulty"));
        assert_eq!(
            format_next_state_hint(from, difficulty),
            case["output"].as_str().expect("output"),
            "流转提示 {from}/{difficulty:?} 与上游不一致"
        );
    }
}

/// 静态框架规则与 golden 逐字节相等，防止无意漂移。
///
/// golden 由生成器从上游 `SYSTEM.md` 应用 `ADAPTATIONS` 得到；`src/framework_rules.md`
/// 是它的副本。两边一旦分叉，这个测试就会失败，提醒重跑生成器并 review。
#[test]
fn the_framework_rules_match_the_golden() {
    assert_eq!(FRAMEWORK_RULES, GOLDEN_RULES);
}

/// golden 里记录了对上游正文的每一处改动，且每处都确实生效。
///
/// 这是「移植没有偷偷改正文」的反向证据：`from` 必须已经不存在于任何产物里，
/// `to` 必须存在于对应产物里。
#[test]
fn every_declared_adaptation_is_applied() {
    let golden = golden();
    let adaptations = golden["adaptations"]
        .as_array()
        .expect("adaptations 必须是数组");
    assert!(!adaptations.is_empty());

    let mut haystack = String::from(FRAMEWORK_RULES);
    for entry in golden["prompts"].as_object().expect("prompts").values() {
        haystack.push_str(&entry.to_string());
    }
    for case in golden["templates"].as_array().expect("templates") {
        haystack.push_str(case["output"].as_str().expect("output"));
    }

    for entry in adaptations {
        let from = entry["from"].as_str().expect("from");
        let to = entry["to"].as_str().expect("to");
        assert!(
            haystack.contains(to),
            "改动 `{from}` → `{to}` 没有出现在任何产物里"
        );
        assert!(
            !haystack.contains(from),
            "产物里还残留着上游的 `{from}`"
        );
    }

    // 上游提到、但 AstrCode 里不存在的工具名一个都不该残留。
    for stale in ["mempal", "web_search", "web-fetch", "resources_discover"] {
        assert!(
            !haystack.contains(stale),
            "产物里还残留着上游的 `{stale}`"
        );
    }
}

/// 生成器写出来的语料表与 golden 的模块集合必须完全一致。
#[test]
fn the_golden_covers_exactly_the_corpus() {
    let golden = golden();
    let prompts = golden["prompts"].as_object().expect("prompts");

    let rust_side: BTreeSet<String> = MasterTaskType::ALL
        .iter()
        .flat_map(|master| {
            master_to_sub(*master)
                .iter()
                .map(|sub| kebab(*master, *sub))
                .collect::<Vec<_>>()
        })
        .collect();
    let golden_side: BTreeSet<String> = prompts.keys().cloned().collect();

    assert_eq!(rust_side.len(), 27);
    assert_eq!(rust_side, golden_side);
}
