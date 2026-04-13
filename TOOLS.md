# 工具能力全景对照表

**版本**: v2.0
**日期**: 2026-04-13

> 基于实际源码整理。
> - Claude Code: `claude-code-main/tools/` 目录，每个工具一个子目录（prompt.ts + types.ts + *Tool.ts）
> - OpenClaw: `openclaw/src/agents/tools/` + `openclaw/src/agents/openclaw-tools.ts` 注册入口
> - NOVA: `nova-core/src/tools/`

---

## 一、总览对照

| 类别 | Claude Code | OpenClaw | NOVA |
|:---|:---|:---|:---|
| 文件读取 | FileRead | read | ✅ read_file |
| 文件写入 | FileWrite | write | ✅ write_file |
| 文件精确编辑 | FileEdit | edit | ❌ |
| 多段补丁 | — | apply_patch | ❌ |
| 文件搜索 | Glob | — | ✅ glob |
| 内容搜索 | Grep | — | ✅ grep |
| Shell 执行 | Bash | exec | ✅ bash |
| 后台进程 | BashOutput + KillShell | process | ❌ |
| Notebook | NotebookEdit | — | ❌ |
| 网页搜索 | WebSearch | web_search / x_search | ❌ |
| 网页抓取 | WebFetch | web_fetch | ❌ |
| 浏览器 CDP | — | browser（插件） | ❌ |
| 子 Agent | AgentTool | sessions_spawn / subagents | ❌（骨架） |
| 团队 | TeamCreate / TeamDelete | — | ❌（骨架） |
| 任务 CRUD | TaskCreate/Get/List/Update/Stop/Output | — | ❌（骨架） |
| Todo | TodoWrite | update_plan | ❌ |
| 跨 Agent 消息 | SendMessage | sessions_send | ❌ |
| MCP | MCPTool / McpAuth / ListMcpResources / ReadMcpResource | — | ❌ |
| LSP | LSP | — | ❌ |
| 计划模式 | EnterPlanMode / ExitPlanMode | — | ❌ |
| Worktree | EnterWorktree / ExitWorktree | — | ❌（骨架） |
| 配置 | ConfigTool | gateway | ❌ |
| Skill | SkillTool / SlashCommand | — | ❌（骨架） |
| 工具搜索 | ToolSearchTool | — | ❌ |
| 用户提问 | AskUserQuestion | — | ❌ |
| 定时任务 | CronCreate/Delete/List | cron | ❌（骨架） |
| 远程触发 | RemoteTrigger | — | ❌ |
| 主动消息 | SendUserMessage (Brief) | message | ❌ |
| 结构化输出 | StructuredOutput | — | ❌ |
| 等待 | Sleep | — | ❌ |
| Session 管理 | — | sessions_list/history/status/yield | ❌ |
| 图片 | — | image / image_generate | ❌ |
| PDF | — | pdf | ❌ |
| 音乐 | — | music_generate | ❌ |
| 视频 | — | video_generate | ❌ |
| TTS | — | tts | ❌ |
| Canvas | — | canvas | ❌ |
| 设备节点 | — | nodes | ❌ |
| Agent 列表 | — | agents_list | ❌ |
| 记忆系统 | memdir（非工具，系统管线） | memory_search / memory_get（显式工具） | ❌ |
| PowerShell | PowerShell | — | ❌ |
| REPL | REPL（内部） | — | ❌ |


---

## 二、Claude Code 工具详解（43 个）

> 源码：`claude-code-main/tools/` 每个子目录一个工具

### 2.1 Core — 文件操作（6 个）

#### FileRead (`FileReadTool/`)

读取本地文件。

- 参数：`file_path`（绝对路径）、`offset`（起始行）、`limit`（行数）
- 默认读前 2000 行，超 2000 字符的行截断
- 输出 `cat -n` 格式（带行号）
- 支持读图片（PNG/JPG，多模态展示）、PDF（逐页文本+视觉）、Jupyter notebook（.ipynb 所有 cell+output）
- 空文件返回系统提醒
- 鼓励并行读多个文件

#### FileWrite (`FileWriteTool/`)

写文件，全量覆盖。

- 参数：`file_path`（绝对路径）、`content`
- 已存在的文件必须先 Read 过才能 Write（防盲写）
- 优先编辑现有文件，不主动创建新文件
- 不主动创建 *.md / README

#### FileEdit (`FileEditTool/`)

精确字符串替换，修改代码的主要工具。

