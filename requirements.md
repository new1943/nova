# NOVA 项目需求文档

**版本**: v3.0
**日期**: 2026-04-08
**状态**: Phase 1 已实现 + Phase 2/3 骨架已搭建

> 基于 Claude Code 源码分析 + OpenClaw 文档，完整记录所有策略。

---

## 一、项目定位

**NOVA = Rust 重写的完整 OpenClaw + Claude Code 全部 16 个策略 + 赛博朋克 TUI**

### 核心定义

- **`kiko` 命令直接启动** — 输入 `kiko` 即可对话
- **完整 OpenClaw workspace** — SOUL / IDENTITY / USER / AGENTS / MEMORY / STATE / TOOLS / TASKS / HEARTBEAT 等全部文件
- **Claude Code 全部 16 个策略** — Query Loop、Compact、Token Budget、Forked Agent、Hooks、双写互斥等
- **赛博朋克 TUI** — ratatui 实现，青/紫/品红配色
- **完整 Session 管理** — workspace 文件持久化，JSONL 追加写入
- **Skills 系统** — ~/.nova/skills/ 下的可扩展 prompt 模板
- **Heartbeat 机制** — 后台周期性任务调度

### 与 OpenClaw 的关系

NOVA 是 OpenClaw 的 Rust 重写版。运行时加载同一套 workspace 文件。

| | OpenClaw（现有） | NOVA（Rust 重写） |
|:---|:---|:---|
| 语言 | TypeScript/Node.js | Rust |
| 界面 | Discord/飞书等 channel | 赛博朋克 TUI |
| Session | channel-based | workspace JSONL 文件 |
| 部署 | 需要 OpenClaw 守护进程 | 守护进程 + TUI 客户端 |

---

## 二、Claude Code 16 个核心策略

### 实现状态总览

| # | 策略 | 状态 | 模块 |
|:--|:---|:---|:---|
| 1 | Query Loop | ✅ 已实现 | `nova-core/src/agent/loop.rs` |
| 2 | Token Budget 双阈值 | ✅ 已实现 | `nova-core/src/token/budget.rs` |
| 3 | Compact 对话压缩 | ✅ 已实现 | `nova-core/src/token/compact.rs` |
| 4 | Forked Agent | ✅ 已实现 | `nova-core/src/agent/forked.rs` |
| 5 | PostSampling Hooks | ✅ 已实现 | `nova-core/src/hooks/post_sampling.rs` |
| 6 | StopHooks | ✅ 已实现 | `nova-core/src/hooks/stop.rs` |
| 7 | 双写互斥记忆 | ✅ 已实现 | `nova-core/src/memory/dual_write.rs` |
| 8 | 工具池稳定排序 | ✅ 已实现 | `nova-core/src/tools/registry.rs` |
| 9 | Team 系统 | 🔧 骨架已搭建 | `nova-core/src/team/` |
| 10 | Subagent spawn | 🔧 骨架已搭建 | `nova-core/src/subagent/` |
| 11 | SideQuery | ✅ 已实现 | `nova-core/src/sidequery/` |
| 12 | autoDream | 🔧 骨架已搭建 | `nova-core/src/dream/` |
| 13 | Worktree 隔离 | 🔧 骨架已搭建 | `nova-core/src/worktree/` |
| 14 | Coordinator 模式 | 🔧 骨架已搭建 | `nova-core/src/coordinator/` |
| 15 | Paste Store | 🔧 骨架已搭建 | `nova-core/src/paste/` |
| 16 | Session History JSONL | ✅ 已实现 | `nova-core/src/session/` |

### 策略 1：Query Loop（核心交互循环）

**状态**: ✅ 已实现

```
用户输入
  → 构建 messages（system + 历史 + 新输入）
  → API 流式调用（messages + tools）
  → 解析响应（text / tool_calls / thinking）
  → 无 tool_calls → Done
  → 执行每个工具（超时 60s）
  → 工具结果追加到 messages
  → 继续循环（最多 max_turns）
```

**约束**：
- max_turns = 20（可配置）
- 单个工具执行超时 = 60s（可配置）
- 工具执行失败 → 返回错误 JSON，不中断 loop
- 支持 MiniMax M2.7 的 thinking block

