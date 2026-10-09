//! 任务意图分类与改写模式判定：纯本地启发式，不调模型。
//!
//! 规则表与判定顺序逐条移植 pi-promptsmith 的 `src/intent.ts`，**顺序即语义**：explain 快路径
//! 先于强意图规则，强意图规则先于「实现动词 + 代码面」的组合判定，最后才兜底到 general。
//! 调换分支顺序会改变分类结果，因此下面的判定与上游一一对应。
//!
//! 上游是纯英文正则。本仓库的提示词大量是中文，而汉字没有词边界、`\b` 对它也不成立，所以每个
//! 信号额外配了一组**子串关键词**（正则与关键词任一命中即算命中）。这是刻意的偏离，不是上游行为
//! 的子集。

use std::sync::LazyLock;

use regex::Regex;

/// 任务意图，与上游 `PromptsmithTaskIntent` 同集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskIntent {
    Implement,
    Debug,
    Refactor,
    Review,
    Research,
    Docs,
    TestFix,
    Explain,
    General,
}

impl TaskIntent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Implement => "implement",
            Self::Debug => "debug",
            Self::Refactor => "refactor",
            Self::Review => "review",
            Self::Research => "research",
            Self::Docs => "docs",
            Self::TestFix => "test-fix",
            Self::Explain => "explain",
            Self::General => "general",
        }
    }
}

/// 用户可配置的改写模式。
///
/// 序列化走 kebab-case，与 [`RewriteMode::as_str`] 共用同一套字面量：
/// 配置文件里的值与命令行的值不会两套写法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RewriteMode {
    /// 由意图决定走 plain 还是 execution-contract。
    Auto,
    Plain,
    ExecutionContract,
}

impl RewriteMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Plain => "plain",
            Self::ExecutionContract => "execution-contract",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "plain" => Some(Self::Plain),
            "execution-contract" => Some(Self::ExecutionContract),
            _ => None,
        }
    }
}

/// 本轮实际生效的改写模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveMode {
    Plain,
    ExecutionContract,
}

impl EffectiveMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::ExecutionContract => "execution-contract",
        }
    }
}

// ─── 判定信号 ──────────────────────────────────────────────────────────
//
// 每个信号一份英文正则（抄自上游）+ 一份中文关键词。上游把 execution verb 定义成
// 「IMPLEMENT_PATTERNS 加几个额外动词」的拼接，这里展开成显式列表：少一层间接，改关键词时
// 不会连带影响别的信号。

/// 一个判定信号：英文正则组 + 中文关键词组，任一命中即成立。
struct Signal {
    patterns: Vec<Regex>,
    keywords: &'static [&'static str],
}

impl Signal {
    /// 任意位置命中。
    fn hits(&self, text: &str) -> bool {
        self.patterns.iter().any(|regex| regex.is_match(text))
            || self.keywords.iter().any(|keyword| text.contains(keyword))
    }

    /// 起手命中：英文走 `^` 锚定正则，中文先剥礼貌前缀再比前缀。
    fn leads(&self, text: &str) -> bool {
        if self.patterns.iter().any(|regex| regex.is_match(text)) {
            return true;
        }
        let stripped = strip_polite(text);
        self.keywords.iter().any(|keyword| stripped.starts_with(keyword))
    }
}

fn compile(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|pattern| Regex::new(pattern).expect("intent pattern is valid"))
        .collect()
}

macro_rules! signal {
    (
        $name:ident,
        en = [$($en_pat:literal),* $(,)?],
        zh = [$($zh_kw:literal),* $(,)?] $(,)?
    ) => {
        static $name: LazyLock<Signal> = LazyLock::new(|| Signal {
            patterns: compile(&[$($en_pat),*]),
            keywords: &[$($zh_kw),*],
        });
    };
}

signal!(REVIEW, en = [
    r"\breview\b",
    r"\baudit\b",
    r"\bfindings?\b",
    r"\blook for issues?\b",
    r"\bcode review\b",
], zh = ["审查", "评审", "检视", "找问题", "挑毛病"]);

signal!(TEST_FIX, en = [
    r"\bfailing tests?\b",
    r"\bregression tests?\b",
    r"\bupdate tests?\b",
    r"\badd tests?\b",
    r"\btest fix\b",
    r"\bfix tests?\b",
], zh = [
    "测试失败",
    "失败的测试",
    "测试跑不通",
    "修复测试",
    "补测试",
    "加测试",
    "写测试",
    "更新测试",
    "回归测试",
]);