- 参数：`file_path`、`old_string`、`new_string`、`replace_all`（bool，默认 false）
- 必须先 Read 过才能 Edit
- `old_string` 必须唯一匹配，否则失败（除非 `replace_all=true`）
- 保留精确缩进（tab/空格），不能包含行号前缀
- 内部版额外规则：用最小 old_string（2-4 行足够），避免 10+ 行上下文
- 四阶段流水线：Validation → Preparation → Application → Verification
- 有文件状态追踪（content hash、mtime、encoding、line endings、isBinary）
- 输出含 structuredPatch（diff hunk）、gitDiff、userModified 标记

#### Glob (`GlobTool/`)

文件模式匹配。

- 参数：`pattern`（如 `**/*.js`）、`path`（搜索目录）
- 返回按修改时间排序的匹配路径
- 适用任意大小代码库

#### Grep (`GrepTool/`)

基于 ripgrep 的内容搜索。

- 参数：`pattern`（正则）、`path`、`glob`（文件过滤）、`type`（js/py/rust 等）
- `output_mode`：content / files_with_matches / count
- 支持 `-A`/`-B`/`-C` 上下文行、`-n` 行号、`-i` 大小写不敏感
- `head_limit` + `offset` 分页
- `multiline: true` 跨行匹配

#### NotebookEdit (`NotebookEditTool/`)

编辑 Jupyter notebook cell。

- 参数：`notebook_path`、`cell_id`、`new_source`、`cell_type`（code/markdown）、`edit_mode`（replace/insert/delete）

### 2.2 Exec — 执行（3 个）

#### Bash (`BashTool/`)

持久 shell session 中执行命令。

- 参数：`command`、`timeout`（最大 600000ms，默认 120000ms）、`description`（5-10 字）、`run_in_background`（bool）、`dangerouslyDisableSandbox`
- 输出超 30000 字符截断
- 不用于文件操作（有专用工具）
- 避免 find/grep/cat/head/tail/sed/awk/echo（有专用工具替代）
- 内置完整 git commit / PR 创建流程（含 HEREDOC 格式、Co-Authored-By、pre-commit hook 重试）
- 支持后台运行（`run_in_background: true`）

#### BashOutput (`BashTool/` 内)

读取后台 shell 输出。

- 参数：`bash_id`、`filter`（正则过滤行）
- 只返回上次检查以来的新输出

#### KillShell (`BashTool/` 内)

终止后台 shell。

- 参数：`shell_id`

### 2.3 Agent — 子 Agent（4 个）

#### AgentTool (`AgentTool/`)

启动子 agent 处理复杂多步骤任务。

- 参数：`description`（3-5 字）、`prompt`（详细任务）、`subagent_type`、`model`（sonnet/opus/haiku）、`resume`（恢复 agent ID）
- 内置 agent 类型：
  - `general-purpose`：通用，可访问所有工具
  - `Explore`：快速代码库探索，支持 quick/medium/very thorough 三档
  - `Plan`：规划 agent
  - `statusline-setup`：配置状态栏（仅 Read + Edit）
  - `claude-code-guide`：使用指南（Glob + Grep + Read + WebFetch + WebSearch）
- 每次调用无状态，agent 只返回一条最终消息
- 鼓励并行启动多个 agent
- 支持 `resume` 恢复之前的 agent 上下文

#### TeamCreate (`TeamCreateTool/`)

创建多 agent 团队。

- 参数：`team_name`、`description`
- 创建 `~/.claude/teams/{name}/config.json` + `~/.claude/tasks/{name}/`
- 工作流：创建团队 → 创建任务 → spawn 队友 → 分配任务 → 队友工作 → 关闭团队
- 队友通过 Agent 工具 spawn，指定 `team_name` + `name`
- 队友空闲是正常状态，发消息可唤醒
- 角色分离：read-only agent（Explore/Plan）不能修改代码
- 队友间通过 SendMessage 通信，消息自动投递
- 关闭前必须先终止所有队友

#### TeamDelete (`TeamDeleteTool/`)

删除团队和任务目录。

- 必须先终止所有活跃成员

#### SendMessage (`SendMessageTool/`)

跨 agent 消息传递。

- 参数：`to`（队友名 / `"*"` 广播）、`summary`、`message`
- 本地：`uds:/path/to.sock`（Unix Domain Socket，需 UDS_INBOX feature flag）
- 远程：`bridge:session_01AbCd`（跨机器）
- 消息以 `<cross-session-message from="...">` 到达
- 支持 ListPeers 发现其他 agent
- 支持 shutdown_request / plan_approval_request 协议响应

### 2.4 Task — 任务管理（7 个）

#### TaskCreate (`TaskCreateTool/`)

创建任务。

- 参数：`subject`（简短标题，祈使句）、`description`、`activeForm`（进行时形式，可选）
- 创建时状态为 pending
- 使用场景：3+ 步骤复杂任务、plan mode、用户提供多任务
- 不使用：单一简单任务、纯信息查询
- 支持 agent swarms 时可分配给队友