### 策略 2：Token Budget 双阈值

**状态**: ✅ 已实现

- 阈值1（90%）：累计 input_tokens 达到 context 的 90% 时触发 Compact
- 阈值2（边际递减）：当 token 消耗是上一轮的 3 倍时停止循环
- 精确计数依赖 API `usage.input_tokens`

### 策略 3：Compact（对话压缩）

**状态**: ✅ 已实现

- 触发时机：Token Budget 超过阈值时
- 保留 system prompt
- 早期消息 → LLM 摘要（目标 50 字）
- 最近消息原样保留
- 目标：压缩到 context 的 60%（可配置）
- 工具调用配对保护：不切割 tool_call + tool_result

### 策略 4：Forked Agent

**状态**: ✅ 已实现

- `tokio::spawn` 执行后台任务
- 独立 16K token 预算
- 最多 2 次重试

### 策略 5：PostSampling Hooks

**状态**: ✅ 已实现

- `MemoryExtractHook`：每次 LLM 响应后提取关键信息到记忆
- 在 forked agent 中异步执行，不阻塞主 loop

### 策略 6：StopHooks

**状态**: ✅ 已实现

- `MemoryExtractStopHook`：turn 结束时写入记忆
- 串行执行，阻塞主 loop

### 策略 7：双写互斥记忆系统

**状态**: ✅ 已实现（JSONL 版），🔧 重构为三层 markdown 记忆

- 主 agent 已写 → forked agent 跳过
- 主 agent 未写 → forked agent 兜底
- 绝不重复写入
- 记忆类型：user / feedback / project / reference
- 存储路径：`~/.nova/memories/<type>.jsonl`（旧）→ 迁移为三层 markdown 记忆（见 P0 新增需求）

### 策略 8：工具池稳定排序

**状态**: ✅ 已实现

- built-in tools 排前面（按注册顺序）
- MCP tools 排后面
- 每次注册后保持稳定顺序

### 策略 9：Team 系统

**状态**: 🔧 骨架已搭建（数据结构 + 配置管理 + Mailbox）

- `TeamManager`：创建/加载/保存团队
- `Team`：成员列表 + 任务列表
- `Task`：状态（Pending/InProgress/Done）
- `Mailbox`：per-agent 消息收件箱
- **待完成**：与 QueryLoop 集成、TeamCreate/TaskCreate 工具

### 策略 10：Subagent spawn

**状态**: 🔧 骨架已搭建

- `SubagentSpawner`：spawn 子 agent
- `SubagentConfig`：工具池类型（ReadOnly/FullCapability）
- `SubagentHandle`：后台任务句柄
- 支持并行 spawn
- **待完成**：Agent 工具、与 Team 系统集成

### 策略 11：SideQuery

**状态**: ✅ 已实现

- `SideQuery`：独立发起额外 API 查询，不走主 query loop，不计入 turn
- 支持同步等待（`query_await`）和异步查询（`query` + oneshot channel）
- 独立 2048 token 预算
- 已接入 Agentic Session Search（见下方）

### 策略 12：autoDream

**状态**: 🔧 骨架已搭建

- `DreamEngine`：空闲检测 + 建议生成
- 可启用/禁用
- 空闲超时触发
- **待完成**：与 daemon 集成、TUI 显示建议

### 策略 13：Worktree 隔离

**状态**: 🔧 骨架已搭建

- `WorktreeManager`：管理 git worktree
- `Worktree`：创建/清理临时 worktree
- 自动 cleanup（Drop trait）
- **待完成**：与 Session 集成

### 策略 14：Coordinator 模式

**状态**: 🔧 骨架已搭建

- `Coordinator`：多 Agent 编排
- 四阶段：Research → Synthesis → Implementation → Verification
- **待完成**：Worker spawn、阶段转换逻辑

### 策略 15：Paste Store

**状态**: 🔧 骨架已搭建

- `PasteStore`：hash 去重存储
- 相同内容只存一份
- 引用标签系统
- **待完成**：与 TUI 粘贴事件集成

### 策略 16：Session History（JSONL）

