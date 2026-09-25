// 从上游 pi 扩展 asymptotic-thinking 的 TypeScript 源码生成三份产物：
//
//   1. src/prompts/corpus.rs           —— 27 个领域提示词模块的数据表
//   2. tests/golden/prompts.json       —— 上游 buildPrompt / buildTemplate /
//                                        formatNextStateHint 的逐字输出
//   3. tests/golden/framework_rules.md —— 上游 SYSTEM.md（静态框架规则）
//
// 三者都是「移植是否逐字一致」的证据：Rust 侧的 tests/prompts.rs 拿它们逐字比对。
// 上游更新时重跑本脚本并 review 产物 diff。
//
// 用法（需要能跑 TypeScript 的运行时；本机用 bun 1.4.2 验证过）：
//
//   bun crates/astrcode-ext-asymptotic-thinking/tools/port-prompts.mjs <上游扩展目录>
//
// 脚本不直接 import 上游 src，而是先复制一份并替换掉 session-store.ts——
// 上游的 state-machine.ts 在模块加载期就会打开 SQLite，那与提示词组装无关。

import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const CRATE = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const UPSTREAM = process.argv[2];
if (!UPSTREAM) {
  console.error('用法：bun tools/port-prompts.mjs <上游 asymptotic-thinking 目录>');
  process.exit(2);
}

// ── 移植时对上游正文的**全部**改动，声明在一处 ──────────────────────────────
//
// 每一条都是「上游提到的东西在 AstrCode 里不存在」，不改会让模型去调用不存在的工具。
// Rust 侧存的就是应用过这些改动之后的正文，golden 也是，因此两边逐字相等。
const ADAPTATIONS = [
  // pi 的记忆系统叫 mempal，AstrCode 是 memory_list / memory_save。
  ['mempal本地记忆系统', 'memory_list/memory_save 本地记忆系统'],
  // pi 的联网工具叫 web_search / web-fetch，AstrCode 是 web-search / fetch-url。
  ['web_search网络搜索', 'web-search 网络搜索'],
  ['web-fetch网页获取', 'fetch-url 网页获取'],
  // SYSTEM.md 的注入通道：pi 用 resources_discover，AstrCode 用 prompt_build 钩子。
  [
    '本文件由 asymptotic-thinking 扩展通过 resources_discover 注入系统提示词（扩展加载才生效）。',
    '本文件由 asymptotic-thinking 扩展通过 prompt_build 钩子注入系统提示词（扩展加载才生效）。',
  ],
  // 状态机图的自动转换点：pi 在 before_agent_start，AstrCode 在 TurnStart 生命周期钩子。
  // 整行替换而不是只换词：新词短 9 个字符，只换词会让框线右端再错位一截；
  // 顺带把 `┘` 对齐到上一行的 `│`（上游本来就差 3 列）。
  [
    '   └──────────── before_agent_start 自动转换 ─────────────┘',
    '   └────────────────── TurnStart 自动转换 ───────────────────┘',
  ],
];

function adapt(text) {
  let out = text;
  for (const [from, to] of ADAPTATIONS) out = out.split(from).join(to);
  return out;
}

const fail = (message) => {
  console.error(`✗ ${message}`);
  process.exit(1);
};

/** Rust 字符串字面量转义（不用原始字符串，正文里可能出现 `"#`）。 */
function rust(text) {
  return `"${text
    .replaceAll('\\', '\\\\')
    .replaceAll('"', '\\"')
    .replaceAll('\n', '\\n')
    .replaceAll('\r', '\\r')
    .replaceAll('\t', '\\t')}"`;
}

// ── 在上游 src 的一份副本上做提示词组装 ─────────────────────────────────────
const SANDBOX = join(CRATE, 'target', 'port-prompts-src');
rmSync(SANDBOX, { recursive: true, force: true });
cpSync(join(UPSTREAM, 'src'), SANDBOX, { recursive: true });
writeFileSync(
  join(SANDBOX, 'session-store.ts'),
  'export const sessionStore = { load: () => null, save: () => {}, delete: () => {}, ' +
    'getAllSessionIds: () => [], isEnabled: () => true, setEnabled: () => {}, cleanup: () => 0 };\n',
);