#### TaskGet (`TaskGetTool/`)

按 ID 获取任务详情（subject/description/status/blocks/blockedBy）。

#### TaskList (`TaskListTool/`)

列出所有任务（id/subject/status/owner/blockedBy）。

- 队友工作流：完成当前任务 → TaskList 找可用任务 → 优先按 ID 顺序领取

#### TaskUpdate (`TaskUpdateTool/`)

更新任务。

- 可更新：status、subject、description、activeForm、owner、metadata、addBlocks、addBlockedBy
- 状态流转：pending → in_progress → completed，或 deleted 永久删除
- 更新前应先 TaskGet 获取最新状态

#### TaskStop (`TaskStopTool/`)

停止运行中的后台任务。

#### TaskOutput (`TaskOutputTool/`)

收集任务结果。

#### TodoWrite (`TodoWriteTool/`)

管理当前 session 的 todo 列表。

- 参数：`todos` 数组，每项含 `content`、`status`（pending/in_progress/completed）、`activeForm`
- 同一时间只能有一个 in_progress
- 完成后立即标记，不批量
- 使用场景：3+ 步骤复杂任务、用户提供多任务列表
- 不使用：单一简单任务、纯信息查询、3 步以内

### 2.5 Web — 网页（2 个）

#### WebSearch (`WebSearchTool/`)

搜索网页。

- 参数：`query`、`allowed_domains`、`blocked_domains`
- 返回搜索结果块（标题、链接、摘要）
- 必须在回答后附 Sources 引用列表
- 搜索查询要用当前年份（从 `getLocalMonthYear()` 获取）

#### WebFetch (`WebFetchTool/`)

抓取 URL 内容并用小模型处理。

- 参数：`url`、`prompt`（提取指令）
- HTML 转 markdown 后用小模型处理
- HTTP 自动升级 HTTPS
- 15 分钟自清理缓存
- 重定向时返回重定向 URL
- 如有 MCP 提供的 web fetch 工具，优先用 MCP 版
- GitHub URL 优先用 `gh` CLI
- 非预批准域名：严格 125 字符引用上限，引号标记精确语言

### 2.6 MCP — Model Context Protocol（4 个）

#### MCPTool (`MCPTool/`)

调用 MCP 服务器工具。

#### McpAuth (`McpAuthTool/`)

MCP 认证。

#### ListMcpResources (`ListMcpResourcesTool/`)

列出 MCP 服务器资源。

#### ReadMcpResource (`ReadMcpResourceTool/`)

读取 MCP 资源。参数：`server`（服务器名）、`uri`（资源 URI）。

### 2.7 IDE（1 个）

#### LSP (`LSPTool/`)

Language Server Protocol 集成。

- 操作：goToDefinition、findReferences、hover、documentSymbol、workspaceSymbol、goToImplementation、prepareCallHierarchy、incomingCalls、outgoingCalls
- 参数：`filePath`、`line`（1-based）、`character`（1-based）
- 需要对应文件类型的 LSP server 已配置

### 2.8 Session（4 个）

#### Sleep (`SleepTool/`)

用户可中断的等待。

- 优先于 `Bash(sleep ...)`（不占 shell 进程）
- 可与其他工具并发调用
- 收到 `<tick>` 提示时寻找有用工作
- 每次唤醒消耗一次 API 调用，prompt cache 5 分钟过期

#### ScheduleCron (`ScheduleCronTool/`)

定时任务调度（CronCreate / CronDelete / CronList 三个子工具）。

- 受 `AGENT_TRIGGERS` feature flag 控制
- 运行时受 `tengu_kairos_cron` GrowthBook gate 控制
- `durable: true` 持久化到 `.claude/scheduled_tasks.json`（跨重启）
- `durable: false`（默认）仅 session 内有效
- `CLAUDE_CODE_DISABLE_CRON` 环境变量可本地禁用

#### RemoteTrigger (`RemoteTriggerTool/`)

通过 claude.ai CCR API 管理远程触发器。

- 操作：list / get / create / update / run
- OAuth token 自动注入，不暴露到 shell
- 直接调 API 而非 curl

#### SendUserMessage / Brief (`BriefTool/`)

向用户发送消息（KAIROS 系统）。

- 工具名实际为 `SendUserMessage`（`Brief` 是旧名）
- 参数：`message`（支持 markdown）、`attachments`（文件路径）、`status`（normal/proactive）
- 工具外的纯文本用户大概率看不到（在 detail view 里），真正的回复必须通过此工具
- proactive：定时任务完成、后台工作遇阻、需要用户输入
- 长任务流程：ack → 工作 → 结果，中间发 checkpoint（有信息量的，不是 "running tests..."）