**状态**: ✅ 已实现

- Session 文件：`~/.nova/sessions/<uuid>.jsonl`
- 启动时自动恢复最近 session
- 每条消息实时追加
- 元数据分离：`<uuid>.meta.json`

---

## 三、TUI 界面规范

### 整体布局（三区域）

```
┌─────────────────── Status Bar ───────────────────────────┐
│ [●] NOVA v1.0 │ <session> │ 7091↑ 44↓ │ [██░░] 8%      │
├──────────── 2/3 宽度 ──────────┬──── 1/3 宽度 ───────────┤
│  ◈ Chat  聊天区               │  ⚙ Commands  命令区     │
│                                │                         │
│  ▶ You                         │  $ bash: ls -la         │
│    在？                        │  → file1.txt            │
│                                │  → file2.rs             │
│  ◆ Kiko                       │                         │
│    在的！有什么可以帮你的吗？😊 │  $ bash: cat foo.rs     │
│                                │  → fn main() { ... }    │
│                                │                         │
│                                │  (自动滚动到最新命令)    │
├────────────────────────────────┴─────────────────────────┤
│  ⌨ Input                                                 │
│  Type a message... (/new /quit)                          │
└──────────────────────────────────────────────────────────┘
```

### 区域说明

1. **Status Bar**（顶部，高度 3 行）
   - 连接状态指示灯（● 已连接 / ○ 未连接）
   - Session ID（前 8 位）
   - Token 用量（input↑ output↓）
   - Budget 进度条 + 百分比
   - 单行显示，不换行

2. **聊天区**（左侧，宽度 2/3）
   - 显示 User / Assistant / System 消息
   - 角色前缀：▶ You / ◆ Kiko / ◇ System
   - 支持 Wrap 自动折行
   - 自动滚动到底部，支持 Up/Down/PageUp/PageDown 手动滚动
   - 不显示工具调用细节（工具调用在右侧命令区显示）

3. **命令区**（右侧，宽度 1/3）
   - 显示所有工具调用及其结果
   - 格式：`$ <tool_name>: <参数摘要>` + 缩进的结果输出
   - 独立滚动，自动滚动到最新命令
   - 长输出截断显示（保留前 N 行 + "... truncated"）

4. **输入框**（底部，高度 3 行，全宽）
   - 横跨左右两个区域
   - 支持 `/quit` `/exit` `/new` 命令
   - 光标位置正确跟踪（含 CJK 宽字符）

### 按键绑定

| 按键 | 功能 |
|:---|:---|
| Enter | 发送消息 |
| Ctrl+C | 退出 |
| Up/Down | 聊天区滚动 |
| PageUp/PageDown | 聊天区快速滚动 |
| Tab | 切换焦点到命令区（命令区获得滚动控制） |
| `/quit` `/exit` | 退出 |
| `/new` | 新建 Session |
| `/search <query>` | 语义搜索历史 Session |

### 配色（赛博朋克主题）

| 元素 | 颜色 |
|:---|:---|
| 背景 | RGB(10, 10, 25) 深蓝黑 |
| 主色 | RGB(0, 255, 255) 青色 |
| 辅色 | RGB(180, 0, 255) 紫色 |
| 强调 | RGB(255, 0, 128) 品红 |
| 文字 | RGB(200, 200, 220) 浅灰 |
| 暗色 | RGB(80, 80, 100) 灰色 |
| 用户消息 | RGB(0, 200, 200) 青绿 |
| AI 消息 | RGB(180, 140, 255) 淡紫 |
| 工具消息 | RGB(255, 180, 0) 橙色 |
| 错误 | RGB(255, 60, 60) 红色 |
| 边框 | RGB(60, 0, 120) 暗紫 |
| 状态栏背景 | RGB(20, 0, 40) 深紫 |

### 已知问题（待修复）

- 状态栏信息重复显示两行（应为单行）
- 工具调用结果和聊天消息混在一起（应分左右两栏）
- daemon 日志输出混入 TUI 渲染（daemon 日志不应显示在 TUI 中）

---

## 四、附加功能

### 已实现