signal!(DEBUG, en = [
    r"\bdebug\b",
    r"\bfix\b",
    r"\bbug\b",
    r"\bbroken\b",
    r"\bfails?\b",
    r"\bfailing\b",
    r"\bstuck\b",
    r"\bhangs?\b",
    r"\bcrash(?:es|ing)?\b",
    r"\berrors?\b",
    r"\broot cause\b",
], zh = [
    "调试",
    "修复",
    "报错",
    "错误",
    "失败",
    "崩溃",
    "卡住",
    "根因",
    "不工作",
    "跑不通",
    "有问题",
    "不对劲",
]);

signal!(REFACTOR, en = [
    r"\brefactor\b",
    r"\bclean\s+up\b",
    r"\bcleanup\b",
    r"\bsimplif(?:y|ication)\b",
    r"\bdedupe\b",
    r"\bdeduplicate\b",
    r"\brestructure\b",
    r"\breorganize\b",
], zh = ["重构", "清理", "简化", "去重", "合并重复", "重组", "解耦"]);

// 上游 docs 规则含 `\bdocument(?:ation)?\b`。中文只取「文档」而不取「说明」：后者与 explain
// 的「说明一下」重叠，会把大量提问误判成文档任务。
signal!(DOCS, en = [
    r"\breadme\b",
    r"\bdocs?\b",
    r"\bdocument(?:ation)?\b",
    r"\busage guide\b",
], zh = ["文档", "使用指南", "说明手册"]);

signal!(RESEARCH, en = [
    r"\bresearch\b",
    r"\blook up\b",
    r"\binvestigate\b",
    r"\bcompare\b",
    r"\bfind (?:the )?best approach\b",
    r"\bevaluate\b",
    r"\bspike\b",
], zh = ["调研", "调查", "研究", "对比", "比较", "评估", "选型", "查证"]);

signal!(EXPLAIN, en = [
    r"\bexplain\b",
    r"\bhow does this work\b",
    r"\bwhy does this\b",
    r"\bwalk me through\b",
    r"\bhelp me understand\b",
], zh = ["解释", "讲讲", "说明一下", "带我理解", "帮我理解", "原理"]);

signal!(IMPLEMENT, en = [
    r"\bimplement\b",
    r"\badd\b",
    r"\bbuild\b",
    r"\bcreate\b",
    r"\bsupport\b",
    r"\bwire up\b",
    r"\bintegrate\b",
    r"\bupdate\b",
    r"\bchange\b",
    r"\bmodify\b",
], zh = [
    "实现",
    "添加",
    "新增",
    "开发",
    "创建",
    "支持",
    "接入",
    "集成",
    "更新",
    "修改",
    "改一下",
    "改成",
    "写一个",
    "做一个",
    "补上",
    "加上",
]);

signal!(EXECUTION_VERB, en = [
    r"\bimplement\b",
    r"\badd\b",
    r"\bbuild\b",
    r"\bcreate\b",
    r"\bsupport\b",
    r"\bwire up\b",
    r"\bintegrate\b",
    r"\bupdate\b",
    r"\bchange\b",
    r"\bmodify\b",
    r"\bdebug\b",
    r"\bfix\b",
    r"\brefactor\b",
    r"\breview\b",
    r"\baudit\b",
    r"\bresearch\b",
    r"\binvestigate\b",
    r"\bdocument\b",
], zh = [
    "实现",
    "添加",
    "新增",
    "开发",
    "创建",
    "支持",
    "接入",
    "集成",
    "更新",
    "修改",
    "调试",
    "修复",
    "重构",
    "审查",
    "评审",
    "调研",
    "调查",
    "研究",
    "排查",
    "写文档",
    "更新文档",
]);

signal!(EXPLAIN_LEAD, en = [
    r"^explain\b",
    r"^please\s+explain\b",
    r"^(?:can|could|would)\s+you\s+explain\b",
    r"^why\b",
    r"^how\b",
    r"^walk me through\b",
    r"^please\s+walk me through\b",
    r"^(?:can|could|would)\s+you\s+walk me through\b",
    r"^help me understand\b",
    r"^please\s+help me understand\b",
    r"^(?:can|could|would)\s+you\s+help me understand\b",
], zh = ["解释", "讲讲", "说明一下", "带我理解", "帮我理解"]);