### 2.9 Nav — 导航（4 个）

#### EnterPlanMode (`EnterPlanModeTool/`)

进入计划模式。

- 触发条件：多种可行方案、重大架构决策、大规模变更、需求不明确
- 不触发：简单任务、小 bug、单函数、纯研究
- 计划模式中：探索代码库 → 设计方案 → 写计划文件 → 用户审批

#### ExitPlanMode (`ExitPlanModeTool/`)

退出计划模式，提交计划供审批。

- 计划已写入文件，此工具只是信号
- 仅用于需要写代码的实现规划，不用于纯研究
- 有歧义时先用 AskUserQuestion 澄清

#### EnterWorktree (`EnterWorktreeTool/`)

创建隔离 git worktree。

#### ExitWorktree (`ExitWorktreeTool/`)

离开 worktree。

- 参数：`action`（keep/remove）、`discard_changes`（bool）
- keep：保留 worktree 目录和分支
- remove：删除（有未提交变更时拒绝，除非 `discard_changes: true`）
- 恢复原工作目录，清除 CWD 相关缓存
- 仅操作当前 session 的 EnterWorktree 创建的 worktree，不碰手动创建的

### 2.10 Config（4 个）

#### ConfigTool (`ConfigTool/`)

管理配置。

#### SkillTool (`SkillTool/`)

执行 skill（markdown 模板注入 system prompt）。

- 参数：`skill`（skill 名称）
- skill 列表占 context window 的 1%（字符预算）
- 每个 skill 描述最多 250 字符
- 调用后 skill prompt 展开提供详细指令

#### SlashCommand（`SkillTool/` 内）

执行自定义斜杠命令。

- 参数：`command`（如 `/review-pr 123`）
- 命令定义在 `.claude/commands/foo.md`

#### ToolSearchTool (`ToolSearchTool/`)

获取延迟加载工具的完整 schema。

- MCP 工具默认延迟加载（只有名字，无参数 schema）
- 查询形式：`"select:Read,Edit,Grep"`（精确）、`"notebook jupyter"`（关键词）、`"+slack send"`（名称必含+排序）
- 返回 `<functions>` 块中的完整 JSONSchema
- 加载后工具即可调用
- ToolSearchTool 自身永不延迟
- `alwaysLoad: true` 的 MCP 工具不延迟

### 2.11 UX（4 个）

#### AskUserQuestion (`AskUserQuestionTool/`)

向用户提问。

- 参数：`questions`（1-4 个问题）
  - 每个含 `question`、`header`（≤12 字符标签）、`options`（2-4 个选项，每个含 label + description）、`multiSelect`
- 用户始终可选 "Other" 自定义输入
- 推荐选项放第一个，label 加 "(Recommended)"
- 支持 `preview` 字段（markdown/HTML 预览，用于 UI 布局/代码片段对比）
- plan mode 中用于澄清需求，不用于 "计划好了吗？"

#### StructuredOutput (`SyntheticOutputTool/`)

返回结构化 JSON 输出。

- 工具名实际为 `StructuredOutput`
- 仅在非交互 session 中启用（SDK/CLI 模式）
- 接受动态 JSON Schema，用 Ajv 验证
- 必须在响应末尾恰好调用一次
- WeakMap 缓存 schema → tool 实例（同 schema 对象复用）

#### PowerShell (`PowerShellTool/`)

Windows PowerShell 执行（类似 Bash，用于 PS cmdlets）。

#### REPL (`REPLTool/`)

交互式 REPL 模式（内部 feature flag）。


---

## 三、OpenClaw 工具详解

> 源码：`openclaw/src/agents/openclaw-tools.ts` 为注册入口，`openclaw/src/agents/tools/*.ts` 为各工具实现。
> 注意：OpenClaw 的 exec/read/write/edit 等基础工具在 `pi-agent-core` 包中，browser 通过插件系统提供。

### 3.1 注册入口 (`openclaw-tools.ts`)

`createOpenClawTools()` 函数注册所有内置工具，返回 `AnyAgentTool[]`：

```
canvas, nodes, cron, message, tts,
imageGenerate, musicGenerate, videoGenerate,
gateway, agentsList, updatePlan,
sessionsList, sessionsHistory, sessionsSend, sessionsYield, sessionsSpawn,
subagents, sessionStatus,
webSearch, webFetch, image, pdf
+ 插件工具（resolveOpenClawPluginToolsForOptions）
```

### 3.2 Runtime — 执行

#### exec / process

