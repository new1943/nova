# NOVA Phase 1 任务清单

**版本**: v3.0
**日期**: 2026-04-13
**目标**: MVP — 能跑、有性格、TUI 好看、基本工具、核心策略
**状态**: 以实际代码为准，已核实更新

---

## 任务总览

| ID | 任务 | 优先级 | 预估 | 依赖 | 状态 |
|:---|:---|:---|:---|:---|:---|
| T01 | Cargo Workspace 初始化 | P0 | 2h | — | ✅ 完成 |
| T02 | 配置加载 | P0 | 2h | T01 | ✅ 完成 |
| T03 | API 客户端 + SSE Streaming | P0 | 4h | T02 | ✅ 完成 |
| T04 | Workspace 加载 + System Prompt | P0 | 3h | T02 | ✅ 完成 |
| T05 | 工具系统（trait + registry + 5 个工具） | P0 | 6h | T02 | ✅ 完成 |
| T06 | Query Loop 核心 | P0 | 8h | T03, T04, T05 | ✅ 完成 |
| T07 | Session 持久化（JSONL） | P0 | 3h | T06 | ✅ 完成 |
| T08 | IPC 协议层 | P0 | 2h | T01 | ✅ 完成 |
| T09 | 守护进程 | P0 | 3h | T06, T08 | ✅ 完成 |
| T10 | 赛博朋克 TUI | P0 | 6h | T08 | ✅ 完成 |
| T11 | Token Budget 双阈值 | P1 | 2h | T06 | ✅ 完成 |
| T12 | Compact 对话压缩 | P1 | 4h | T03, T11 | ✅ 完成 |
| T13 | Forked Agent | P1 | 3h | T03 | ✅ 完成 |
| T14 | Hooks 系统（PostSampling + Stop） | P1 | 3h | T06, T13 | ✅ 完成 |
| T15 | 双写互斥记忆 | P1 | 3h | T14 | ✅ 完成 |
| T16 | Heartbeat 机制 | P1 | 2h | T09 | ✅ 完成 |
| T17 | Skills 系统 | P1 | 2h | T05 | ✅ 完成 |
| T18 | bash 受限模式安全加固 | P2 | 2h | T05 | ✅ 完成 |
| T19 | 端到端集成测试 | P2 | 4h | 全部 | ❌ 未开始 |
| T20 | file_edit 精确编辑工具 | P0 | 3h | T05 | ✅ 完成 |
| T21 | 三层记忆系统 | P0 | 10h | T15, T05 | ✅ 完成 |
| T22 | browser (CDP) 浏览器自动化 | P0 | 12h | T05 | ❌ 未开始 |

**总预估**: ~89h
**已完成**: ~70h（T01-T18, T20, T21）
**剩余**: ~19h（T19, T22）

---

## 依赖关系图

```
T01 ─┬─► T02 ─┬─► T03 ─┬─► T06 ─┬─► T07
     │        │         │        ├─► T09 ─► T16
     │        ├─► T04 ──┘        ├─► T11 ─► T12
     │        └─► T05 ──┘        └─► T14 ─► T15
     │              │
     │              ├─► T17
     │              └─► T18
     └─► T08 ─┬─► T09
              └─► T10
T03 ─► T13 ─► T14
```

---

## 任务详情

### T01: Cargo Workspace 初始化 ✅

**验收标准**: `cargo check` 全部 5 个 crate 通过编译，无 warning。

子任务：
- [x] 创建根 `Cargo.toml`（workspace members: nova-core, nova-api, nova-daemon, nova-tui, nova-ipc）
- [x] 创建 `nova-core/Cargo.toml` + `nova-core/src/lib.rs`（19 个 pub mod）
- [x] 创建 `nova-api/Cargo.toml` + `nova-api/src/lib.rs`（client, types, stream）
- [x] 创建 `nova-daemon/Cargo.toml` + `nova-daemon/src/main.rs`
- [x] 创建 `nova-tui/Cargo.toml` + `nova-tui/src/main.rs`
- [x] 创建 `nova-ipc/Cargo.toml` + `nova-ipc/src/lib.rs`
- [x] 配置 workspace.dependencies 共享依赖

> **注**: 实际 crate 命名从原设计的 runtime/daemon/tui/ipc 改为 nova-core/nova-api/nova-daemon/nova-tui/nova-ipc（5 个），nova-api 独立拆出。

---

### T02: 配置加载 ✅