signal!(QUESTION_LEAD, en = [r"^why\b", r"^how\b"], zh = [
    "为什么", "为何", "怎么", "如何", "什么是", "是什么"
]);

// 「解释 + 顺带要干的事」的连接式。中文没有词边界，只能把动词列进正则的捕获分支里，
// 对应上游 EXPLAIN_WITH_ACTION_PATTERNS 的三种形态（and/then/also、以及句首动词）。
signal!(EXPLAIN_WITH_ACTION, en = [
    r"\b(?:and|then|also)\s+(?:debug|fix|implement|add|build|create|support|wire up|integrate|update|change|modify|refactor|clean\s+up|cleanup|simplify|dedupe|deduplicate|restructure|reorganize|review|audit|investigate|compare|evaluate|spike|document)\b",
    r"\b(?:and|then|also)\s+(?:run tests?|verify|check|reproduce)\b",
    r"[.!?]\s*(?:please\s+)?(?:debug|fix|implement|add|build|create|support|wire up|integrate|update|change|modify|refactor|review|audit|investigate|compare|evaluate|document|run tests?|verify|check|reproduce)\b",
    r"(?:并|并且|然后|接着|之后|再|同时|还要|另外|需要)\s*(?:帮我|请)?\s*(?:修复|修好|调试|实现|添加|新增|开发|创建|重构|清理|简化|审查|评审|调研|调查|研究|对比|评估|写文档|跑测试|验证|检查|复现|确认)",
    r"[。！？]\s*(?:请|麻烦)?\s*(?:修复|修好|调试|实现|添加|新增|开发|创建|重构|清理|简化|审查|评审|调研|调查|研究|对比|评估|写文档|跑测试|验证|检查|复现|确认)",
], zh = []);

signal!(CODE_SURFACE, en = [
    r"\brepo(?:sitory)?\b",
    r"\bcodebase\b",
    r"\bcode\b",
    r"\bfile\b",
    r"\bfunction\b",
    r"\bclass\b",
    r"\bmodule\b",
    r"\bcomponent\b",
    r"\bapi\b",
    r"\bendpoint\b",
    r"\btest(?:s)?\b",
    r"\bcommand\b",
    r"\bmodel\b",
    r"\beditor\b",
    r"\bsession\b",
    r"\bpromptsmith\b",
    r"\bsmith\b",
    r"(?:^|\s)(?:\.{1,2}\/|\/)[\w./-]+",
    r"\b[\w./-]+\.(?:ts|tsx|js|jsx|mjs|cjs|json|md|java|kt|py|go|rs|rb|php|swift|sql|yaml|yml)\b",
    r"`[^`]+`",
    r"\b(?:pnpm|npm|yarn|bun|pytest|vitest|jest|cargo|mvn|gradle|go test|git)\b",
], zh = [
    "仓库",
    "代码",
    "代码库",
    "文件",
    "函数",
    "模块",
    "组件",
    "接口",
    "端点",
    "测试",
    "命令",
    "模型",
    "脚本",
    "插件",
    "扩展",
    "工具",
    "配置",
]);

signal!(VERIFICATION, en = [
    r"\bverify\b",
    r"\bcheck\b",
    r"\brun tests?\b",
    r"\btest(?:s)?\b",
    r"\blint\b",
    r"\bbuild\b",
    r"\breproduce\b",
    r"\bconfirm\b",
    r"\bregression\b",
], zh = [
    "验证",
    "检查",
    "跑测试",
    "运行测试",
    "跑一遍",
    "构建",
    "编译",
    "复现",
    "确认",
    "回归",
]);

/// 强意图规则的判定顺序，与上游 `STRONG_INTENT_RULES` 一致。
const STRONG_ORDER: [(TaskIntent, &LazyLock<Signal>); 6] = [
    (TaskIntent::Review, &REVIEW),
    (TaskIntent::TestFix, &TEST_FIX),
    (TaskIntent::Debug, &DEBUG),
    (TaskIntent::Refactor, &REFACTOR),
    (TaskIntent::Docs, &DOCS),
    (TaskIntent::Research, &RESEARCH),
];

