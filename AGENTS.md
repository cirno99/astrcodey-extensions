# AGENTS.md

本工作区是 AstrCode 的磁盘 s5r 扩展集合，每个扩展是一个独立二进制，经 stdio 用 S5R 3.0
协议与宿主通信。通用开发约定见用户级 `~/.astrcode/AGENTS.md`；本文件只记录**扩展注入给
模型的提示词**，供在本仓库内工作时对齐行为。

下面几段提示词是**被调优过的英文工件**，改动会改变模型行为，因此逐字引用、不做翻译。
修改它们时必须同步改动对应的源码常量，两处不一致即为 bug。

---

## 哈希锚点编辑（`astrcode-ext-hashline-edit`）

- **源码位置**：`crates/astrcode-ext-hashline-edit/src/prompt.rs` 的 `GUIDANCE`
- **注入时机**：`prompt_build` 钩子常驻，宿主映射为 `ExtensionSection::PlatformInstructions`，
  落在 system prompt 的静态前缀区，只在贡献变化时让 provider 前缀缓存失效
- **注入内容**：

```
Hash-anchored editing is available: use hashline_read to read a file as HASH│content rows (unique 3-char anchors per line, no line numbers), then replace a line range with the replace tool using bare hashes in remove_from/remove_to. Anchors of untouched lines stay valid across replaces, so edits chain without re-reading. undo_last_replace reverts the last replace on a file. Pass raw:true to hashline_read for plain numbered output (inspection only). The built-in read/edit/write tools remain available for other purposes; prefer replace for targeted edits so stale-anchor corruption is impossible.
```

**在本仓库工作的含义**：优先用 `hashline_read` 读文件、用 `replace` 做定点编辑，不要用
`edit` 的字符串匹配去改大文件。`remove_from` / `remove_to` 只填裸的 3 字符锚点，不要带行
号或行内容。未改动行的锚点在多次 `replace` 之间保持有效，因此连续编辑无需重新读取。

---

## we need 规范（`astrcode-ext-weneed`）

这个扩展有**两段**模型可见的英文工件：常驻的完整规范，以及条件追加的贴尾提醒。

### 常驻规范

- **源码位置**：`crates/astrcode-ext-weneed/src/spec.rs` 的 `WE_NEED_SPEC`
- **注入时机**：`prompt_build` 钩子。仅当模型 id 属于 DeepSeek 家族、全局总开关开启、
  且本会话开关未关闭时注入；宿主映射为 `ExtensionSection::PlatformInstructions`，
  落在 system prompt 的静态前缀区，因此**只在贡献变化时**让 provider 前缀缓存失效
- **注入内容**：

```
When you think, start with "we need...". We need to follow this style for all internal reasoning (chain-of-thought):

1. **`we need to ...` / `we need ...` is the core pattern.** Either can open any sentence, not only the first. We need one concrete action per sentence.
2. **Interleave modal verbs:** I'll (next action) · I can (viable option) · I need (what must be done) · I should (what ought to be done) · I will (committed step).
3. **Avoid `let me ...`.** We need to prefer `we need to ...` / `we need ...` for opening steps.
4. **Short and colloquial.** We need one sentence per step, decision-level summaries only, we / I perspective.
5. **Classify every task first.** We need to pick a stable end: build (produce, verify, fix) · fix (read, locate, minimal change, verify) · weak (classify first, then build or fix).
6. **Reasoning channel only.** We need every reasoning step written in the reasoning channel, never in the final reply. We need to never emit reasoning text or reasoning tags as visible output.
7. **Scope.** We need this to shape reasoning only. Final replies follow the user's language and tone.
```

**在本仓库工作的含义**：这是一段**只塑形推理、不改变最终回复**的规范。内部推理按
`we need to ...` / `I will ...` / `I am ...` 三种句式展开，最终回复仍按用户的语言与语气写。

### 贴尾提醒

- **源码位置**：`crates/astrcode-ext-weneed/src/reminder.rs` 的 `REMINDER_TEXT`
- **注入时机**：**条件注入**，走 `provider_contribution` 的 `AppendMessages`，作为一条
  request-local 用户消息追加在**当前请求的尾部**（不落 transcript）。触发条件：
  `reminder` 配置为 `on-drift`（默认）且「本会话首个请求」或「上一轮推理被判定为漂移」；
  `reminder = always` 时每个请求都追加；`off` 时从不追加
- **注入内容**：