**验收标准**: 能从 `~/.nova/config` 加载 TOML 配置，缺失字段有合理默认值。

子任务：
- [x] 定义 `NovaConfig` 结构体（api_key, model, api_base_url, context_window, workspace, heartbeat_interval_secs, char_delay_ms, max_turns, tool_timeout_secs, compact_target_pct, budget_trigger_pct）
- [x] 实现 `NovaConfig::load()` — 读取 `~/.nova/config`，TOML 反序列化
- [x] 实现默认值（model="MiniMax-M2.7", context_window=200000, heartbeat=300s, char_delay=5ms）
- [x] 路径展开（`~` → home dir）

---

### T03: API 客户端 + SSE Streaming ✅

**验收标准**: 能向 Anthropic 兼容 API 发送请求，正确解析 SSE 流式响应，提取 text delta 和 tool_calls，返回 Usage 统计。

子任务：
- [x] 定义 API 类型：`ApiRequest`, `StreamEvent`, `ContentBlock`, `Delta`, `Usage`（nova-api/src/types.rs）
- [x] 实现 `ApiClient::new(config)` — 构建 reqwest client，设置 headers（x-api-key, anthropic-version）
- [x] 实现 `ApiClient::stream()` — POST 请求，返回 SSE 流式事件
- [x] SSE 解析：按 `data: ` 前缀逐行解析
- [x] 从流中收集完整 `AssistantMessage`（text + tool_calls）— AccumulatedToolCall
- [x] 提取 `Usage`（input_tokens, output_tokens）
- [x] 支持 thinking block（MiniMax M2.7 特性）
- [x] 实现 `ApiClient::complete()` — 非流式调用
- [x] context overflow 错误检测

---

### T04: Workspace 加载 + System Prompt ✅

**验收标准**: 能加载 `~/.nova/` 下所有 workspace 文件，按正确顺序拼接 system prompt，缺失文件跳过不报错，支持热加载。

子任务：
- [x] 实现 `BootstrapLoader` — 带 mtime 缓存的热加载器（nova-core/src/workspace/loader.rs）
- [x] 每次 API 请求前检查文件 mtime，变化时重新读取
- [x] 注入顺序：SOUL → IDENTITY → AGENTS → USER → STATE → TASKS
- [x] 截断规则：单文件 20K 字符，总量 150K 字符，头部 70% + 尾部 20% + 中间截断标记
- [x] 安全 UTF-8 截断（不切断多字节字符）
- [x] 实现 `PromptBuilder` — system prompt 拼接（nova-core/src/agent/prompt.rs）

---

### T05: 工具系统（trait + registry + 5 个工具） ✅

**验收标准**: Tool trait 可扩展，ToolRegistry 保持稳定排序，5 个内置工具可独立执行并返回正确结果。

子任务：
- [x] 定义 `Tool` trait（name, description, input_schema, execute）
- [x] 实现 `ToolRegistry`（register_builtin, register_mcp, as_api_tools, execute）
- [x] `as_api_tools()` 保证 builtin 在前、MCP 在后的稳定排序
- [x] 实现 `BashTool` — 调用 `tokio::process::Command`，Open/Sandbox 双模式
- [x] 实现 `ReadFileTool` — 读文件，支持 start_line/end_line，1MB 限制
- [x] 实现 `WriteFileTool` — 写文件（全量覆盖 / 追加模式），自动创建目录
- [x] 实现 `GlobTool` — 使用 glob crate 搜索文件
- [x] 实现 `GrepTool` — 封装 ripgrep，fallback 到 grep，200 行截断

> **注**: 原设计 4 个工具，实际实现 5 个（新增 GrepTool）。

---

### T06: Query Loop 核心 ✅

**验收标准**: 输入用户消息后，能完成完整的 loop（API 调用 → 解析 → 工具执行 → 追加结果 → 继续），max_turns=20 时正确停止，工具超时 60s 正确处理。

子任务：
- [x] 实现 `QueryLoop::run()` 主循环（~280 行）
- [x] 构建 messages 数组（system + history + new user msg）
- [x] 调用 `ApiClient::stream()` 获取响应
- [x] 解析 tool_calls，调用 `ToolRegistry::execute()`
- [x] 工具执行超时控制（`tokio::time::timeout`）
- [x] 工具失败 → 返回错误 JSON，不中断 loop
- [x] turn 计数 + max_turns 检查
- [x] 通过 `mpsc::Sender<LoopEvent>` 发送流式事件
- [x] Pre-flight / Post-flight token budget 检查
- [x] Context overflow 自动 compact + retry
- [x] 空响应重试（最多 3 次）
- [x] PostSampling / StopHooks 集成