/// 判定草稿的任务意图。
pub fn detect(draft: &str) -> TaskIntent {
    let text = normalize(draft);
    if text.is_empty() {
        return TaskIntent::General;
    }

    let starts_as_explanation = EXPLAIN_LEAD.leads(&text);
    let starts_with_question_lead = QUESTION_LEAD.leads(&text);
    let requests_operational_action = EXPLAIN_WITH_ACTION.hits(&text);
    let has_code_surface = CODE_SURFACE.hits(&text);
    let has_verification_signal = VERIFICATION.hits(&text);
    let has_execution_verb = EXECUTION_VERB.hits(&text);
    let has_implement_verb = IMPLEMENT.hits(&text);

    // why/how 起手不走 explain 快路径：让它落到强规则里，这样「为什么测试跑不通」能被 debug
    // 接走。上游在这一点上有专门注释说明是刻意的。
    if starts_as_explanation && !requests_operational_action && !starts_with_question_lead {
        return TaskIntent::Explain;
    }

    for (intent, signal) in STRONG_ORDER {
        if signal.hits(&text) {
            return intent;
        }
    }

    if starts_as_explanation
        && !requests_operational_action
        && !(has_implement_verb && (has_code_surface || has_verification_signal))
        && !(has_execution_verb && has_code_surface)
    {
        return TaskIntent::Explain;
    }

    if has_implement_verb && (has_code_surface || has_verification_signal) {
        return TaskIntent::Implement;
    }

    if has_execution_verb && has_code_surface {
        return TaskIntent::Implement;
    }

    if has_code_surface && has_verification_signal {
        return TaskIntent::Implement;
    }

    if EXPLAIN.hits(&text) {
        return TaskIntent::Explain;
    }

    TaskIntent::General
}

/// 由配置模式与意图决定本轮实际生效的改写模式（上游 `resolveEffectiveRewriteMode`）。
pub const fn resolve_effective_mode(configured: RewriteMode, intent: TaskIntent) -> EffectiveMode {
    match configured {
        RewriteMode::Plain => EffectiveMode::Plain,
        RewriteMode::ExecutionContract => EffectiveMode::ExecutionContract,
        RewriteMode::Auto if wants_contract(intent) => EffectiveMode::ExecutionContract,
        RewriteMode::Auto => EffectiveMode::Plain,
    }
}

/// 意图是否值得编译成执行契约（上游 `EXECUTION_CONTRACT_INTENTS`）。
pub const fn wants_contract(intent: TaskIntent) -> bool {
    matches!(
        intent,
        TaskIntent::Implement
            | TaskIntent::Debug
            | TaskIntent::Refactor
            | TaskIntent::Review
            | TaskIntent::Research
            | TaskIntent::Docs
            | TaskIntent::TestFix
    )
}

/// 归一化：统一换行、压平空白、去首尾、转小写。
fn normalize(draft: &str) -> String {
    let mut result = String::with_capacity(draft.len());
    let mut previous_was_space = false;
    for ch in draft.chars() {
        if ch == '\r' {
            continue;
        }
        if ch.is_whitespace() {
            if !previous_was_space && !result.is_empty() {
                result.push(' ');
            }
            previous_was_space = true;
            continue;
        }
        previous_was_space = false;
        result.push(ch);
    }
    result.trim().to_lowercase()
}