| 功能 | 状态 | 模块 |
|:---|:---|:---|
| 赛博朋克 TUI | ✅ | `nova-tui/` |
| Heartbeat 调度 | ✅ 骨架 | `nova-core/src/heartbeat/` |
| Skills 系统 | ✅ 骨架 | `nova-core/src/skills/` |
| Sandbox 安全策略 | ✅ 骨架 | `nova-core/src/sandbox/` |
| Retry Policy | ✅ 骨架 | `nova-core/src/retry/` |
| Workspace 文件加载 | ✅ 热加载+mtime缓存 | `nova-core/src/workspace/` |
| 每日笔记 | ✅ 骨架 | `nova-core/src/memory/daily.rs` |
| bash 受限模式 | ✅ | `nova-core/src/tools/bash.rs` |

### 工具

| 工具 | 状态 | 模块 |
|:---|:---|:---|
| `bash` | ✅ 已实现 | `nova-core/src/tools/bash.rs` |
| `read_file` | ✅ 已实现 | `nova-core/src/tools/read_file.rs` |
| `write_file` | ✅ 已实现 | `nova-core/src/tools/write_file.rs` |
| `glob` | ✅ 已实现 | `nova-core/src/tools/glob.rs` |
| `grep` | ✅ 已实现 | `nova-core/src/tools/grep.rs` |
| `file_edit` | 🔧 P0 待实现 | `nova-core/src/tools/file_edit.rs` |
| `browser` | ✅ 已实现 | `nova-core/src/tools/browser.rs` |

### P0 新增工具需求

#### file_edit — 精确字符串替换

参考 Claude Code `FileEditTool`，实现精确字符串替换，避免全量覆盖文件。

- 参数：`file_path`、`old_string`（要替换的精确文本）、`new_string`（替换后的文本）、`replace_all`（bool，默认 false）
- `old_string` 必须在文件中唯一匹配，否则返回错误（除非 `replace_all=true`）
- 保留精确缩进（tab/空格）
- 必须先 read_file 过才能 edit（防盲改）— 通过文件状态追踪实现
- 返回：修改后的 diff 信息（old_string、new_string、替换次数）

#### 三层记忆系统

参考 Claude Code `memdir/` + `services/autoDream/` 和 OpenClaw `extensions/memory-core/` dreaming 系统，实现基于人类记忆模型的三层记忆架构。

##### 层1：MEMORY.md（工作记忆）

当前核心事项，LLM 每次对话都能看到。

- 存储：`~/.nova/MEMORY.md`，纯 markdown，精炼索引+核心事项，<200 行
- 内容分类：用户（角色/偏好）、反馈（纠正/确认）、项目（进行中的工作）、参考（外部系统指针）
- 写入：LLM 通过 file_edit/write_file 主动维护（prompt 引导 + 用户显式要求）+ Dream 定期整理
- 读取：BootstrapLoader 每次 API 请求前加载，始终注入 system prompt
- 参考：Claude Code 的 `MEMORY.md` / `ENTRYPOINT.md` 索引机制

##### 层2：memories/YYYY-MM-DD.md（情景记忆）

按天组织的日记，时间是关键维度。

- 存储：`~/.nova/memories/YYYY-MM-DD.md`，每天一个文件，追加式写入，带时间戳标题
- 写入时机（系统自动，不依赖 LLM 自觉）：
  - Compact 前：信息即将丢失，用 SideQuery 生成即将被压缩的消息摘要追加
  - Session 结束时：退出 TUI / /new，预处理 session（保留 user 原文 + assistant 决策，删工具调用细节），map-reduce 分层摘要（Map 按维度提取关键点 → Reduce 汇总成 ~200 字），追加到当天日记
  - 每 N 个 turn（可选）：定期追加增量摘要
- 预处理 + Map-Reduce 摘要流程：
  1. 预处理：保留 user 原文 + assistant 决策，删除工具调用细节
  2. Map 阶段：按 ~180K 字符分批，每批按维度（事件、反馈、用户偏好、项目状态、重要决策、参考资料）提取关键点
  3. Reduce 阶段：合并所有批次摘要，汇总成 ~200 字最终摘要（纯文本，无 markdown）