- `exec`：运行 shell 命令（`bash` 是别名）
- `process`：管理后台进程（启动/停止/列出）
- 安全策略 `tools.exec.security` 不可通过 gateway 工具修改
- 源码：`bash-process-registry.ts` 管理进程注册

#### code_execution

沙箱化远程 Python 执行，隔离环境。

### 3.3 FS — 文件系统

#### read / write / edit / apply_patch

- `read`/`write`/`edit`：基础文件操作（在 pi-agent-core 中）
- `apply_patch`（`apply-patch.ts` + `apply-patch-update.ts`）：多段补丁，支持多 hunk 批量修改

### 3.4 Web — 网页（3 个）

#### web_search (`web-search.ts`)

- `createWebSearchTool()` 工厂函数
- 支持多 provider（通过 `resolveWebSearchDefinition` 解析）
- 插件可注册自定义 web search provider
- 有搜索结果缓存（`SEARCH_CACHE`）
- 还有 `x_search`（搜索 X/Twitter 帖子）

#### web_fetch (`web-fetch.ts`)

- 参数：`url`、`extractMode`（markdown/text）、`maxChars`
- 默认最大 50000 字符、2MB 响应体
- 支持 readability 模式（提取正文）
- SSRF 防护（`fetchWithWebToolsNetworkGuard`）
- 缓存 TTL 可配置
- User-Agent 伪装 Chrome
- 最大 3 次重定向

### 3.5 Browser — 浏览器 CDP（插件提供）

OpenClaw 的 browser 能力通过插件系统提供，不在 `openclaw-tools.ts` 直接注册。

核心源码分布：
- `plugin-sdk/browser-cdp.ts`：CDP URL 解析
- `plugin-sdk/browser-config.ts`：浏览器配置
- `plugin-sdk/browser-profiles.ts`：多 profile 管理
- `plugin-sdk/browser-bridge.ts`：浏览器桥接
- `plugin-sdk/browser-node-runtime.ts`：节点运行时
- `plugin-sdk/browser-control-auth.ts`：控制认证
- `agents/sandbox/browser.ts`：沙箱浏览器

能力（从文档 + 源码推断）：
- 标签管理：list/open/focus/close
- 导航：navigate URL
- 快照：AI snapshot（数字 ref）/ Role snapshot（e12 格式 ref）/ ARIA
- 截图：全页/元素级/带 ref 标签叠加
- 操作：click/type/press/hover/drag/select/scrollintoview/fill/upload/download/dialog
- 等待：文本/URL/加载状态/JS 条件/选择器可见
- JS 执行：evaluate
- 网络：console/errors/requests/responsebody
- 状态：cookies/localStorage/sessionStorage/地理位置/时区/语言/设备模拟/离线模式
- 独立 `openclaw` profile，不碰用户浏览器
- 支持多 profile（openclaw/work/remote）
- 支持远程 CDP + Browserless 托管
- Chrome 扩展 relay 模式

### 3.6 Session 管理（6 个）

#### sessions_list (`sessions-list-tool.ts`)

列出可见 session，支持 kind/最近活动/最后消息过滤。

#### sessions_history (`sessions-history-tool.ts`)

获取 session 历史（安全过滤视图）。去除 thinking tags、工具调用 XML、敏感内容。

#### sessions_send (`sessions-send-tool.ts`)

向其他 session 发消息。等待目标 run 完成并返回 assistant 回复。

#### sessions_spawn (`sessions-spawn-tool.ts`)

创建子 session。

- 参数：`task`、`label`、`runtime`（subagent/acp）、`agentId`、`model`、`thinking`、`cwd`、`runTimeoutSeconds`、`thread`、`mode`（run 一次性/session 持久）、`cleanup`（delete/keep）、`sandbox`（inherit/require）、`streamTo`、`lightContext`、`attachments`（内联附件，最多 50 个）
- 支持恢复已有 session（`resumeSessionId`）
- subagent 继承父 workspace 目录

#### sessions_yield (`sessions-yield-tool.ts`)

让出控制权（yield），回调通知。

#### subagents (`subagents-tool.ts`)

子 agent 编排。

#### session_status (`session-status-tool.ts`)

轻量级状态查询（usage/time/cost/model），可设置 per-session model override。

### 3.7 Media（5 个）

#### image (`image-tool.ts`)

图片分析。

- 支持本地文件、canvas snapshot
- 图片大小限制 + 质量压缩
- 需要模型有 vision 能力

#### image_generate (`image-generate-tool.ts`)

图片生成/编辑。支持多 provider（openai/google/fal 等）。

#### music_generate (`music-generate-tool.ts`)

音乐生成。支持多 provider（google/minimax 等）。后台执行（`music-generate-background.ts`）。