const { buildTemplate } = await import(join(SANDBOX, 'templates.ts'));
const { formatNextStateHint } = await import(join(SANDBOX, 'state-machine.ts'));
const { loadPromptModule } = await import(join(SANDBOX, 'prompt-registry.ts'));

const STATES = ['DEEP_UNDERSTAND', 'DESIGN', 'EXECUTE', 'VERIFY'];
const DIFFICULTIES = ['TRIVIAL', 'SIMPLE', 'MODERATE', 'COMPLEX', 'HARD', 'EXTREME'];
const SUBS = {
  CODING: ['JAVA_DEV', 'RUST_DEV', 'PYTHON_DEV', 'JS_DEV', 'GO_DEV', 'CRUD_DEV', 'BUG_FIX',
    'CODE_REFACTOR', 'TESTING', 'ARCHITECT', 'CODE_REVIEW', 'PERF_OPTIMIZE'],
  RETRIEVAL: ['PAPER_RETRIEVAL', 'DAILY_RETRIEVAL', 'DOC_RETRIEVAL', 'CODE_RETRIEVAL'],
  ANALYTICS: ['DATA_ANALYSIS', 'CODE_ANALYSIS', 'LOG_ANALYSIS', 'REQUIREMENT_ANALYSIS'],
  DEVOPS: ['DEPLOY', 'MONITOR', 'CICD', 'CONFIG'],
  ENTERTAINMENT: ['FUN_CHAT', 'CREATIVE_WRITING'],
  GENERAL: ['GENERAL'],
};

/** 上游 `prompt-registry.ts` 的 kebab 路径。 */
const kebab = (master, sub) => `${master.toLowerCase()}/${sub.toLowerCase().replace(/_/g, '-')}`;

/** 上游把 LABEL / HINT 写成模块内不导出的 const，只能从 TS 源码里读。 */
function sourceConstant(master, sub, name) {
  const source = readFileSync(join(SANDBOX, 'prompts', `${kebab(master, sub)}.ts`), 'utf8');
  const match = source.match(new RegExp(`^const ${name} = "([^"]*)";`, 'm'));
  if (!match) fail(`${kebab(master, sub)} 里找不到 const ${name}`);
  return match[1];
}

const EXTRA = ' 需深度分析所有边界条件、隐含约束和潜在风险。';

/** 六档难度压成四档：上游 switch 把 HARD|EXTREME 与 MODERATE|COMPLEX（默认分支）合并了。 */
function tier(raw, state) {
  return {
    trivial: raw('TRIVIAL', state),
    simple: raw('SIMPLE', state),
    standard: raw('MODERATE', state),
    hard: raw('HARD', state),
  };
}

// ── 1. 逐模块取四档正文 ─────────────────────────────────────────────────────