- 整理：Dream 分析日记内容，生成摘要索引，清理冗余
- 召回：用户消息到达时，SideQuery 从日记文件中选相关的注入 `<relevant_memories>` 到 system prompt
- 与 Agentic Session Search 并行执行，共享 10 秒超时

##### 层3：sessions/（细节记忆）

完整对话记录，最重的一层，只在需要细节时才翻。

- 已有：JSONL session 文件 + Agentic Session Search
- 不变

##### Dream — 记忆整理

参考 Claude Code `autoDream`（4 阶段 forked agent）+ OpenClaw dreaming（Light/Deep/REM 三阶段 + 评分系统）。

- 触发条件：距上次整理 ≥24h + ≥5 个新 session，或用户手动 `/dream`
- 触发时机：每个 turn 结束时检查（stopHooks 中）
- 执行方式：forked agent / SideQuery，限制在 memory 目录内操作
- 整理流程：
  1. Orient — 读 MEMORY.md + ls memories/ 目录
  2. Gather — 读最近日记，必要时 grep session transcript
  3. Consolidate — 从日记提炼核心事项更新 MEMORY.md，合并重复，相对日期→绝对日期，删除矛盾
  4. Prune — MEMORY.md 保持 <200 行，日记冗余条目精简
- 锁机制：PID 文件锁防并发

##### 三层关系

```
MEMORY.md          ← 始终可见，精炼的"此刻"
    ↑ Dream 提炼
memories/YYYY-MM-DD.md  ← 按需召回，"那天"的摘要
    ↑ 系统自动写入（Compact 前 / Session 结束时）
sessions/<uuid>.jsonl   ← 最后手段，完整细节（Agentic Session Search）
```

#### browser — 浏览器自动化（已实现）

基于 `@playwright/mcp`（Microsoft 官方 Playwright MCP Server），通过 MCP JSON-RPC over stdio 协议驱动浏览器。

**架构（shell out 模式）**：
- 每次工具调用启动一个 `npx @playwright/mcp@latest --config ~/.nova/playwright-mcp.json` 子进程
- 通过 stdin 发送三条 MCP JSON-RPC 消息（initialize / notifications/initialized / tools/call）
- 从 stdout 读取响应，解析后返回给 LLM
- 零长连接，零状态管理，进程自生自灭

**支持的 action**：
- `navigate`：导航到 URL
- `snapshot`：获取页面无障碍树（aria snapshot）文本，含 `@ref` 元素引用
- `click`：点击元素（ref 或 selector）
- `type`：输入文字（ref 或 selector）
- `press`：按键（Enter / Tab / Escape 等）
- `scroll_down` / `scroll_up`：滚动页面
- `screenshot`：截图，保存到 `~/.nova/browser-screenshots/`
- `go_back`：后退
- `close`：关闭浏览器

**关键特性**：
- 自动发现本机 Chrome（`/Applications/Google Chrome.app/...`），规避 Playwright 内置 Chromium 被反爬检测的问题
- 支持指定 User Data Dir 保留登录状态和 Cookies
- 自定义 User-Agent 伪装正常浏览器
- `--disable-blink-features=AutomationControlled` 抑制自动化标记

**前提**：系统安装 Node.js >= 18（`brew install node`），`@playwright/mcp` 通过 `npx --yes` 自动按需下载。

**模块**：`nova-core/src/tools/browser.rs`（单文件，~230 行）

### Agentic Session Search

**状态**: ✅ 已实现

基于 Claude Code 的 `agenticSessionSearch` 机制，用 SideQuery + LLM 语义搜索历史 session。

**自动触发**：每次用户发送消息时（>5 字符），daemon 自动执行搜索，将最相关的 3 个历史 session 的摘要注入 system prompt 的 `<relevant_history>` 块中，让 LLM 拥有跨 session 的记忆能力。搜索有 10 秒超时，失败不影响主流程。注入量动态调整：使用 context window 的 5% 作为历史注入预算。

**手动触发**：TUI 中输入 `/search <关键词>` 可手动搜索并显示结果列表。