#### video_generate (`video-generate-tool.ts`)

视频生成。支持多 provider（qwen 等）。后台执行（`video-generate-background.ts`）。

#### tts (`tts-tool.ts`)

文本转语音。

### 3.8 PDF (`pdf-tool.ts`)

PDF 分析。

- 支持多 provider（`pdf-native-providers.ts`）
- 模型配置（`pdf-tool.model-config.ts`）

### 3.9 其他

#### canvas (`canvas-tool.ts`)

驱动 node Canvas。

- 操作：present/hide/navigate/eval/snapshot/a2ui_push/a2ui_reset
- snapshot 支持 png/jpg 格式
- eval 执行 JavaScript
- a2ui_push 推送 JSONL 数据

#### nodes (`nodes-tool.ts`)

发现和控制配对设备。

- 命令（`nodes-tool-commands.ts`）
- 媒体（`nodes-tool-media.ts`）
- workspace guard 保护

#### cron (`cron-tool.ts`)

定时任务管理。

#### gateway (`gateway-tool.ts`)

Gateway 运行时管理（owner-only）：config.schema.lookup / config.get / config.patch / config.apply / update.run。

#### message (`message-tool.ts`)

跨 channel 发消息（Telegram/Discord/WhatsApp/Slack/Signal/iMessage）。

- 支持 auto-threading（Slack）
- reply-to 模式：off/first/all/batched
- owner-only 限制
- 沙箱 root 支持

#### agents_list (`agents-list-tool.ts`)

列出所有 agent。

#### update_plan (`update-plan-tool.ts`)

更新当前结构化工作计划（类似 Claude Code 的 TodoWrite）。

### 3.10 工具分组与 Profile

源码中通过 `tools.allow` / `tools.deny` 和 `tools.profile` 控制：

| 分组 | 包含 |
|:---|:---|
| group:runtime | exec, process, code_execution |
| group:fs | read, write, edit, apply_patch |
| group:sessions | sessions_list/history/send/spawn/yield, subagents, session_status |
| group:memory | memory_search, memory_get |
| group:web | web_search, x_search, web_fetch |
| group:ui | browser, canvas |
| group:automation | cron, gateway |
| group:messaging | message |
| group:nodes | nodes |
| group:agents | agents_list |
| group:media | image, image_generate, music_generate, video_generate, tts |

| Profile | 包含 |
|:---|:---|
| full | 无限制 |
| coding | group:fs, group:runtime, group:web, group:sessions, group:memory, cron, image, image_generate, music_generate, video_generate |
| messaging | group:messaging, sessions_list/history/send, session_status |
| minimal | 仅 session_status |


---

## 四、NOVA 当前工具（5 个）

| 工具 | 模块 | 能力 |
|:---|:---|:---|
| bash | `tools/bash.rs` | Open/Sandbox 双模式，灾难命令拦截，命令替换拦截，权限提升拦截，50+ 白名单，~/.nova/ 保护 |
| read_file | `tools/read_file.rs` | 读文件，start_line/end_line 行范围，1MB 限制 |
| write_file | `tools/write_file.rs` | 写文件，覆盖/追加模式，自动创建目录，~/.nova/ 保护 |
| glob | `tools/glob.rs` | glob 模式文件搜索 |
| grep | `tools/grep.rs` | 封装 ripgrep，fallback grep，上下文行，大小写不敏感，glob 过滤，200 行截断 |

---

## 五、NOVA 缺失工具优先级

### P0 — 核心编码体验

| 工具 | 来源 | 理由 | 复杂度 |
|:---|:---|:---|:---|
| file_edit | CC FileEdit + OC edit | 精确字符串替换，避免全量覆盖 | 中 |
| web_search | CC WebSearch + OC web_search | agent 搜索最新信息 | 低 |
| web_fetch | CC WebFetch + OC web_fetch | 抓取网页内容 | 低 |
| memory_search | OC memory_search + CC memdir | 搜索记忆文件，见下方详解 | 中 |

### P1 — Agent 能力

| 工具 | 来源 | 理由 | 复杂度 |
|:---|:---|:---|:---|
| agent | CC AgentTool + OC sessions_spawn | 骨架已有 SubagentSpawner | 低 |
| todo_write | CC TodoWrite + OC update_plan | 任务进度追踪 | 低 |
| ask_user | CC AskUserQuestion | 向用户提问获取澄清 | 低 |
| apply_patch | OC apply_patch | 多段补丁 | 中 |

### P2 — 浏览器与高级