const modules = [];
for (const [master, subs] of Object.entries(SUBS)) {
  for (const sub of subs) {
    const mod = loadPromptModule(master, sub);
    if (!mod) fail(`上游查不到模块：${kebab(master, sub)}`);
    const path = kebab(master, sub);
    const label = sourceConstant(master, sub, 'LABEL');
    const hint = sourceConstant(master, sub, 'HINT');
    const raw = (difficulty, state) => adapt(mod.buildPrompt(difficulty, state));

    // 四档模型必须成立，否则整张表就得改成六档。
    //
    // 只对 DESIGN/EXECUTE/VERIFY 断言：DEEP_UNDERSTAND 的正文里嵌了难度中文标签
    // （`当前为…（困难难度）。` vs `（极难难度）`），六档本来就两两不同。
    for (const state of STATES.filter((value) => value !== 'DEEP_UNDERSTAND')) {
      if (raw('EXTREME', state) !== raw('HARD', state)) {
        fail(`${path} 的 ${state}：EXTREME 与 HARD 输出不同，四档模型不成立`);
      }
      if (raw('COMPLEX', state) !== raw('MODERATE', state)) {
        fail(`${path} 的 ${state}：COMPLEX 与 MODERATE 输出不同，四档模型不成立`);
      }
    }

    // DEEP_UNDERSTAND 的首行带难度标签，正文与难度无关（HARD/EXTREME 多一段常量 extra）。
    const headTrivial = `当前为${label}（微不足道难度）。`;
    const headHard = `当前为${label}（困难难度）。`;
    const understandTrivial = raw('TRIVIAL', 'DEEP_UNDERSTAND');
    const understandHard = raw('HARD', 'DEEP_UNDERSTAND');
    if (!understandTrivial.startsWith(headTrivial)) {
      fail(`${path} 的 DEEP_UNDERSTAND 首行不是预期的「当前为…（…难度）。」`);
    }
    const understandTail = understandTrivial.slice(headTrivial.length);
    if (understandHard.slice(headHard.length) !== EXTRA + understandTail) {
      fail(`${path} 的 DEEP_UNDERSTAND 在 HARD 下的 extra 段不是预期常量`);
    }

    modules.push({
      master,
      sub,
      path,
      label,
      hint,
      understandTail,
      design: tier(raw, 'DESIGN'),
      execute: tier(raw, 'EXECUTE'),
      verify: tier(raw, 'VERIFY'),
    });
  }
}

// ── 2. 写 src/prompts/corpus.rs ─────────────────────────────────────────────

const corpus = [];
corpus.push('//! 27 个领域提示词模块的数据表，由 `tools/port-prompts.mjs` 从上游 TS 生成。');
corpus.push('//!');
corpus.push('//! **勿手工编辑**：改这张表不会改上游，只会让 `tests/prompts.rs` 的逐字对照失败。');
corpus.push('//! 要更新语料，重跑生成器并 review 本文件的 diff。');
corpus.push('//!');
corpus.push('//! 每个模块的正文按「状态 × 四档难度」拆开存；上游 `buildPrompt` 的 `switch` 正是这个形状，');
corpus.push('//! 生成器会断言 `HARD == EXTREME` 且 `MODERATE == COMPLEX`，不成立就直接失败。');
corpus.push('//!');
corpus.push('//! `understand_tail` 是 DEEP_UNDERSTAND 首行（`当前为…（…难度）。`）之后的固定正文；');
corpus.push('//! 首行由 [`super::assemble`] 按模块标签与当前难度重建。');
corpus.push('');
corpus.push('use super::{Module, Tiered};');
corpus.push('');

for (const entry of modules) {
  corpus.push(`/// \`${entry.path}\`。`);
  corpus.push(`pub static ${entry.master}_${entry.sub}: Module = Module {`);
  corpus.push(`    label: ${rust(entry.label)},`);
  corpus.push(`    hint: ${rust(entry.hint)},`);
  corpus.push(`    understand_tail: ${rust(entry.understandTail)},`);
  for (const state of ['design', 'execute', 'verify']) {
    corpus.push(`    ${state}: Tiered {`);
    for (const key of ['trivial', 'simple', 'standard', 'hard']) {
      corpus.push(`        ${key}: ${rust(entry[state][key])},`);
    }
    corpus.push('    },');
  }
  corpus.push('};');
  corpus.push('');
}

corpus.push('/// `prompt-registry.ts` 的 kebab 路径 → 模块路由表。');
corpus.push('pub static MODULES: &[(&str, &Module)] = &[');
for (const entry of modules) {
  corpus.push(`    ("${entry.path}", &${entry.master}_${entry.sub}),`);
}
corpus.push('];');
corpus.push('');

mkdirSync(join(CRATE, 'src', 'prompts'), { recursive: true });
writeFileSync(join(CRATE, 'src', 'prompts', 'corpus.rs'), corpus.join('\n'));

// ── 3. 写 tests/golden/prompts.json ─────────────────────────────────────────

