# astrcodey-extensions

AstrCode 的**磁盘 s5r 扩展**工作区。用 Rust 写原生扩展进程（非 WASM、非进程内 bundled），
每个扩展是一个独立二进制，由宿主按 `extension.json` 启动，经 stdio 用 S5R 3.0 帧通信。

## 布局

```
crates/
  astrcode-ext-common/          扩展共用工具（不依赖宿主 SDK，可完整单元测试）
  astrcode-ext-cache-usage/     /usage —— 查看会话的 prompt 缓存命中率
  astrcode-ext-cache-doctor/    before_provider_request —— 观测 prompt 前缀，定位缓存断点
  astrcode-ext-context-offload/ post_tool_use —— 把过大的工具输出换成可检索的占位符
  astrcode-ext-hashline-edit/   hashline_read/replace/undo —— 哈希锚点编辑（每行一个地址）
  astrcode-ext-rtk-optimizer/   tool_input_transform + post_tool_use —— 命令改写与输出压缩
  astrcode-ext-weneed/          prompt_build + provider_contribution + pre_tool_use —— DeepSeek 的 we need 规范
  astrcode-ext-sleep-continue/  continue_after_stop + pre_tool_use + post_tool_use + turn_end —— 无人值守续跑与失败重试
scripts/install.sh              构建并安装到 ~/.astrcode/extensions/
```

宿主依赖走本地路径（`astrcodey` 未发布到 crates.io）：

```toml
astrcode-extension-worker = { path = "../astrcodey/crates/astrcode-extension-worker" }
```

换成远程依赖只需改工作区根 `Cargo.toml` 的 `[workspace.dependencies]` 一处。

## 状态存放决策表

每个扩展都要「记住点什么」，但存放位置不同。判据是**作用域**与**读写频率**：

| 存放位置 | 作用域 | 读写代价 | 用在哪 |
|---|---|---|---|
| 插件数据目录的 `config.json` | 插件级（全局） | 启动读一次，热路径只读内存 | `weneed`、`rtk-optimizer`、`cache-doctor` |
| 宿主的 `astrcode.session.state` | 扩展 × 会话 | 每次读写一次 IPC（带内存缓存） | `weneed` 的注入开关、`sleep-continue` 的续跑开关 |

判据展开：

1. **插件级配置走插件自己的数据目录。** `prompt_build` / `before_provider_request` 每轮都触发，
   读宿主状态意味着每请求一次 IPC；配置又是插件级而非会话级的，放本地最省。
2. **会话级状态也走插件数据目录，但按 `session_id` 分桶。** S5R worker 是跨会话共享的单进程，
   状态挂在「插件实例」上会让两个会话互相覆盖。不用 `session_state` 是因为它的单值上限是
   1 MiB，而状态机状态每请求都要读写。
3. **只有「必须跨扩展重载与宿主重启存活、且值极小」的开关才写宿主的 `session_state`。**
   `weneed` 的会话开关是 `on` / `off` 几个字节，远低于上限，换来的是一份宿主托管的持久化。

三者的**文件生命周期**（定位 → 读 → 归一化 → 原子写）由 `astrcode-ext-common` 的
`common::config::ConfigStore` 统一承担；各 crate 只提供「怎么解析、怎么归一化」。

## 常用命令

```sh
cargo test --workspace                                  # 各 crate 的单元测试与集成测试
cargo clippy --workspace --all-targets -- -D warnings
./scripts/install.sh                                    # 装到 ~/.astrcode/extensions/

# 线缆一致性验收：证明二进制是合法的 S5R 3.0 worker
cd ../astrcodey && cargo run -p astrcode-s5r-runtime --features conformance --bin s5r-conformance -- \
  --extension-id astrcode-cache-usage -- \
  ../astrcodey-extensions/target/release/astrcode-ext-cache-usage
```

---

## `astrcode-cache-usage`

### 用法

```
/usage
```

扫描当前会话的 durable 事件流，汇总每个模型请求的 `TokenUsageRecorded` 载荷，输出：

```
缓存命中 87.3% · 输入 1.20M（命中 1.05M / 未命中 150.0K） · 输出 45.6K · 128 次请求
明细：缓存写入 12.0K · 推理 8.0K · 上下文窗口 1.00M · 状态栏已更新为 cache 87.3%
```

同时把状态栏那一格更新成 `cache 87.3%`（命令行版页脚、网页版输入栏底部）。

### 为什么必须读原始事件

宿主的省事接口 `astrcode.session.history.token_usage` 只返回
`non-cached input + output` 与上下文窗口（见 `host_router/session.rs` 的
`history_token_usage`，它累加的是 `LlmTokenUsage::non_cached_tokens()`），**不含缓存字段**，
反推不出命中率。完整的 `LlmTokenUsage`（`cached_input_tokens`、
`cache_creation_input_tokens`、`input_accounting`）只出现在 durable 事件
`TokenUsageRecorded` 的载荷里，所以走 `astrcode.session.read_events` 分页读取。

### 命中率口径

provider 的 input 计数语义不同，直接相加会算错分母，因此按样本归一化后再累加：

| `input_accounting` | 完整 prompt | 命中 |
|---|---|---|
| `inclusive`（OpenAI 风格，`input_tokens` 已含缓存读取） | `input_tokens` | `cached_input_tokens` |
| `components`（Anthropic 风格，三段独立） | `input + cache_creation + cached` | `cached_input_tokens` |

未声明语义但出现 `cache_creation_input_tokens` 时按 `components` 处理。
这套判定与宿主 `LlmTokenUsage::non_cached_tokens` 的兜底路径逐位一致，
因此插件的「未命中」与宿主自身的 token 预算统计口径相同。

### 平台硬限制

这两条是 S5R 协议的边界，不是实现取舍：

1. **无法注册状态栏条目。** S5R 的 `InitializeManifest` 没有 `status_items` 字段且
   `deny_unknown_fields`，宿主 `S5rExtension::register()` 也不注册任何状态栏项
   （对比进程内 bundled 的 `mode` 扩展，它走 `Registrar::status_item` 所以能常驻）。
   因此那一格**只在第一次执行 `/usage` 之后**才出现。宿主不要求该 id 预先注册：
   前端 `applyDelta` 与 CLI `handle_event` 都直接按 id 写入渲染表。
2. **无法每轮自动刷新。** 向外推送状态更新的入口只有「命令结果携带 `status_update`」
   （`ExtensionCommandResult::Display`）这一条；钩子返回值没有对应字段。
   做不到 Claude Code 那种每轮自动刷新的用量条——那需要改宿主。

### 输出能力

插件只能输出纯文本：命令结果是 `Display { content }`，状态栏是一格 `text`。
没有表格、进度条、配色或图表，只能靠文字排版。

---

## `astrcode-context-offload`

### 用法

工具输出超过 8000 字符时自动生效，无需手动调用：

```
📦 [offload #call_abc123 · shell · 128.5K chars] npm run build
   → retrieve(ref="call_abc123") returns the full output, paginated via offset/limit
```