**工作流程**：
1. 加载所有 session 元数据（`.meta.json`）+ JSONL 历史消息
2. 提取每个 session 的标题（首条用户消息）和 transcript（前后各 50 条消息，截断到 2000 字符）
3. 预过滤：先找包含查询词的 session，再补充最近的 session，最多 50 个
4. 构建 prompt 发给 LLM（通过 SideQuery，不计入主 loop turn）
5. LLM 返回 `{"relevant_indices": [2, 5, 0]}` 格式的排序结果
6. 自动模式：取 top 3 结果注入 system prompt；手动模式：全部返回给 TUI 显示

**模块**：`nova-core/src/session/search.rs`

### Bootstrap 热加载

**状态**: ✅ 已实现

每次 API 请求前从磁盘重新加载 workspace 文件（SOUL/IDENTITY/AGENTS/USER/STATE/TASKS），带 mtime 缓存优化。

- 加载方式：OpenClaw 风格（每次请求前检查），但加了文件 mtime 缓存（文件没改不读磁盘）
- 注入顺序：SOUL → IDENTITY → AGENTS → USER → STATE → TASKS → HEARTBEAT（独立区块）→ 工具描述
- 截断规则：单文件 20K 字符，总量 150K 字符，头部 70% + 尾部 20% + 中间截断标记
- MEMORY.md 不注入 system prompt（通过工具搜索访问）
- 运行中修改 .md 文件立即生效，无需重启 daemon

**模块**：`nova-core/src/workspace/loader.rs` (`BootstrapLoader`)

### GrepTool

**状态**: ✅ 已实现

封装 ripgrep（rg）的代码搜索工具，比 bash + grep 更安全高效。

- 优先使用 `rg`，不可用时 fallback 到 `grep -rn`
- 支持：正则模式、glob 文件过滤、上下文行数、大小写不敏感
- 结果截断到 200 行，防止输出爆炸
- 每个文件最多 50 个匹配

**模块**：`nova-core/src/tools/grep.rs`

---

## 五、Workspace 文件结构

```
~/.nova/
├── config                # TOML 配置（顶层键值对，无 section header）
├── SOUL.md              # 系统人格定义
├── IDENTITY.md          # Agent 身份定义
├── USER.md              # 用户偏好定义
├── AGENTS.md            # 工作区规则
├── MEMORY.md            # 长期记忆
├── STATE.md             # 运行时状态
├── TOOLS.md             # 工具配置
├── TASKS.md             # 任务列表
├── HEARTBEAT.md         # 心跳任务配置
├── memory/              # 每日笔记
│   └── YYYY-MM-DD.md
├── skills/              # 技能目录
│   └── <skill-name>/
│       └── SKILL.md
├── sessions/            # Session 持久化
│   ├── <uuid>.jsonl
│   └── <uuid>.meta.json
├── memories/            # 记忆存储
│   └── <type>.jsonl
└── teams/               # 团队配置（Phase 2）
    └── <team-name>/
        └── config.json
```

---

## 六、配置文件格式

```toml
# ~/.nova/config（顶层键值对，不要加 [section]）
api_key = "<key>"
model = "MiniMax-M2.7"
api_base_url = "https://api.minimaxi.com/anthropic"
context_window = 200000
char_delay_ms = 5
heartbeat_interval_secs = 300
max_turns = 20
tool_timeout_secs = 60
compact_target_pct = 0.6
budget_trigger_pct = 0.9

# Browser 配置（均为可选，有合理默认值）
# browser_chrome_path = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
# browser_profile_dir = "/Users/<name>/.nova/browser-profile"
# browser_headless = true
```


---

## 七、技术栈

- Rust 2021 edition
- tokio 异步运行时
- ratatui 0.29 + crossterm 0.28 TUI
- reqwest 0.12 HTTP + SSE 流式
- serde + serde_json 序列化
- toml 配置解析
- Unix Domain Socket IPC（JSON lines 协议）
- Anthropic 兼容 API（支持 thinking block）

---

## 八、Out of Scope

- OpenClaw Gateway
- 多 Channel 支持
- WebSocket API / Web Control UI
- 多节点（iOS/Android）
- OAuth / Pairing
- Canvas / A2UI