---

### T07: Session 持久化（JSONL） ✅

**验收标准**: 每条消息实时追加到 `~/.nova/sessions/<uuid>.jsonl`，重启后能恢复最近 session，元数据文件正确更新。

子任务：
- [x] 实现 `SessionHistory::append()` — 追加一行 JSON 到 .jsonl 文件
- [x] 实现 `SessionHistory::load()` — 逐行解析 .jsonl 恢复 messages
- [x] 实现 `SessionManager::resume_latest()` — 按 meta.json 的 updated_at 找最近 session
- [x] 实现 `SessionManager::save_meta()` — 写入/更新 .meta.json
- [x] Session 生命周期：新建时生成 UUID，每条消息 append，退出时更新 meta

---

### T08: IPC 协议层 ✅

**验收标准**: `Request` 和 `Event` 类型可序列化/反序列化，JSON lines 编码/解码正确，能通过 Unix Socket 双向传输。

子任务：
- [x] 定义 `Request` enum（UserMessage, ResumeSession, NewSession, SearchSessions, Shutdown）
- [x] 定义 `Event` enum（TextDelta, ToolCallStart, ToolCallResult, TurnEnd, Notification, Error, TokenUsage, SessionRestored, SessionCreated, SearchResults）
- [x] 实现 JSON lines 协议（`\n` 分隔）
- [x] 实现 `IpcServer` — Unix Socket 服务端，stale socket 清理
- [x] 实现 `IpcClient` — Unix Socket 客户端
- [x] 双向 send/recv

> **注**: 实际使用 JSON lines 协议（`\n` 分隔），而非原设计的 4 字节长度前缀。

---

### T09: 守护进程 ✅

**验收标准**: daemon 启动后监听 `/tmp/nova.sock`，TUI 连接后能收发消息，支持 session 恢复/新建。

子任务：
- [x] `nova-daemon/src/main.rs` — 主入口，日志写入 ~/.nova/daemon.log
- [x] 绑定 Unix Socket，accept 连接，tokio::spawn 处理
- [x] 收到 `Request::UserMessage` → 调用 `QueryLoop::run()`，流式发送 `Event`
- [x] 收到 `Request::ResumeSession` → 加载最近 session
- [x] 收到 `Request::NewSession` → 创建新 session
- [x] 收到 `Request::Shutdown` → 优雅退出
- [x] PID 文件：`/tmp/nova.pid`
- [x] 工具注册（bash, read_file, write_file, glob, grep）
- [x] Hook 初始化（PostSampling + Stop）
- [x] Skill 注入（auto-trigger + 手动 /skill）
- [x] Agentic Session Search 自动搜索 + 历史注入（context window 5% 预算）

---

### T10: 赛博朋克 TUI ✅

**验收标准**: `kiko` 启动后显示赛博朋克风格界面，能输入消息、显示流式响应（打字机效果）、显示 token 使用率、滚动历史消息。

子任务：
- [x] `nova-tui/src/app.rs` — App 状态管理（messages, commands, focus, input, scroll, token usage）
- [x] `nova-tui/src/main.rs` — IPC 连接 + 主渲染循环（16-20ms 刷新）
- [x] `nova-tui/src/ui.rs` — 三区域布局（状态栏 + 聊天区/命令区 + 输入框），Budget 进度条
- [x] `nova-tui/src/theme.rs` — 赛博朋克配色（cyan/purple/magenta，10 个颜色常量）
- [x] `nova-tui/src/input.rs` — 键盘处理（Enter 发送，Ctrl+C 退出，方向键滚动）
- [x] 打字机效果：逐字符渲染 TextDelta（1-2 chars/frame）
- [x] 状态栏：Token 使用百分比进度条
- [x] 命令处理：/quit, /new, /search
- [x] Unicode 宽度处理（CJK 字符）
- [x] Session 恢复

---

### T11: Token Budget 双阈值 ✅

**验收标准**: input_tokens 超过 context 90% 时返回 `NeedsCompact`，单轮 token 超过上轮 3 倍时返回 `Diminishing`。

子任务：
- [x] 实现 `TokenBudget` 结构体
- [x] 实现 `check()` — 阈值 1（90% budget）检测
- [x] 实现 `check()` — 阈值 2（3x 边际递减）检测
- [x] 集成到 QueryLoop：每轮开始前调用 check()
- [x] NeedsCompact → 调用 Compactor
- [x] Diminishing → 停止循环