模型用 `retrieve` 按需取回原文（默认一页 8000 字符，上限 20000，支持 `offset`/`limit`
分页）；`/offload` 列出本会话已换出的条目与合计大小。

### 为什么原文必须由插件自己存

`post_tool_use` 的 `ModifyResult` 改的是**即将落盘的** `ToolResult.content`
（`astrcode-session/src/tool_pipeline/commit.rs:131-135`），宿主会把替换后的文本写进
durable 记录。换出因此是**不可逆**的——插件必须先存原文再返回占位符，否则模型再也
看不到完整输出。原文按 session 落在：

```
~/.astrcode/extension_data/astrcode-context-offload/sessions/<session>/offload/<ref>.txt
```

不用宿主的 `astrcode.session.state`：它的单值上限是 1 MiB
（`HOST_SESSION_STATE_VALUE_MAX_BYTES`），而大工具输出经常超过这个量级，且每次读写
都要走一次 IPC。

### 确定性

占位符对同一份输入产生逐字节相同的输出。它会被写进历史并在之后每一轮重放，任何
非确定性（时间戳、随机 id、哈希迭代顺序）都会让 provider 的前缀缓存失效。因此占位符
只使用内容本身可推导的信息：工具名、字符数、首个非空行预览、由 `tool_call_id` 派生的
ref。

### 平台边界

1. **无法撤销替换。** 见上：durable 记录里留下的是占位符，原文只在插件数据目录。
2. **`retrieve` 自身的结果不换出。** 否则模型会陷入「retrieve → 又得到占位符」的死循环。
3. **无法注册状态栏条目。** 与 cache-usage 相同：S5R 的 `InitializeManifest` 没有
   `status_items` 字段，那一格只在第一次执行 `/offload` 之后出现。
4. **阈值按字符数而非 token 数。** 工具热路径上不该为了精确阈值再走一次宿主 IPC；
   8000 字符对中文与代码都已经明显值得换出。
5. **分页按字符切分。** `offset`/`limit` 的语义是字符数，不是字节数。

---