| 工具 | 来源 | 理由 | 复杂度 |
|:---|:---|:---|:---|
| browser (CDP) | OC browser | 浏览器自动化，用 chromiumoxide crate | 高 |
| plan_mode | CC EnterPlanMode | 复杂任务规划模式 | 中 |
| worktree | CC EnterWorktree | 骨架已有 WorktreeManager | 低 |
| skill | CC SkillTool | 骨架已有 SkillsLoader | 低 |
| lsp | CC LSP | 代码智能 | 高 |

### P3 — 扩展

| 工具 | 来源 | 理由 | 复杂度 |
|:---|:---|:---|:---|
| notebook_edit | CC NotebookEdit | Jupyter 编辑 | 中 |
| background_bash | CC BashOutput + KillShell | 后台 shell 监控 | 中 |
| mcp_tool | CC MCPTool | MCP 协议集成 | 高 |
| cron | CC ScheduleCron + OC cron | 骨架已有 HeartbeatScheduler | 低 |
| structured_output | CC StructuredOutput | SDK/非交互模式结构化输出 | 低 |
| send_message | CC SendMessage + OC sessions_send | 跨 agent 消息 | 中 |

---

## 六、Claude Code 内部版差异（源码实证）

从 `prompt.ts` 中 `process.env.USER_TYPE === 'ant'` 分支和 `feature()` 宏：

| 维度 | 外部版 | 内部版 (`ant`) |
|:---|:---|:---|
| 工具调用间输出 | "Be concise" | ≤25 字 |
| 最终响应 | 无硬限制 | ≤100 字 |
| 代码注释 | 未提及 | "默认不写注释，只在 WHY 不明显时加" |
| FileEdit | 无额外限制 | "用最小 old_string（2-4 行），避免 10+ 行上下文" |
| 多文件验证 | 无 | 3+ 文件编辑必须启动验证 agent（VERIFICATION_AGENT flag） |
| REPL | Opt-in | 默认开启 |
| Undercover | 代码被 `feature()` 剥离 | 公开仓库自动启用 |
| Feature Flags | 编译时 dead code elimination | 全部启用 |

### Feature Flags（源码中发现）

| Flag | 说明 | 源码位置 |
|:---|:---|:---|
| BUDDY | 虚拟宠物系统 | `buddy/` 目录 |
| KAIROS | 主动助手（Brief + Cron + RemoteTrigger） | `BriefTool/`, `ScheduleCronTool/` |
| KAIROS_BRIEF | 仅 SendUserMessage | `BriefTool/prompt.ts` |
| AGENT_TRIGGERS | Cron 调度系统 | `ScheduleCronTool/prompt.ts` |
| UDS_INBOX | Unix Domain Socket 消息 | `SendMessageTool/prompt.ts` |
| FORK_SUBAGENT | AgentTool 不延迟加载 | `ToolSearchTool/prompt.ts` |
| VERIFICATION_AGENT | 自动验证子 agent | — |
| CACHED_MICROCOMPACT | 零 API 调用上下文压缩 | — |
| EXPERIMENTAL_SKILL_SEARCH | AI skill 发现 | `ToolSearchTool/prompt.ts` |

### 内部模型代号（源码中发现）

| 代号 | 位置 |
|:---|:---|
| Capybara | buddy 物种名，用 `String.fromCharCode()` 编码避免构建扫描器 |
| Tengu | feature flag 前缀：`tengu_kairos_cron`、`tengu_kairos_cron_durable`、`tengu_glacier_2xr`、`tengu_hive_evidence` |
| Numbat | prompts.ts "Remove this section when we launch numbat" |


---

## 七、记忆搜索系统详解（源码）

Claude Code 和 OpenClaw 的记忆搜索架构完全不同。

### 7.1 Claude Code — memdir（LLM 选择式）

源码：`claude-code-main/memdir/`

**存储**：纯 markdown 文件目录（`~/.claude/memory/`），无向量数据库。

- `MEMORY.md`（旧）/ `ENTRYPOINT.md`（新）：索引文件，<25KB，每行 <150 字符
- 记忆类型（`memoryTypes.ts`）：user（角色/偏好）、feedback（纠正）、project（项目）、reference（外部指针）
- 每个记忆一个 .md 文件，带 frontmatter header

**搜索**（`findRelevantMemories.ts`）：

1. `scanMemoryFiles()` 扫描记忆目录，提取每个文件的 header（文件名+描述）
2. 过滤掉已展示过的（`alreadySurfaced`）
3. 用 sideQuery 调 Sonnet 模型选择最相关的（最多 5 个）
4. system prompt：给定用户查询 + 记忆文件列表（文件名+描述），返回有用的文件名
5. 选择规则：不确定就不选、空列表也行、最近用过的工具的 API 文档不选（但 gotcha/已知问题要选）
6. 返回文件绝对路径 + mtime