---

### T12: Compact 对话压缩 ✅

**验收标准**: 触发 compact 后，messages 被压缩到 context 60% 以下，system prompt 保留，最近消息保留，早期消息变为摘要，tool_call/tool_result 配对不被切割，防重入生效。

子任务：
- [x] 实现 `Compactor::compact()` 主逻辑
- [x] 分割点计算：安全切割点（不切 tool_call 配对）
- [x] 早期消息 → 调用 LLM 生成摘要
- [x] 替换早期消息为摘要 User message
- [x] 防重入：`AtomicBool` CAS 保护
- [x] API context-overflow 错误时自动触发 compact + retry（在 QueryLoop 中）

---

### T13: Forked Agent ✅

**验收标准**: `ForkedAgent::spawn()` 能在后台执行任务，不阻塞主 loop，独立 token 预算，失败可重试。

子任务：
- [x] 实现 `ForkedAgent` 结构体
- [x] `spawn()` — `tokio::spawn` 包装
- [x] 重试逻辑：指数退避，可配置 max_retries
- [x] `spawn_oneshot()` — 单次执行变体

---

### T14: Hooks 系统（PostSampling + Stop） ✅

**验收标准**: PostSamplingHook 在每次 LLM 响应后异步执行（不阻塞），StopHook 在 turn 结束后串行执行（阻塞），Hook 可动态注册。

子任务：
- [x] 定义 `PostSamplingHook`, `StopHook` trait
- [x] 实现 `HookManager`（register, fire_post_sampling, fire_stop）
- [x] `fire_post_sampling()` — 异步执行，不阻塞主 loop，错误仅 log
- [x] `fire_stop()` — 串行 await 每个 hook，错误仅 log
- [x] 实现 `MemoryExtractHook`（PostSampling）— 提取响应关键信息写入记忆，>200 字符过滤
- [x] 实现 `MemoryExtractStopHook`（Stop）— turn 结束时写入记忆，工具动作追踪

---

### T15: 双写互斥记忆 ✅

**验收标准**: 主 agent 写了记忆后 forked agent 不再写，主 agent 没写时 forked agent 兜底写入，绝不重复写入同一条记忆。

子任务：
- [x] 实现 `DualWriteMemory` 结构体
- [x] `has_writes_since()` — 检查本 turn 是否已有写入
- [x] `write()` — 追加到 `~/.nova/memories/<type>.jsonl`
- [x] `mark_written()` / `clear_marker()` — 标记/重置写入状态
- [x] 4 种记忆类型：User, Feedback, Project, Reference
- [x] 集成到 MemoryExtractHook 和 MemoryExtractStopHook
- [x] `MemoryStore` — 通用 JSONL 追加存储

---

### T16: Heartbeat 机制 ✅

**验收标准**: 按配置间隔周期执行 HEARTBEAT.md 中定义的任务，结果通过 channel 推送。

子任务：
- [x] 解析 HEARTBEAT.md 中的任务定义（`## TaskName` 格式）
- [x] 实现 `HeartbeatScheduler` — `tokio::time::interval` 周期触发
- [x] `start()` — 后台 tokio task，每次心跳遍历任务列表
- [x] 通过 `mpsc::Sender<HeartbeatEvent>` 推送事件

---

### T17: Skills 系统 ✅

**验收标准**: 能加载 `~/.nova/skills/<name>/SKILL.md`，支持手动 `/skill-name` 触发和 auto-trigger。

子任务：
- [x] 实现 `SkillsLoader::load_all()` — 扫描 `~/.nova/skills/` 目录
- [x] 解析 SKILL.md 中的 prompt 模板
- [x] `find_by_name()` — `/skill-name` 命令匹配
- [x] `match_auto_trigger()` — 关键词自动触发
- [x] auto_trigger frontmatter 解析（keywords + path_patterns）

---

### T18: bash 受限模式安全加固 ✅

**验收标准**: 危险命令被拦截，`~/.nova/` 路径被保护，双模式（Open/Sandbox）可切换。

子任务：
- [x] 实现 Open / Sandbox 双模式
- [x] 灾难性命令拦截（rm -rf /, mkfs, dd if= 等）
- [x] 命令替换拦截（$(), ``, <(), >()）
- [x] 权限提升拦截（sudo, su, doas）
- [x] Sandbox 白名单（50+ 安全命令）
- [x] `~/.nova/` 路径保护