## `astrcode-ext-weneed`
把 [dsh-weneed](https://github.com/Nwflower/dsh-weneed)（DSH 插件，MIT）的 **we need 模式**
移植到 AstrCode：在 DeepSeek 会话上向 system prompt 注入「`we need to ...`」思维链引导规范
（7 条，正文源自 [scp3500/oh-we-need](https://github.com/scp3500/oh-we-need)，MIT），
并参考 [phi-deepseek-enhanced](../phi-extensions/crates/phi-deepseek-enhanced) 补齐
「注入规范」之外提高遵循率的手段。

### 用法

| 命令 | 效果 |
|---|---|
| `/weneed` | 开启当前会话（默认本就是开启） |
| `/weneed off` | 关闭当前会话的注入 |
| `/weneed status` | 查看模型判定、会话开关、全局配置与运行期统计 |
| `/weneed global on\|off` | 全局总开关（落盘，是硬闸门） |
| `/weneed reminder off\|on-drift\|always` | 贴尾风格提醒的触发节奏 |
| `/weneed drift on\|off` | 是否读推理通道做漂移判定 |
| `/weneed guard on\|off` | 工具守卫（默认关闭） |
| `/weneed block [add\|remove] <tool>` | 查看/增删被拦工具 |
| `/weneed reset` | 恢复默认配置并清除本会话开关 |
| `/weneed help` | 全部子命令 |

触发条件是**模型**，不需要手动开启：只要激活模型是 DeepSeek，每轮推理就按规范展开。

### 四个钩子

| 钩子 | 职责 | 能力 |
|---|---|---|
| `prompt_build` | 注入完整规范到 system prompt 的静态前缀区 | — |
| `provider_contribution` | 需要时往请求尾部追加一条极简风格提醒 | `provider_request` |
| `after_provider_response` | 读 assistant 的推理通道，判定风格漂移 | `provider_request` |
| `pre_tool_use` | 拦截黑名单里的工具直呼 | `tool_intercept` |

### 触发判定只看 model id

判定规则是「按非字母数字切词，任一词以 `deepseek` 开头」，因此 `deepseek-chat`、
`deepseek-v4-flash`、`deepseek/deepseek-r1`、`accounts/x/models/deepseek-v3` 都命中，
而 `notdeepseek` 不命中。

**不能用 `ModelSelection::provider_kind` 判定。** 宿主组装 hook 上下文时走
`ModelSelection::simple(model_id)`（`astrcode-session/src/turn_context.rs:128`、
`session_prompt.rs:176`），`profile_name` 与 `provider_kind` **恒为空串**——
用 provider_kind 判定会让插件永远不触发。这一点已用真实宿主验证过。

### 规范注入路径

`prompt_build` 钩子返回 `PromptContributions::system_prompts`，宿主把它映射成
`ExtensionSection::PlatformInstructions`（`astrcode-session/src/session_setup.rs:84-88`），
落在 system prompt 的**静态前缀区**，因此只在贡献变化时让 provider 前缀缓存失效。

非 DeepSeek 会话**完全不读会话状态**：模型闸门在宿主调用之前就返回空贡献，
一次 `session_state` 都不会发出。全局总开关同样拦在会话状态之前。

### 贴尾风格提醒

完整规范落在 system prompt 的**静态前缀区**，离上下文尾部很远，而模型对远离尾部的指令遵循度
衰减很快。`provider_contribution` 的 `AppendMessages` 可以在请求组装前追加一条 request-local
消息，落在**尾部**同一位置——这是 phi-deepseek-enhanced 真正的杠杆。

AstrCode 没有「往当前用户消息末尾追加」的钩子（`user_message_envelope` 在 S5R worker 上被
显式拒绝），`provider_contribution` 是唯一的等价通道。

默认节奏是 `on-drift`：**每会话首个请求**贴一次引导，之后只在 `after_provider_response`
判定上一轮推理**漂移**时才贴。判定只看首句是否以 `we need` 起手——逐句检查会被编号列表、
代码块、工具叙述大量误伤，把提醒刷成噪音。provider 不回推理通道时（DeepSeek 非思考模式）
视为「无观测」，不会因此每轮刷屏。

### 工具守卫

`pre_tool_use` 拦截黑名单里的工具直呼，默认**关闭**，默认名单只有 `edit`。语义是**黑名单**
而不是 phi 的白名单：白名单在 AstrCode 里会把别的扩展注册的工具一起拦掉，而模型没有任何
自救手段。拦截原因里会给出替代用法（`hashline_read` 读、`replace` 定点改）与放行方式。

### 开关按会话持久化

`/weneed off` 写宿主的 `astrcode.session.state`（键 `weneed`，值 `on` / `off`），
落盘在 `<会话目录>/extension_data/astrcode-weneed/weneed`，扩展重载后仍然有效。
读路径带内存缓存：`prompt_build` 每轮都触发，缓存命中时不再产生宿主往返。

开关是**三态**：`/weneed off` 写入的是显式关闭，与「从未设置过」区分开，后者才回落到全局
配置。`/weneed reset` 写空串把它清回未设置。

读失败回落「未设置」而不是让本轮 prompt 组装失败；写失败如实上抛，但内存缓存已经更新，
本次会话内开关仍然立即生效。

### 全局配置

落在 `<astrcode_dir>/extension_data/astrcode-weneed/config.json`，首次启动时自动写入一份
默认配置。字段：`enabled`（总开关）、`reminder`（`off` / `on-drift` / `always`）、
`driftCheck`、`guard.enabled`、`guard.blockedTools`。归一化对残缺、类型错误的输入一律回落
默认值，因此手改坏的文件不会让插件失效。

### 与原版 dsh-weneed 的差异

1. **触发条件是模型，不是手动开关或 preset。** 原版靠 `autoEnable` 配置或 agent-preset
   常驻；AstrCode 没有 DSH 那套预设装配接口，注入改由 `prompt_build` 钩子承担。
2. **开关持久化。** 原版把状态放在进程内存；这里写宿主会话状态。
3. **规范第 6 条改写。** 原版要求把推理写进 ` thinking` 标签；AstrCode 走 provider 原生的
   `reasoning_content` 字段（`astrcode-core/src/llm.rs:138`），模型若照抄标签会把它当成
   **可见正文**输出，正好违反该条后半句。改后为「推理只写推理通道」。
4. **去掉身份句。** 原版首段的 `You are a helpful software engineer assistant.` 由 AstrCode
   的 Identity 段承担，重复声明会让两段互相竞争。

### 刻意不移植 phi-deepseek-enhanced 的两件事

1. **「首轮完整锚点 + 之后每轮极简提醒」的轮次节奏。** 那是 phi 的架构补丁：phi 只能把锚点
   追加到当前用户消息末尾，注入一次就随历史滚远，所以必须反复补。AstrCode 的 `prompt_build`
   **每轮**都会重新贡献规范，宿主把它放进 system prompt 的静态前缀区，既不会被淹没也不会被
   压缩丢掉。反过来，按轮次变换贡献内容会**每轮击穿 provider 前缀缓存**。
2. **「压缩后重注入」。** 同上：规范在 system prompt 里，不参与 transcript 重写。

### 平台边界

1. **无法注册状态栏条目。** 与另两个扩展相同：S5R 的 `InitializeManifest` 没有
   `status_items` 字段。
2. **不适用于非 DeepSeek 模型。** 规范是针对 DeepSeek V4 调优的提示词，换模型是另一回事，
   所以闸门是硬性的，`/weneed` 在非 DeepSeek 模型上也不会强行注入。
3. **规范正文与贴尾提醒是英文。** 与原版一致：它们是被调优过的提示词工件，逐句翻译会改变语义。
---

## `astrcode-cache-doctor`

把 [pi-cache-optimizer](https://github.com/jiangge/pi-cache-optimizer)（MIT）的**可落地子集**
移植到 AstrCode：观测 provider 请求的 prompt 前缀，定位缓存断点。

上游是 pi（TypeScript 宿主）的扩展，能力面与 s5r 磁盘插件并不等价；而且上游最核心的那几步
（前缀稳定化、`prompt_cache_key`、长缓存保留、Anthropic TTL 修正）在 AstrCode 上**已经由宿主内建**，
搬过来只会得到死代码。因此这里做的是上游思路在 AstrCode 真正缺的那一侧：**把断点指出来**。

### 用法

| 命令 | 效果 |
|---|---|
| `/cache-doctor` · `/cache-doctor help` | 用法 |
| `/cache-doctor status` | 当前模型、观测开关、本会话前缀保留情况、最近断点 |
| `/cache-doctor doctor` | 命中率 + 前缀断点 + 诊断建议 |
| `/cache-doctor stats` | 最近若干次请求的前缀对比明细 |
| `/cache-doctor reset` | 清空本会话的观测计数（保留上一次请求基线） |
| `/cache-doctor enable` / `disable` | 开关观测（持久化，重载后仍生效） |
| `/cache-doctor config` | 查看配置 |
| `/cache-doctor config watch on\|off` | 设置观测开关 |
| `/cache-doctor config history <n>` | 快照保留的对比条数（1..=200） |

`status` / `doctor` / `stats` 会把状态栏那一格（id `cache-prefix`）更新为 `prefix 11/12`。

### 观测的是什么

provider 的 prompt 缓存是**前缀缓存**：只有从第一条消息开始逐字节一致的部分才可能命中。
所以真正要回答的不是「哪里变了」，而是「共同前缀有多长、断点落在第几条消息上」。

`before_provider_request` 钩子每次拿到完整的 provider 可见消息列表，于是：

1. 对每条消息算一个指纹（序列化字节的 FNV-1a）与结构标签（角色 + 体量）；
2. 与同一会话上一次请求的指纹序列求最长公共前缀；
3. 断点不在「上一次的末尾」时，就说明**历史被追溯改写**了——provider 的前缀缓存从
   该位置起全部失效，其后的 prompt 每次都要重算。

指纹缓冲在调用内复用，比较代价只与消息条数相关（与正文体量无关）；内存里留下的是
「第 7 条 tool 消息，8.2K 字符」这样的结构性事实，**不保留任何 prompt 正文**。

`watch = off` 时钩子直接返回 `Allow`，不做任何指纹计算——关闭就是真的零开销。

### 为什么不重排 system prompt

上游最核心的一步是「把稳定的 system prompt 内容提到动态内容之前」。在 AstrCode 上这是**空操作**：

- system prompt 由 `astrcode-context/src/prompt_engine.rs` 按固定 section 顺序组装，
  `PromptSectionOrder::is_stable()`（`prompt_engine.rs:260-265`）已把 Identity / System /
  Task Guidelines / Communication 放在最前，动态段（Environment、Tool Summary、Skills 等）在后。
- 上游的 skills 压缩在这里也不需要：AstrCode 的技能索引本来就是单行紧凑格式
  （`astrcode-extension-skill/src/lib.rs:554` 的 `format_skills_for_model`），且带
  `MAX_DESCRIPTION_CHARS` / `MAX_INDEX_CHARS` 双重上限。
- 实测：同一会话 67 次请求里 `cached_input_tokens` 单调跟随上一次 `input_tokens`、从未重置，
  命中率稳定在 95–99%，说明 system prompt 在会话内逐字节稳定。

真正会毁掉缓存的不是 system prompt，而是**历史消息被追溯改写**（上下文压缩、扩展改写历史、
`provider_visible_messages` 的插话重排、deferred tools 提醒追加）。

### 上游能力对照

| 上游能力 | AstrCode 现状 |
|---|---|
| 稳定内容前置（`optimizeSystemPrompt`） | 宿主已内建：`prompt_engine.rs` 固定 section 顺序 + `is_stable()` |
| skills 列表压缩 | 宿主已内建：单行索引 + 双重长度上限 |
| `prompt_cache_key` 兜底 | 宿主已内建：`astrcode-ai/src/wire/openai/body.rs:233` 由 `(model, system, tools)` 派生 |
| 长缓存保留 | 宿主已内建：`prompt_cache_retention` profile 能力（`astrcode-core/src/config/raw.rs:162`） |
| Anthropic TTL 顺序修正 | 宿主已内建：`astrcode-ai/src/wire/anthropic/body.rs:344` 的 `cache_control: ephemeral` |
| `<session-overview>` 去抖 | trellis 特有块，AstrCode 不存在 → 不适用 |
| 缓存命中率统计 | 已由 `astrcode-cache-usage` 的 `/usage` 覆盖；本插件的 `doctor` / `stats` 复用同一口径 |
| 工具重排（`normalizeToolsInPayload`） | `ProviderHookInput`（`astrcode-extension-sdk/src/s5r/hooks.rs:57`）没有 tools → **不可达** |
| 请求体参数（cache key / retention / temperature） | 无请求体钩子，`ProviderResult` 只有 4 个变体（`extension/hooks/results.rs:102`）→ **不可达** |
| proxy affinity / adaptive-thinking 兼容诊断 | 钩子上下文里 `ModelSelection.provider_kind` 恒为空串（`turn_context.rs:128`）→ **不可达** |
| `fix` / `rollback` 改 `models.json` | 扩展被圈禁在 workspace 内，读不到也写不了 `~/.astrcode/config.toml` → **不可达** |
| router 协议集成（`Symbol.for`） | AstrCode 没有对应全局协议 → **不适用** |

### 平台边界

1. **无法注册状态栏条目。** 与另三个扩展相同：S5R 的 `InitializeManifest` 没有
   `status_items` 字段，那一格只在第一次执行 `/cache-doctor status`（或 `doctor` / `stats`）
   之后才出现；也没有「每轮自动刷新」的通道。
2. **观测状态不落盘。** 前缀对比天然只需要「上一次请求」这一个事实，重启后没有任何可比较的
   对象：重启后的第一次请求必然是「首次观测」。落盘只会留下过期的对比对象。
3. **只看得到 provider 可见消息。** 工具定义、请求体参数、provider 身份都不在钩子载荷里，
   因此「工具集变化导致 `prompt_cache_key` 改变」只能作为**可能性**提示，不能直接证实。
4. **不保留 prompt 正文。** 诊断信息只有角色、体量与指纹；这是刻意的隐私取舍。
5. **单进程作用域。** 观测表最多跟踪 64 个会话，超出按插入顺序淘汰（子 agent 会为每次委派
   新建会话）。

---

## `astrcode-ext-rtk-optimizer`

把 [pi-rtk-optimizer](https://github.com/MasuRii/pi-rtk-optimizer)（Pi 扩展，MIT）移植到 AstrCode：
在 `shell` 工具调用前把命令改写成 `rtk` 等价命令，并把 `shell`/`read`/`grep` 的工具结果
多级压缩。

### 用法

| 命令 | 效果 |
|---|---|
| `/rtk`、`/rtk show` | 显示配置与运行期状态（rtk 可用性、路径、最近一次改写） |
| `/rtk set` | 列出全部可修改项 |
| `/rtk set <key> <value>` | 在线改一项并落盘（替代上游的 TUI 弹窗） |
| `/rtk path` | 配置文件路径 |
| `/rtk verify` | 重新探测 `rtk` 可执行文件 |
| `/rtk stats`、`/rtk clear-stats` | 压缩节省统计（进程内、跨会话） |
| `/rtk reset`、`/rtk help` | 恢复默认配置 / 用法 |

两条能力链都自动生效，不需要手动开启：

- **命令改写**（`tool_input_transform`）：命令交给外部 `rtk rewrite` 决策。
- **输出压缩**（`post_tool_use`）：`shell` 走构建/测试/git/linter 聚合，`read` 走源码过滤与
  截断，`grep` 走搜索结果分组，最后统一硬截断。

配置落在 `<astrcode_dir>/extension_data/astrcode-rtk-optimizer/config.json`。字段名、默认值、
取值范围与上游一致；残缺或越界的手改文件按字段回落默认值，不会让插件失效。

### 改写决策完全交给 `rtk`

插件不维护任何改写规则表：`rtk rewrite <命令>` 的退出码就是契约（`0`/`3` 成功、`1` 无等价
命令、`2` 主动拒绝），与上游一致。`guardWhenRtkMissing` 打开时 rtk 不可用则原样放行，
并用 30 秒状态缓存避免反复探测。

调用走宿主受限子进程（`HostClient::process().spawn`），因此需要声明敏感能力
`process_spawn`，每次 `shell` 调用多一次子进程往返（实测 `rtk rewrite` 约 10ms，钩子预算 30s）。

### 与上游的差异

1. **修正了测试输出聚合的分组错位。** 上游第一条计数模式
   `/test result:\s*(\w+)\.\s*(\d+)\s*passed;\s*(\d+)\s*failed;/` 里组 1 是状态词，
   但取值时统一按「组 1 = passed」读，于是 `test result: FAILED. 2 passed; 1 failed;`
   会被报成「0 passed / 2 failed / 1 skipped」。上游测试没覆盖这条模式，所以一直没被发现；
   本移植按模式各自声明组映射，修正为「2 passed / 1 failed / 0 skipped」。
2. **保留 shell 结果的退出状态前导。** 宿主 shell 工具的内容是
   `Process exited with code N\nOutput:\n<输出>`，上游的 Pi bash 工具没有这段。压缩前切出
   前导、压缩后原样回贴，否则构建/测试过滤会把退出码一起丢掉。前导不计入节省统计。
3. **锚点按 astrcode 的 `read` 格式识别。** 宿主 `read` 输出每行是 `{:>6}\t内容`
   （右对齐 6 位行号 + Tab），上游针对 Pi hashline 的三条正则在这里一条都匹配不到。
   本移植把「行号 + Tab」视为不可分割的行首锚点：过滤与截断可以整行丢弃，但不切断它，
   硬截断插入锚点安全标记。注意 astrcode 的 `edit` 用 `oldText` 对文件正文精确匹配，
   行号前缀本身不是编辑锚点（工具说明写明「without line numbers」）；锚点安全解决的是
   「前缀不被切碎、模型仍能可靠剥离」，而「整行被丢掉导致 `oldText` 匹配失败」是有损压缩的
   固有代价——这正是 `readCompaction` 默认关闭的原因。
4. **`read` 的精确读取判定多了两个参数。** 上游只看 `offset`/`limit`；astrcode 的 `read`
   另有 `charOffset`/`maxChars` 两种字符范围参数，同样是「精确切片」意图，一并视为精确读取。
5. **技能目录按 astrcode 的布局。** `preserveExactSkillReads` 覆盖 `~/.claude/skills`、
   `~/.astrcode/skills` 与工作目录各级祖先下的同名目录（上游是 Pi 的 `.pi/skills` / `.agents/skills`）。
6. **通知改为事后查询。** 上游用 TUI 通知展示「命令被改写」与建议改写；S5R 钩子的返回值里
   没有面向用户的文本通道，因此最近一次改写记进运行期状态，由 `/rtk show` 呈现。
   `showRewriteNotifications` 随之变成「是否记录这条信息」。
7. **`RTK_DB_PATH` 的临时目录用本插件名**（`astrcode-rtk-optimizer` 而不是
   `pi-rtk-optimizer`），避免同一台机器上两个插件共用同一个历史库。
8. **排查提示改写。** 上游让用户「在 Pi TUI 里跑 `/rtk` 关掉 Read compaction」；AstrCode
   没有弹窗，提示改成 `/rtk set outputReadCompactionEnabled off`。
9. **新增 `regex` 依赖。** 构建/测试/linter/源码过滤的识别规则直接来自上游 TS 实现，
   逐条写成正则字面量才能逐条对照验收；手写扫描器会在十几处引入静默分歧。
   pattern 里一律把 `\d` 写成 `[0-9]`、`\w` 写成 `[A-Za-z0-9_]`，因为 JS 的这两类只覆盖
   ASCII 而 Rust `regex` 默认开启 Unicode。

### 平台边界

1. **无法推送 TUI 通知。** 见差异 6：`Replace` 只换工具入参，`ModifyResult` 只换工具结果正文。
2. **无法注册状态栏条目。** S5R 的 `InitializeManifest` 没有 `status_items` 字段，统计只能靠
   `/rtk stats` 拉取。
3. **没有流式输出清洗。** 上游监听 `tool_execution_update` 清洗流式 bash 输出；宿主没有
   对应钩子，`shell` 工具也不向钩子暴露部分输出。这一条**没有移植**，也不伪造用途。
4. **`read` 压缩是有损的。** 会丢掉整行，后续 `edit` 的 `oldText` 匹配可能因此失败。
   与上游一致默认关闭；开启且同时启用源码过滤与截断时，通过 `prompt_build` 注入排查提示。
5. **配置是全局的，不是按会话的**（与上游一致）。`/rtk set` 立即落盘，`session_start` 时
   重新读取，因此手工编辑的 `config.json` 在下一个会话生效。
6. **`process_spawn` 是敏感能力。** 只在真的需要命令改写时才有价值；`/rtk set enabled off`
   或把 `mode` 设成 `suggest` 都不会取消已声明的能力，只是不再调用。

### 性能

压缩管线在每次工具调用后都要跑一遍，因此按 5000 行的合成输入量过一遍
（`tests/performance.rs`，默认忽略）：

```sh
cargo test -p astrcode-ext-rtk-optimizer --release --test performance -- --ignored --nocapture
```

同机、同一份输入，改动前后两个二进制交替跑三轮取最小值：

| 项 | 改动前 | 改动后 | 加速 |
|---|---|---|---|
| `read` 默认配置（`readCompaction` 关） | 10.0µs | 7ns | 不再拷贝整段正文 |
| `grep` 默认配置 | 3.68ms | 0.31ms | 12.0x |
| `group_search_results` | 3.69ms | 0.26ms | 14.1x |
| `filter_source_code(aggressive)` | 9.95ms | 1.61ms | 6.2x |
| `read` 全开（普通路径） | 10.7ms | 2.18ms | 4.9x |
| `read` 全开（锚点路径） | 12.0ms | 4.79ms | 2.5x |
| `shell` 非构建命令（无技术命中） | 47µs | 32µs | 1.5x |
| `truncate` | 19.8µs | 14.5µs | 1.4x |

三处大头各有成因，都不是「算法只能这样」：

1. **`group_search_results`** 每行跑一次 `^(.+?):([0-9]+)?:(.+)$` 的 `captures`
   （占该技术 78% 的耗时），再每行分配三个 `String`，最后按文件名做 `Vec::iter_mut().find()`
   线性扫描。现在换成手写解析 + `BTreeMap` 分组，三者一起去掉。
   手写解析的语义由 `differential_matches_the_regex` 与原正则逐行对拍守住。
2. **`filter_source_code(aggressive)`** 每行先 `chars().collect::<Vec<char>>()`，再对**每个
   字符位置**调 `starts_with_at`，而它内部又 `pattern.chars().collect::<Vec<char>>()`——
   5000 行约 37 万次分配。现在在 `&str` 上按下标推进，模式匹配用 `str::starts_with`、
   块注释结束位置用 `str::find`，一次分配都不做；同时每行的代码部分只算一遍
   （原来 `count_code_braces` 与尾部比较各算一遍）。
3. **默认路径上的整段拷贝**：`strip_ansi_fast` 与 `compact_read_text` 的「原样返回」分支
   都会把整段正文拷一份再被丢掉。前者改成返回 `Cow`，后者改成在调用方提前放行。

`read` 全开还剩 4.79ms，其中 1.61ms 是源码过滤、0.23ms 是智能截断，余下约 2.9ms 落在
锚点式分支上——那条分支目前对整段输出做多次「逐行 `String` + 整段重新渲染」，
是下一步最值得动的地方。

### 测试

191 个测试：每个压缩技术一个模块（断言逐条对齐上游 TS 测试），加管线集成测试
（`shell`/`read`/`grep` 三条路径、锚点安全、前导保留、指标）、
`tests/rewrite_decision.rs`（用注入的 `HostApi` 覆盖 `rtk rewrite` 的每种退出码与 spawn 失败），
以及两处换实现后的**差分对照**：手写搜索行解析对拍原正则，按字节推进的 `get_code_portion`
对拍上游的 `Vec<char>` 写法。

```sh
cargo test -p astrcode-ext-rtk-optimizer
```

---

## `astrcode-hashline-edit`

把 [dsh-hashline-edit-pro](https://github.com/sleepinginsummer/dsh-hashline-edit-pro)（MIT，DSH 插件）
移植为磁盘 s5r 扩展；上游又是 Pi Coding Agent 生态
[pi-hashline-edit-pro](https://www.npmjs.com/package/pi-hashline-edit-pro) 的 DSH 移植。

**每行文本携带一个唯一的 3 字符内容哈希作为地址；编辑用哈希定位，绝不依赖行号或字符串匹配。**
文件在读取后被修改时，过期锚点会在写入前被拦下并返回新锚点反馈，因此不会出现「改错行」的静默损坏。

### 用法

| 工具 | 效果 |
|---|---|
| `hashline_read` | 把文件读成 `HASH│content` 行（3 字符字母数字锚点，无行号）。支持 `offset`/`limit` 分页；`raw: true` 返回普通带行号内容 |
| `replace` | 用 `remove_from`/`remove_to`（裸 3 字符哈希）圈定行范围，`replacement_text` 给新内容（`""` 删除范围） |
| `undo_last_replace` | 回滚某文件最后一次 `replace`；记录持久化，重启后仍可回滚 |

```
hashline_read  → ve7│function hello() {
                 aB3│  console.log("world");
                 cD4│}
replace(path="a.js", remove_from="aB3", remove_to="aB3",
        replacement_text="  console.log(\"hi\");")
  → Successfully replaced in a.js. Added 1 line(s), removed 1 line(s).
    Diff (HASH│anchored; ...):
     ve7│function hello() {
    -aB3│  console.log("world");
    +xY9│  console.log("hi");
     cD4│}
```

`+` 行携带的是**编辑后**的新锚点，所以可以直接拿 `xY9` 做下一次 `replace`——不必重新读文件。
未触碰的行（`ve7`、`cD4`）锚点保持不变，这就是「链式编辑」的基础。

### 两道闸门

1. **锚点校验。** 两个锚点必须在当前文件里**唯一**命中。过期报 `[E_STALE_ANCHOR]`、
   重复报 `[E_AMBIGUOUS_ANCHOR]`，两者都随错误附上当前上下文的新锚点，模型下一轮直接可用。
2. **served 守卫。** 被替换范围内的每一行都必须是模型**真正见过**的（`hashline_read` 输出过、
   或 diff 里出现过）。这道闸门拦住「凭记忆编一个锚点」：编出来的哈希即使碰巧存在，也不在
   served 集合里，报 `[E_RANGE_STALE]`。

### 自动纠正

模型经常把整行（`aB3│content`）或 diff 预览的 `+`/`-` 行粘进锚点字段。这些不报错，而是剥掉
并附警告，让一次工具调用就能纠正：`[E_BAD_REF]`（锚点字段里的 `HASH│` / `+` / `-` 前缀）、
`[E_BARE_HASH_PREFIX]`（`replacement_text` 行首的 `HASH│`）、`[E_INVALID_PATCH]`（diff 标记）。
反向的 `remove_from`/`remove_to` 会对调并报 `[E_BAD_OP]`。

### 与原版的差异

1. **工具是增量的，不遮蔽内置 `read`/`edit`。** Pi 原版覆盖内置 `read` 并禁用 `edit`；
   AstrCode 的 `ToolRegistry` 对重名注册直接报 `duplicate tool registered`，磁盘扩展无法遮蔽
   内置工具。所以与原版 DSH 移植一样注册**新增**三个工具，靠 system prompt 引导模型优先用
   `replace`（`prompt_build` 钩子 → `ExtensionSection::PlatformInstructions`）。
   这不影响安全性：served 守卫只认 `hashline_read` 展示过的锚点，模型用内置 `read` 拿到的
   行号在 `replace` 里会被直接拒绝。
2. **状态按会话隔离。** 原版把状态挂在插件实例上、文件落在首个会话的 cwd；S5R worker 是
   跨会话共享的单进程，那样做会让两个会话互相覆盖。这里按 `session_id` 分桶，各自写
   `~/.astrcode/extension_data/astrcode-hashline-edit/sessions/<sid>/state.json`。
3. **文件 IO 直接走 `std::fs`。** 宿主没有「按行保真读写 + 原子替换」的等价能力：
   `workspace.read` 有 10MiB 文件 / 1MiB 输出 / 100k 行三重上限，且只返回 `String`，
   会把有效文件规模压到 ~1MiB。planner 仍声明 `ResourceAccess::read_write_file(path)`，
   让宿主权限系统看得见这次访问。
4. **嗅探边界修正。** 原版对 8192 字节的样本做严格 UTF-8 校验，采样点恰好切开一个多字节
   字符时会把合法 UTF-8 文件误判成二进制。这里只在整段校验失败时才去掉末尾至多 3 字节重试。
5. **undo 持久化失败会真的报警告。** 原版 README 写了 `[E_UNDO_UNAVAILABLE]`，但代码把
   `saveState` 的错误吞在内部，那段警告实际不可达；这里按 README 声明的契约实现。
6. **拒绝性返回标为 `is_error`。** `[E_UNDO_STALE]` 这类「拒绝执行」在 AstrCode 里走
   `is_error = true` 的工具结果，而不是像原版那样当成成功返回一段文本。
7. **哈希器换成 FxHash。** 见下方「性能」。

### 平台边界

1. **没有沙箱。** worker 是独立进程，能读写运行用户有权限的任何路径。路径解析用宿主给的
   `working_dir`（相对路径）或原样绝对路径；越界防护靠宿主权限系统与 planner 声明，
   不是强制隔离。
2. **无法注册状态栏条目。** 与另几个扩展相同：S5R 的 `InitializeManifest` 没有 `status_items`。
3. **一次 `replace` 只能改一个连续区间。** 这是原版的设计：用两个锚点圈定范围。多处不连续
   修改要多次调用（但可以链式，不必重读）。
4. **上限 100MB / 238328 行。** 238328 = 62³ 是 3 字符锚点的容量，超过就再也分配不出唯一锚点。
5. **文件 IO 在阻塞线程池上执行**（`spawn_blocking`），不声明 `ExecutionMode::Parallel`，
   因此同一 worker 内的工具调用是串行的。
6. **图片 / 二进制 / UTF-16 拒绝。** 图片请用内置 `read_image`；二进制请用内置 `read`/`write`。

### 性能

编辑要对整份文件重建几遍查找表，所以哈希器不是无关紧要的。实测（5000 行 / 154467 字节，
两个二进制**交替跑 6 轮取最小值**，基准见 `tests/performance.rs` 与 `tests/hasher_comparison.rs`）：

| 项 | std (SipHash13) | FxHash | 加速 |
|---|---|---|---|
| 纯散列 5000 键 | 36.4µs | 8.9µs | 4.11x |
| `HashMap<&str, Vec<usize>>` 建表+查找 | 297µs | 141µs | 2.12x |
| `val_edit`（锚点索引） | 638.7µs | 504.2µs | 1.27x |
| `map_stable_hashes` | 1579.3µs | 1260.3µs | 1.25x |
| 完整 replace 路径 | 2739.2µs | 2201.2µs | 1.24x |

对照组 `canon`（不用哈希表）只差 2%，说明残余噪声远小于上述差异。键空间是用户自己的源码，
不是对抗性输入，不需要 SipHash 的 HashDoS 防护，所以采用 `rustc-hash`。

> **记一次教训：** 「给整个插件换哈希器再各跑一次」是无效对照。两次跑在不同的机器状态上，
> 漂移（连 `canon` 都同向偏 10%）会完全盖住 24% 的真实差值，得出**相反**的结论。
> 要么在同一个二进制里交替测，要么两个二进制交替跑取最小值。

上表是**换哈希器时**的 A/B。绝对数字此后又被两轮改动压低了一截——`canon` 不再逐行
`String` 分配（LF 文件里 `trim_end` 给出的就是原切片的子切片，返回 `Cow` 就够了），
两张按内容分组的查找表改用 `common::arena` 的线程本地竞技场。同机、同一份输入，
改动前后**两个二进制交替跑三轮取最小值**：

| 项 | 改动前 | 改动后 | 加速 |
|---|---|---|---|
| `canon` 全文件 | 157.1µs | 37.2µs | 4.23x |
| `line_hashes_pure` | 510.3µs | 367.1µs | 1.39x |
| `val_edit`（锚点索引） | 498.1µs | 173.9µs | 2.87x |
| `map_stable_hashes` | 1255.5µs | 874.5µs | 1.44x |
| `apply_edit` | 777.5µs | 439.8µs | 1.77x |
| 完整 replace 路径 | 2119.8µs | 1431.5µs | 1.48x |

`val_edit` 的收益最大：它既要对整份文件跑一遍 `canon`（原来每行一次 `String`），又要
建一张「锚点 → 候选行号」的表（原来每个锚点一个 `Vec<usize>`），两条都在这一轮里被
拿掉了。`map_stable_hashes` 拿到 1.44x（快 30%），比原先估的 10~15% 多一截；它剩下的
开销是哈希表操作与锚点分配本身，不在这轮改动范围内。

### 测试

```sh
cargo test -p astrcode-ext-hashline-edit

# 微基准（默认忽略）
cargo test -p astrcode-ext-hashline-edit --release --test performance -- --ignored --nocapture
cargo test -p astrcode-ext-hashline-edit --release --test hasher_comparison -- --ignored --nocapture
```

纯核心的正确性靠**与原版 JS 实现逐位对照**：`hashline::xxh32` 与 `hashline::hash` 里的金标准
期望值，是把原版 `src/host.js` 的纯核心切出来在 Node 里实跑得到的。凡是对算法细节（边界重复
的判定方向、稳定映射的锚点继承顺序）读代码推断不确定的地方，都以实测输出为准。

---

## `astrcode-sleep-continue`

无人值守续跑。上游是 pi 插件的 `sleep-continue`（`cirno99/pi-backup` 的
`agent/extensions/sleep-continue/src/index.ts`）；本 crate 取它的语义，落在 S5R 的钩子上，
不照搬实现——宿主机制不同，照抄会做出一堆空转代码。

### 用法

```
/sleep                     # 等价于 /sleep on
/sleep on                  # 开启本会话的续跑（预算归零）
/sleep off                 # 关闭本会话（全局配置不受影响）
/sleep set 继续按计划推进   # 换续跑文本并开启
/sleep nudge 请直接给出下一步动作  # 换「上一步没进展」时的纠正提示
/sleep max 200             # 单次人工 turn 的续跑上限
/sleep idle 3              # 空转熔断阈值，0 表示关闭
/sleep answer off          # 关掉提问自动应答
/sleep tools add askUser   # 增删自动应答的工具
/sleep status              # 开关、预算、统计、最近一次停止原因
/sleep reset               # 恢复默认配置并清除本会话开关
```

开启后，模型每次**自然停下**时注入一条续跑消息再推一个 step：上一步干过活（有工具调用）
就注入续跑文本（默认 `继续`），没干活就换成纠正提示。单次 turn 上限 100 次。

### 五个钩子

| 钩子 | 模式 | 职责 |
|---|---|---|
| `continue_after_stop` | blocking | 判定该不该续跑；注入续跑文本并返回 `ContinueOneStep` |
| `pre_tool_use` | blocking | 拦下提问类工具，按推荐项自动作答 |
| `post_tool_use` | non-blocking | 记工具活动，供空转熔断与复读熔断判定 |
| `UserPromptSubmit` / `SessionStart` | non-blocking | 重置续跑预算 / 重载配置 |
| `turn_end` | non-blocking | turn 失败后自动重试（读持久事件日志找错误） |

### 注入走 `defer_context`，不是 `provider_contribution`

两者都能把一段话喂给下一轮，区别在**可见性**：

- `session.control.defer_context` 追加的是一条**持久化用户消息**，在下一步边界被
  `sync_mid_turn_user_messages` 吸收，转录里看得见——醒来能复盘插件到底做了什么。
- `provider_contribution` 的 `AppendMessages` 只作用于单次请求、不落 transcript，是
  goal 扩展用的路子。

本插件要的是「可复盘」，所以选前者。代价是多一次 IPC 与一次 transcript 落盘；换来的是
`/sleep` 跑了一夜之后，你翻历史就能看到每一次续跑。

顺带一提，`defer_context` 会让宿主在 `has_pending_mid_turn_user_messages` 那一支继续跑，
也就是**不加 `ContinueOneStep` 也会续跑**。这里两个都做：显式返回 `ContinueOneStep` 让
「为什么又跑了一步」不依赖宿主的隐含分支。

### 闸门：必须显式开启

会话开关是**三态**（未设置 / 开 / 关），写在宿主的 `session_state` 里，因此扩展重载后
仍然有效。判定上 `Unset` 与 `Off` 等价：**只有 `/sleep on` 过才续跑**。

配置里的 `enabled` 是全局总开关（默认**开**），只用来「一次关掉所有会话」。默认必须开，
否则 `/sleep on` 会是个静默无效的命令——新装插件的人第一次用它就撞上「命令说开了，但
什么都不发生」，那是最坏的第一印象。

### 到达上限只停止续跑，开关保持开启

上限的语义是**单次人工 turn 的预算**：`UserPromptSubmit`（每个 turn 开始时派发）会把计数
归零，所以你再输入任意一句话就重新给满。这比上游「到顶直接关掉开关」更适合真正的长任务
——半夜上限走完只是停下等你，不会再消耗额度，早上你敲一句话它接着干。

插件自己注入的消息在 turn 内被吸收，**不会**派发 `UserPromptSubmit`，因此这个归零只会被
真正的人工输入触发。

### 两道熔断

看门狗在内核层面做不了（见下），替代物是两道熔断，命中就把原因留在 `/sleep status` 里：

1. **空转熔断**：连续 `idleStop`（默认 3）次续跑都没有产生**任何工具调用**就停下。
2. **复读熔断**：连续 `noProgressStop`（默认 1）次续跑都**没有新内容**——没有工具调用，
   且回复与上一次规范化后完全相同或为空——就停下。

复读阈值比空转严得多，因为复读是模型在输出里打转（`Let me output.` / `OK.` 交替），多喂几轮
几乎不会自己走出来，而每一轮都要烧掉整段上下文。判定顺序是复读 → 空转 → 到顶：越具体的原因
越值得人看。

两道熔断都只在**判定停下**时生效，续跑时喂什么由「上一步有没有干活」决定：没干活（没有工具
调用，或回复重复/为空）就换成纠正提示而不是裸的「继续」——对复读的模型再说一次「继续」等于
给它同一张牌。

### 提问自动应答

命中 `answer.tools`（默认 `["askUser"]`）的调用直接 `Block`，把选中的答案写进拦截原因回给
模型——原因是**模型可见的**（作为工具调用被拒的错误文本）。

取值优先级是「标了 `recommended: true` 的选项」→「第一个选项」。宿主内建的 `askUser` 会
显式标记推荐项（用户超时未响应时宿主也选它），所以标记优先比上游「一律取第一个」更准；
但**至少给出一个答案**：上游的取舍是宁可答错也不卡住，这里保持一致。多选题把所有推荐项
一起勾上。解析不出选项时仍然拦下，退回「请按任务最合理的默认方案继续」，而不是放行——
放行会让这一轮卡在等人的弹窗上。

拦截原因里带一个逃生口：「如确需人类决策，请在回复末尾注明『需要人工确认：…』然后停下，
不要死循环追问」。

### 刻意不移植的一件事

**看门狗（无活动 → abort）。** 插件的宿主调用走 task-local 的 `with_host_api` 作用域，而
**这个作用域不传播到 `tokio::spawn`**（见 worker 的 `with_host_api` 文档）：插件起不了任何
「能在钩子之外调宿主」的定时器，也就没人在超时那一刻去 abort。空转熔断与复读熔断补上了其中
真正要紧的一半——模型不干活时停下。

### turn 失败后的自动重试

上游的退避重试靠 `after_provider_response` 的 HTTP 429/5xx 触发，这条路在 AstrCode 里走不通：
`ProviderHookInput` 没有 status 字段，更要紧的是 provider 请求失败时 turn 直接报错，
`continue_after_stop` **根本不会被调用**——那个时点上插件没有任何介入机会。

能介入的时点是 `turn_end`。它的载荷里没有错误信息，所以插件自己去读持久事件日志
（`session.read_events`，需要 `SessionHistory` 能力）：游标增量读，找新的 `error_occurred`。
两点刻意设计：

- **只认新事件。** 游标单调推进，历史错误不重试——扩展重载后从旧日志里翻出一条早就处理过的
  错误，去重试一个其实已经成功的 turn，比漏掉一次重试更糟。首次没有游标时唯一的例外是
  「错误就是日志的最后一条事件」：那说明这一轮失败之后什么都没写下来，正是「刚挂掉」的形状。
- **投递用 `queue_or_start`，不是 `inject_or_start`。** `turn_end` 是在 turn 任务内部 await 的，
  此刻 turn 还没结算完、执行槽仍被占用；`inject_or_start` 会把消息注进**正在失败**的那个 turn
  而丢失，`queue_or_start` 则在「已完成未 settle」的窗口里先 settle 再启动队首。

分类只能读文本（`ErrorOccurred.recoverable` 实测恒为 `false`，状态码也没有结构化字段）：
`model not found`、401/403/404/400/413/422（按**词边界**匹配，免得把 `bytes-read=1329670`
这类字节数当成状态码）、鉴权失败、配额不足等判为致命，直接停下等人；其余（传输中断、限流、
上游 5xx、以及**认不出来的一切**）按有界退避重试，`retryMax` 默认 3 次，退避 1s → 2s → 4s
截到 5s。`retryMax: 0` 关掉重试。

### 平台边界

1. **无法注册状态栏条目。** S5R 的 `InitializeManifest` 没有 `status_items` 字段。本插件
   在 `/sleep` 的命令结果里携带 `status_update`（`🌙 12/100`），因此那一格**只在敲过至少
   一次 `/sleep` 之后**才出现，且不会每轮自动刷新。
2. **`TurnAborted` 从未派发。** 协议里声明了 `LifecycleEvent::TurnAborted`，但宿主只发
   `TurnStart` / `UserPromptSubmit` / `StepStart` / `StepEnd` / `TurnEnd` 等，`TurnAborted`
   没有任何发射点。因此上游「Esc 中断即停用」做不了。**功能上没有损失**：中断之后
   `continue_after_stop` 不会再触发，循环本就停了，只是开关保持开启。
3. **没有 UI 通知通道。** 开启/停止/熔断都只能在 `/sleep status` 里看到。

---


## 关于 `bumpalo` 与 `simd-json`

两个 crate 都在 `astrcode-ext-common` 里封装成可复用模块（`arena`、`json`），带单元测试，
各扩展按需取用。**先说清楚哪条路径用不上**：宿主经 S5R 交给插件的事件是**已经解析好的**
`serde_json::Value`（`HostSessionEvent::payload`），线缆上没有任何原始字节暴露给插件。
只从 DOM 里取几个 `u64` 的汇总（`cache-usage` / `cache-doctor` 的用量统计）因此既不需要
竞技场也不需要 simd-json——把 `Value` 再序列化成字节交给 simd-json 重解析是负优化。

用得上的是**插件自己拥有的字节缓冲**。当前接入点：

| 接入点 | 用什么 | 换来什么 |
|---|---|---|
| `hashline-edit` 会话状态账本 | `json::parse_owned` | 直接吃掉刚读进来的 `Vec<u8>`，不再复制一份 |
| `rtk-optimizer` `config.json` | `json::parse_owned` | 同上 |
| `cache-doctor` `config.json` | `json::parse_owned` / `to_vec_pretty` | 读取同上；写入改用 simd-json 的 pretty 序列化 |
| `hashline-edit` `val_edit` / `map_stable_hashes` | `arena::with_scratch` | 按内容分组的查找表整段借用线程本地竞技场，调用结束整体复位 |
| `rtk-optimizer` `filter_build_output` | `arena::with_scratch` | 错误块分组不再逐行 `to_owned`，行内容直接借用输出切片 |

hashline-edit 编辑热路径的实测数字见上方「性能」与 `hashline/hash.rs` 的模块文档。

> 注意 `Bump` 不是 `Sync`，`&Bump` 因而**不是 `Send`**；worker handler 的 future 必须是
> `Send`，所以竞技场句柄不能跨 `.await` 持有——先完成全部异步取数，再在同步区块里
> 用竞技场做汇总与渲染。`arena::with_scratch` 的回调是同步的，类型上就堵住了这条。

### 什么时候不该用竞技场

竞技场省的是「全局分配器往返」，不是「复制」。如果一份临时数据本来就是某个输入的子切片，
直接借用比放进竞技场更划算——`rtk-optimizer` 的 `compact.rs` 正是这种形态：锚点式 `read`
输出的每一行都是原输出的子串，那里的临时 `String` 该改成 `&str`，而不是搬进竞技场。
这轮没做，因为它是另一类改动（借用手法），跟「让竞技场派上用场」不是一回事。

若要让 simd-json 进入**宿主事件**的热路径，可行方向只有两条：宿主侧改为暴露原始事件字节
（改宿主），或写一个输入本身就是字节的插件（如日志/文件处理类）。当前没有为此伪造用途。

## 新增一个扩展

1. `crates/astrcode-ext-<name>/` 建 crate，`src/main.rs` + `src/lib.rs`（逻辑放库里，
   集成测试才能导入）。
2. `Cargo.toml` 加 `astrcode-extension-worker`、`astrcode-ext-common`；测试依赖里
   单独开 `features = ["testing"]`。
3. 写 `extension.json`：`extension_id` 必须与 `Worker::new` 的第一个参数一致。
4. `Worker::new(id, version)` → 声明 `capability` → 注册 tool / command / hook →
   `worker.run_stdio()`。
5. 用 `s5r-conformance` 跑一遍线缆验收。
6. 在 `scripts/install.sh` 之外自备安装目标，或把新扩展加进脚本。

参考实现：`astrcodey/crates/astrcode-extensions/tests/s5r-guest/`。
协议细节：`astrcodey/docs/s5r-protocol.md`、`astrcodey/docs/extension-author-guide.md`。