/// 剥掉中文礼貌前缀，让「请帮我解释…」也能命中起手判定。循环剥直到剥不动为止。
fn strip_polite(text: &str) -> &str {
    const PREFIXES: &[&str] = &["请问", "请帮我", "请", "麻烦", "帮我"];
    let mut rest = text.trim_start();
    let mut changed = true;
    while changed {
        changed = false;
        for prefix in PREFIXES {
            if let Some(stripped) = rest.strip_prefix(prefix) {
                rest = stripped.trim_start();
                changed = true;
                break;
            }
        }
    }
    rest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_only_drafts_are_general() {
        assert_eq!(detect(""), TaskIntent::General);
        assert_eq!(detect("   \n\t "), TaskIntent::General);
    }

    #[test]
    fn english_implement_and_debug_and_refactor_are_classified() {
        assert_eq!(
            detect("add rewrite mode support for promptsmith"),
            TaskIntent::Implement
        );
        assert_eq!(detect("debug the failing session startup"), TaskIntent::Debug);
        assert_eq!(detect("refactor the editor component"), TaskIntent::Refactor);
    }

    #[test]
    fn strong_rules_beat_implement_combination() {
        // 上游顺序：强规则先于 implement 组合判定。「fix 并 add 开关」同时含两类动词，
        // 必须先落到 debug。
        assert_eq!(detect("fix the toggle and add a status line"), TaskIntent::Debug);
    }

    #[test]
    fn question_lead_falls_through_to_strong_rules() {
        assert_eq!(detect("why does the build fail here"), TaskIntent::Debug);
        assert_eq!(
            detect("why does this function exist"),
            TaskIntent::Explain,
            "没有操作动词时仍应是 explain"
        );
    }

    #[test]
    fn explanation_with_operational_action_is_not_explain() {
        assert_eq!(
            detect("explain how the cache works and fix the stale entry bug"),
            TaskIntent::Debug,
        );
    }

    #[test]
    fn chinese_intents_are_classified() {
        assert_eq!(detect("实现一下插件的续跑开关"), TaskIntent::Implement);
        assert_eq!(detect("修复登录接口报 500 的问题"), TaskIntent::Debug);
        assert_eq!(detect("重构这个模块，把重复逻辑去掉"), TaskIntent::Refactor);
        assert_eq!(detect("评审一下这个改动的错误处理"), TaskIntent::Review);
        assert_eq!(detect("补测试，覆盖空配置的分支"), TaskIntent::TestFix);
        assert_eq!(detect("更新 readme 文档里的安装步骤"), TaskIntent::Docs);
        assert_eq!(detect("调研一下几种方案的取舍"), TaskIntent::Research);
    }

    #[test]
    fn chinese_explain_lead_respects_polite_prefix() {
        assert_eq!(detect("请解释一下这个哈希锚点是怎么工作的"), TaskIntent::Explain);
        assert_eq!(detect("帮我讲讲这个插件的钩子"), TaskIntent::Explain);
    }

    /// 上游把 TestFix 排在 Debug 之前：提到「测试」的失败草稿该走 test-fix 的
    /// 「先复现、再判断是 bug 还是测试写错」指引，而不是泛化的 debug。
    #[test]
    fn chinese_why_with_test_failure_signal_is_test_fix() {
        assert_eq!(detect("为什么测试跑不通"), TaskIntent::TestFix);
        assert_eq!(detect("为什么登录接口报错"), TaskIntent::Debug);
        // 只写「报 500」不带报错/失败/错误时命不中任何规则，落到 General——这是上游英文规则
        // （`\berrors?\b` 一类关键词）的直接后果，不在这里私自扩表，否则中文侧会开始吞掉
        // 本来该走 explain 的提问。
        assert_eq!(detect("为什么登录接口报 500"), TaskIntent::General);
    }

    #[test]
    fn chinese_explain_plus_action_is_not_explain() {
        assert_eq!(detect("解释一下缓存失效的原因，然后修复它"), TaskIntent::Debug);
    }

    #[test]
    fn auto_mode_maps_intent_to_effective_mode() {
        assert_eq!(
            resolve_effective_mode(RewriteMode::Auto, TaskIntent::Implement),
            EffectiveMode::ExecutionContract
        );
        assert_eq!(
            resolve_effective_mode(RewriteMode::Auto, TaskIntent::Explain),
            EffectiveMode::Plain
        );
        assert_eq!(
            resolve_effective_mode(RewriteMode::Auto, TaskIntent::General),
            EffectiveMode::Plain
        );
        // 显式配置压过意图判定。
        assert_eq!(
            resolve_effective_mode(RewriteMode::Plain, TaskIntent::Debug),
            EffectiveMode::Plain
        );
        assert_eq!(
            resolve_effective_mode(RewriteMode::ExecutionContract, TaskIntent::Explain),
            EffectiveMode::ExecutionContract
        );
    }

    #[test]
    fn normalize_collapses_whitespace_and_lowercases() {
        assert_eq!(normalize("  Add\r\n  Tests  now "), "add tests now");
    }

    #[test]
    fn intent_and_mode_names_are_stable_wires() {
        assert_eq!(TaskIntent::TestFix.as_str(), "test-fix");
        assert_eq!(EffectiveMode::ExecutionContract.as_str(), "execution-contract");
        assert_eq!(RewriteMode::parse("auto"), Some(RewriteMode::Auto));
        assert_eq!(RewriteMode::parse("nope"), None);
    }
}