---

### T19: 端到端集成测试 ❌

**验收标准**: 完整流程可跑通 — 启动守护进程 → TUI 连接 → 发送消息 → 收到流式响应 → Session 持久化 → 重启恢复。

子任务：
- [ ] 测试：daemon 启动 + TUI 连接 + 消息收发
- [ ] 测试：Session JSONL 写入 + 恢复
- [ ] 测试：Token Budget 触发 Compact
- [ ] 测试：双写互斥（主 agent 写 → forked 跳过）
- [ ] 测试：bash 受限模式拦截危险命令
- [ ] 测试：Heartbeat 周期触发

---

### T20: file_edit 精确编辑工具

**验收标准**: `file_edit` 工具能精确替换文件中的字符串，`old_string` 不唯一时报错，`replace_all=true` 时全部替换，保留缩进。

子任务：
- [x] 实现 `FileEditTool` 结构体，实现 `Tool` trait
- [x] 参数解析：file_path、old_string、new_string、replace_all
- [x] 核心逻辑：读文件 → 查找匹配次数 → 替换 → 写回
- [x] 错误处理：old_string 未找到、多次匹配但 replace_all=false
- [x] 在 `tools/mod.rs` 中导出，在 daemon `make_tools()` 中注册
- [x] 返回 diff 信息（替换次数、文件路径）

---

### T21: 三层记忆系统

**验收标准**: MEMORY.md 始终注入 system prompt 且 LLM 可主动维护；Compact 前和 Session 结束时自动写入日记（memories/YYYY-MM-DD.md）；召回管线能从日记中选相关内容注入 system prompt；Dream 能定期整理 MEMORY.md 和日记。

#### T21.1: 日记写入机制 ✅

- [x] 改造 `memory/daily.rs` — 写入 `~/.nova/memories/YYYY-MM-DD.md`（markdown 格式，带时间戳标题）
- [x] Compact 前自动写入 — 在 `Compactor::compact()` 调用前，用 SideQuery 生成即将被压缩的消息摘要，追加到当天日记
- [x] Session 结束时自动写入 — 在 daemon 处理 NewSession / TUI 断开时，用 SideQuery 生成本次对话摘要，追加到当天日记
- [x] SideQuery prompt：从最近 N 条消息中提取关键决策/事件/发现，生成简洁的 markdown 摘要
- [x] 停用旧记忆系统：移除 daemon 中 DualWriteMemory / MemoryExtractHook / MemoryExtractStopHook 的注册，不再写 JSONL

#### T21.2: 记忆召回管线 ✅