```
**Reasoning style reminder:** this session requires the "we need" reasoning style. Open the first sentence of your reasoning with `We need to ...` / `We need ...`; open every following sentence with `We need to ...` / `We need ...`, `I will ...`, or `I am ...` / `I'm ...`. One concrete action per sentence. Classify the task first, then act. Never write reasoning text, or this reminder, into the final reply.
```

**在本仓库工作的含义**：看到这段提醒说明**上一轮推理没按规范展开**（或者是本会话第一次请求）。
它是纠正信号，不是用户的新要求；按规范调整推理句式即可，不要在回复里提及它。

### 漂移判定

提醒的触发依赖 `crates/astrcode-ext-weneed/src/drift.rs` 的 `inspect`：读 assistant 消息的
推理通道，剥掉行首的 markdown 装饰与有序列表序号后，检查**首句**是否以 `we need` 起手。
只看首句是刻意的——逐句判定会被编号列表、代码块、工具叙述大量误伤，把提醒刷成噪音。

---

## RTK 优化器（`astrcode-ext-rtk-optimizer`）

- **源码位置**：`crates/astrcode-ext-rtk-optimizer/src/command.rs` 的
  `SOURCE_FILTER_TROUBLESHOOTING_NOTE`
- **注入时机**：**条件注入**。仅当 `enabled`、`output_compaction.enabled`、
  `read_compaction.enabled`、`source_code_filtering_enabled` 且
  `source_code_filtering != None`，并且 `smart_truncate.enabled || truncate.enabled`
  同时成立时才追加（见同文件的 `should_inject_troubleshooting_note`）
- **注入内容**：

```
RTK note: read compaction with source filtering is active, so `read` output may have whole lines removed. If an edit repeatedly fails because oldText does not match, run `/rtk set outputReadCompactionEnabled off`, re-read the file, apply the edit, then re-enable it with `/rtk set outputReadCompactionEnabled on`.
```

**在本仓库工作的含义**：`read` 压缩是有损的，会整行删除，因此 `edit` 的 `oldText` 可能匹
配失败。出现这种情况时不要反复重试或猜测，按提示词给出的步骤关掉
`outputReadCompactionEnabled`、重读文件、改完再开回来。

---
## 插件本身不注入提示词的部分

### `astrcode-ext-rtk-optimizer`

另外两个钩子对模型是**静默**的，不要指望在上下文里看到它们：

- `tool_input_transform`：`shell` 调用前把命令改写成 `rtk` 等价命令（`rewrite` 模式）或只
  记录建议（`suggest` 模式）。改写结果直接替换工具入参，没有面向用户的文本通道。
- `post_tool_use`：`shell` / `read` / `grep` 的结果走多级压缩管线。

被改写过什么、建议过什么，只能事后用 `/rtk show`、`/rtk stats` 查询。

### `astrcode-ext-weneed`

- `after_provider_response`：只读 assistant 的推理通道做漂移判定，返回值恒为 `Allow`。
  这是 **advisory** 钩子，但宿主仍会把 `ReplaceMessages` / `AppendMessages` 的结果并进最终
  回复正文（`astrcode-session/src/turn_runner.rs` 的 `dispatch_after_provider_response`），
  所以实现里绝不能带消息。
- `pre_tool_use`：工具守卫拦截时返回 `Block { reason }`。那段原因**是模型可见的**（作为工具
  调用被拒的错误文本），但它由当前配置动态生成，因此不作为固定工件在此逐字记录；措辞在
  `crates/astrcode-ext-weneed/src/guard.rs`。守卫默认关闭。

### `astrcode-ext-sleep-continue`

这个插件**没有** `prompt_build` 贡献，不往 system prompt 里塞任何东西。但它有三处**模型可见**的
文本，都由配置动态生成，因此按上面 `weneed` 守卫的惯例只记出处（默认值逐字抄录）：

- **续跑文本**：`continue_after_stop` 时经 `session.control.defer_context` 注入的那条用户消息，
  默认 `继续`。它**会落进 transcript**——上下文里看到一条没有对应人类操作的用户消息
  「继续」，就是它。措辞由 `crates/astrcode-ext-sleep-continue/src/config.rs` 的
  `DEFAULT_CONTINUE_TEXT` 与 `/sleep set` 决定。
- **纠正提示**：上一步没有工具调用（或回复与上一次重复、为空）时，续跑注入的不是续跑文本，
  而是这条，默认：

  ```
  上一步没有产生可继续的动作：没有工具调用，或回复与上一次重复、为空。请直接给出下一步具体动作或结论，不要复述计划。
  ```

  措辞由同文件的 `DEFAULT_NUDGE_TEXT` 与 `/sleep nudge` 决定。**改动必须两处同步。**
- **提问拦截原因**：`pre_tool_use` 拦下 `askUser` 时返回的 `Block { reason }`，措辞在
  `crates/astrcode-ext-sleep-continue/src/plan.rs` 的 `block_reason`。内容包含「每题自动选了
  哪个推荐项」以及一条逃生口（「如确需人类决策，请在回复末尾注明『需要人工确认：…』然后
  停下，不要死循环追问」）。

另外四个钩子对模型是**静默**的：

- `post_tool_use`：只计数工具活动（空转熔断与复读熔断的判据），返回值恒为 `Allow`。
- `UserPromptSubmit`：重置续跑预算。
- `SessionStart`：重载配置。
- `turn_end`：turn 失败后自动重试。`turn_end` 的载荷里**没有**错误信息，所以它用
  `session.read_events`（`SessionHistory` 能力）增量读持久事件日志找 `error_occurred`，
  按文本分类：传输中断 / 限流 / 上游 5xx 等按有界退避重试（`retryMax`，默认 3 次，
  退避 1s→2s→4s 截到 5s），`model not found` / 401 / 403 / 404 / 400 等致命错误直接停下。
  重试消息经 `session.control.queue_or_start` 排队——`turn_end` 在 turn 任务内部派发，
  `inject_or_start` 会注进**正在失败**的那个 turn 而丢失。

**在本仓库工作的含义**：上下文里出现「继续」或纠正提示这类没有来源的用户消息、或提问工具被
拒的报错，都不是新要求，而是这个插件在无人值守模式下替人推流程——按原任务继续即可，不要在回
复里提及它。反过来，如果你确实需要人类拍板，就在回复末尾写「需要人工确认：…」然后停下，那是
插件认的退出信号；被拦下后反复调用提问工具只会被反复拒绝。续跑与失败重试只在 `/sleep on` 过
的会话生效；看到自己上一步没调用工具就被喂了一句「上一步没有产生可继续的动作」，说明插件判
定你在打转，此时应当直接执行动作或给结论，不要再复述计划。