**维护**（Dream Mode，4 阶段）：

1. Orient：ls 记忆目录，读索引，浏览已有主题文件
2. Gather：检查日志，grep JSONL transcript（窄范围）
3. Consolidate：合并新旧，相对日期→绝对日期，删除矛盾
4. Prune：索引 <25KB，删除过期指针

**特点**：无 RAG、无向量嵌入、无 Pinecone。LLM 直接读写文本，Dream Mode 做维护。

### 7.2 OpenClaw — memory_search（SQLite 向量+FTS 混合检索）

源码：`openclaw/src/agents/memory-search.ts` + `openclaw/packages/memory-host-sdk/`

**存储**：SQLite 数据库（`~/.openclaw/state/memory/{agentId}.sqlite`）

数据库 schema（`memory-schema.ts`）：
- `meta` 表：key-value 元数据
- `files` 表：path / source / hash / mtime / size — 追踪已索引的文件
- `chunks` 表：id / path / source / start_line / end_line / hash / model / text / embedding / updated_at — 文本分块 + 向量嵌入
- `fts5` 虚拟表：全文搜索索引（unicode61 或 trigram tokenizer）
- `embedding_cache` 表：provider / model / hash / embedding — 嵌入缓存

**搜索配置**（`ResolvedMemorySearchConfig`）：

- `sources`：memory（记忆文件）/ sessions（历史 session）
- `extraPaths`：额外搜索路径
- `provider`：嵌入模型 provider（auto / openai / voyage / ollama / mistral 等）
- `fallback`：主 provider 不可用时的备选
- `model`：嵌入模型名
- `outputDimensionality`：嵌入维度
- `local`：本地模型路径/缓存目录
- `remote`：远程 API（baseUrl / apiKey / headers / batch 配置）
- `multimodal`：多模态嵌入支持（图片等）

**分块**：
- `chunking.tokens`：默认 400 token/块
- `chunking.overlap`：默认 80 token 重叠

**混合检索**（`query.hybrid`）：
- 向量搜索权重：默认 0.7
- 全文搜索权重：默认 0.3
- 候选倍数：默认 4x（先取 maxResults * 4 候选再排序）
- MMR（Maximal Marginal Relevance）：可选，lambda 默认 0.7
- 时间衰减：可选，半衰期默认 30 天

**查询参数**：
- `maxResults`：默认 6
- `minScore`：默认 0.35

**同步**：
- `onSessionStart`：session 开始时同步
- `onSearch`：搜索时同步
- `watch`：文件监控（debounce 1500ms）
- `intervalMinutes`：定期同步
- session 同步阈值：deltaBytes 100KB / deltaMessages 50 条
- compaction 后强制同步

**缓存**：默认启用，可配置 maxEntries

**特点**：完整的 RAG 管线 — 文件分块 → 向量嵌入 → SQLite 存储 → 混合检索（向量+FTS）→ MMR 去重 → 时间衰减。支持多 embedding provider、本地/远程模型、多模态。

### 7.3 对比

| 维度 | Claude Code (memdir) | OpenClaw (memory_search) |
|:---|:---|:---|
| 存储 | 纯 markdown 文件 | SQLite（向量+FTS） |
| 索引 | 无（文件名+描述 header） | 向量嵌入 + FTS5 全文索引 |
| 搜索 | LLM sideQuery 选择（最多 5 个文件） | 混合检索（向量 0.7 + FTS 0.3） |
| 嵌入模型 | 无 | 多 provider（openai/voyage/ollama/mistral/本地） |
| 分块 | 无（整文件） | 400 token/块，80 overlap |
| 去重 | 无 | MMR（可选） |
| 时间衰减 | 无 | 半衰期 30 天（可选） |
| 维护 | Dream Mode（LLM 整理） | 文件监控 + 定期同步 |
| 多模态 | 无 | 支持（图片嵌入） |
| 复杂度 | 极低 | 高 |
| 依赖 | 无（纯文件） | SQLite + 嵌入 API |

### 7.4 NOVA 的选择

NOVA 当前用 JSONL 记忆存储（`memory/dual_write.rs` + `memory/store.rs`）+ Agentic Session Search（`session/search.rs`，LLM 语义搜索历史 session）。

建议路线：
- **短期**：实现 `memory_search` 工具，用 LLM sideQuery 方式搜索 `~/.nova/memories/*.jsonl`（类似 Claude Code 的 memdir 方案，简单有效）
- **中期**：加 SQLite FTS5 全文索引，不需要向量嵌入也能大幅提升搜索质量
- **长期**：如果记忆量大，加向量嵌入（用 `candle` crate 本地推理或远程 API）