const prompts = {};
for (const entry of modules) {
  const mod = loadPromptModule(entry.master, entry.sub);
  const perState = {};
  for (const state of STATES) {
    const perDifficulty = {};
    for (const difficulty of DIFFICULTIES) {
      perDifficulty[difficulty] = adapt(mod.buildPrompt(difficulty, state));
    }
    perState[state] = perDifficulty;
  }
  prompts[entry.path] = perState;
}

const templates = [];
const pushTemplate = (state, turn, master, sub, difficulty, visited) => {
  templates.push({
    state,
    turn,
    master,
    sub,
    difficulty,
    visited,
    output: adapt(buildTemplate(state, turn, master, sub, difficulty, visited)),
  });
};

pushTemplate('START', 3, null, null, null, []);
pushTemplate('START', 3, null, null, 'COMPLEX', []);
pushTemplate('END', 4, null, null, null, []);
pushTemplate('END', 4, null, null, 'COMPLEX', []);

for (const state of STATES) {
  for (const difficulty of DIFFICULTIES) {
    pushTemplate(state, 2, 'CODING', 'RUST_DEV', difficulty, ['DEEP_UNDERSTAND']);
  }
}

for (const [master, sub] of [
  ['CODING', 'CODE_REVIEW'],
  ['RETRIEVAL', 'PAPER_RETRIEVAL'],
  ['ANALYTICS', 'DATA_ANALYSIS'],
  ['DEVOPS', 'DEPLOY'],
  ['ENTERTAINMENT', 'FUN_CHAT'],
  ['GENERAL', 'GENERAL'],
]) {
  for (const state of STATES) {
    pushTemplate(state, 5, master, sub, 'MODERATE', ['DEEP_UNDERSTAND', 'DESIGN']);
  }
}

// 路径感知的三种适配段：跳理解到 DESIGN、回退到 DESIGN、直达 EXECUTE。
pushTemplate('DESIGN', 2, 'CODING', 'RUST_DEV', 'MODERATE', []);
pushTemplate('DESIGN', 2, 'CODING', 'RUST_DEV', 'MODERATE', ['DEEP_UNDERSTAND', 'EXECUTE']);
pushTemplate('EXECUTE', 2, 'CODING', 'RUST_DEV', 'TRIVIAL', []);
pushTemplate('EXECUTE', 2, 'CODING', 'RUST_DEV', 'MODERATE', ['DEEP_UNDERSTAND', 'DESIGN']);

const nextStateHints = [];
for (const from of [null, 'START', 'DEEP_UNDERSTAND', 'DESIGN', 'EXECUTE', 'VERIFY', 'END']) {
  for (const difficulty of [null, ...DIFFICULTIES]) {
    nextStateHints.push({ from, difficulty, output: formatNextStateHint(from, difficulty) });
  }
}

const golden = {
  source: 'cirno99/pi-backup @ pi-config-20260916-191214/agent/extensions/asymptotic-thinking',
  generator: 'crates/astrcode-ext-asymptotic-thinking/tools/port-prompts.mjs',
  adaptations: ADAPTATIONS.map(([from, to]) => ({ from, to })),
  prompts,
  templates,
  next_state_hints: nextStateHints,
};

mkdirSync(join(CRATE, 'tests', 'golden'), { recursive: true });
writeFileSync(join(CRATE, 'tests', 'golden', 'prompts.json'), `${JSON.stringify(golden, null, 2)}\n`);

// ── 4. 写 tests/golden/framework_rules.md ───────────────────────────────────

writeFileSync(
  join(CRATE, 'tests', 'golden', 'framework_rules.md'),
  adapt(readFileSync(join(UPSTREAM, 'SYSTEM.md'), 'utf8')),
);

console.log(`✓ corpus.rs：${modules.length} 个模块`);
console.log(
  `✓ prompts.json：${modules.length * STATES.length * DIFFICULTIES.length} 条提示词、` +
    `${templates.length} 条模板、${nextStateHints.length} 条流转提示`,
);