- [x] 实现 `memory/recall.rs` — MemoryRecall 结构体
- [x] `scan_diaries()` — 扫描 memories/*.md 文件名（日期）+ 读取首行标题
- [x] `find_relevant()` — SideQuery 调 LLM 从日记列表中选最相关的（最多 3 天）
- [x] `format_injection()` — 读取选中日记内容，格式化为 `<relevant_memories>` XML 块
- [x] 在 daemon 中集成 — 与 Agentic Session Search 并行执行，10 秒超时

#### T21.3: MEMORY.md prompt 引导 ✅

- [x] 在 `workspace/loader.rs` 中添加记忆系统使用说明（MEMORY_GUIDANCE）
- [x] 引导 LLM 用 file_edit/write_file 主动维护 MEMORY.md
- [x] 描述四种记忆类型（user/feedback/project/reference）和写入时机

#### T21.4: Dream 记忆整理 ✅

- [x] 实现 `memory/dream.rs` — Dream 整理引擎
- [x] 触发条件检查：距上次 ≥24h + ≥5 个新 session（锁文件 `.dream-lock`）
- [x] 整理流程：Orient → Gather → Consolidate → Prune（用 SideQuery/forked agent）
- [x] 从 memories/*.md 日记提炼核心事项更新 MEMORY.md
- [x] 日记摘要索引生成/更新
- [x] MEMORY.md 保持 <200 行
- [x] 在 daemon UserMessage 处理中集成触发检查（每 turn 检查）
- [x] `/dream` 手动触发支持（Request::Dream 枚举待添加）
- [x] 双写互斥：主 agent 本 turn 已写 MEMORY.md → Dream 跳过 MEMORY.md 更新（追踪 MEMORY.md 的 mtime，turn 开始时记录，Dream 写入前比对）

---

### T22: browser (CDP) 浏览器自动化

**验收标准**: `browser` 工具能启动 Chrome、导航到 URL、获取页面快照、截图、执行点击/输入操作。

子任务：
- [ ] 添加 `chromiumoxide` 依赖到 `nova-core/Cargo.toml`
- [ ] 实现 `browser/chrome.rs` — Chrome 进程管理（启动/停止/检测可执行文件）
- [ ] 实现 `browser/cdp.rs` — CDP 连接管理（连接/断开/重连）
- [ ] 实现 `browser/actions.rs` — 各 action 实现
  - [ ] `navigate(url)` — 导航到 URL
  - [ ] `snapshot()` — 获取页面可见文本 + 链接列表
  - [ ] `screenshot(full_page, selector)` — 截图，返回 PNG 文件路径
  - [ ] `act(click, selector)` — 点击元素
  - [ ] `act(type, selector, text)` — 输入文本
  - [ ] `act(press, key)` — 按键
- [ ] 实现 `browser/tool.rs` — BrowserTool，action 参数分发
- [ ] 在 `tools/mod.rs` 中导出，在 daemon `make_tools()` 中注册
- [ ] Chrome profile 隔离（`~/.nova/browser/nova-profile/`）
- [ ] daemon 退出时自动 kill Chrome 进程

---

## Phase 2/3 骨架模块状态

> 以下模块在 requirements.md 中标记为"骨架已搭建"，实际均有完整的数据结构和核心逻辑实现。

| 模块 | 文件 | 实际状态 | 说明 |
|:---|:---|:---|:---|
| Team 系统 | `nova-core/src/team/` | ✅ 数据结构 + CRUD + 持久化 | TeamManager/Team/Task/Mailbox 均可用，待集成 QueryLoop |
| Subagent | `nova-core/src/subagent/` | ✅ spawn + 并行执行 | SubagentSpawner 可 spawn 单个/并行子 agent，待集成 Team |
| SideQuery | `nova-core/src/sidequery/` | ✅ 已完整实现 | 同步/异步查询，已接入 Agentic Session Search |
| autoDream | `nova-core/src/dream/` | ✅ 完整逻辑 | 空闲检测 + SideQuery 建议生成，待接入 daemon/TUI |
| Worktree | `nova-core/src/worktree/` | ✅ 完整逻辑 | git worktree 创建/清理/Drop，待接入 Session |
| Coordinator | `nova-core/src/coordinator/` | ✅ 四阶段流水线 | Research→Synthesis→Implementation→Verification，待接入工具 |
| Paste Store | `nova-core/src/paste/` | ✅ hash 去重 + 引用标签 | 待接入 TUI 粘贴事件 |
| Heartbeat | `nova-core/src/heartbeat/` | ✅ 已完整实现 | 解析 + 调度 + 后台 task |
| Skills | `nova-core/src/skills/` | ✅ 已完整实现 | 加载 + 手动/自动触发 |
| Sandbox | `nova-core/src/sandbox/` | ✅ 策略完整 | 四级策略 + 路径/写入/网络检查 |
| Retry | `nova-core/src/retry/` | ✅ 已完整实现 | 指数退避 + 可重试错误匹配 |
| Daily Notes | `nova-core/src/memory/daily.rs` | ✅ 已完整实现 | 每日笔记追加/读取 |
| Session Search | `nova-core/src/session/search.rs` | ✅ 已完整实现 | Agentic Search，已接入 daemon |

---

## 里程碑

| 里程碑 | 完成任务 | 预期成果 | 状态 |
|:---|:---|:---|:---|
| M1: 骨架可编译 | T01, T02, T08 | `cargo check` 通过，配置可加载 | ✅ |
| M2: API 可调用 | T03, T04, T05 | 能发 API 请求，工具可执行 | ✅ |
| M3: Loop 可跑 | T06, T07 | 完整 Query Loop + Session 持久化 | ✅ |
| M4: 可交互 | T09, T10 | 守护进程 + TUI，用户可对话 | ✅ |
| M5: 策略完整 | T11-T15 | 8 个核心策略全部实现 | ✅ |
| M6: 功能完整 | T16, T17, T18 | Heartbeat + Skills + 安全 | ✅ |
| M7: 质量保证 | T19 | 集成测试通过 | ❌ 未开始 |
| M8: P0 工具 | T20, T21, T22 | file_edit + 三层记忆 + 浏览器 | 🔧 T20+T21 完成，T22 进行中 |
