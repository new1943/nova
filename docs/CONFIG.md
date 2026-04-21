# NOVA 配置参数详解手册

**版本**: v2.0
**日期**: 2026-04-19
**状态**: 持续更新

> 详细说明每个参数的机制、作用、取值范围及调优建议

---

## 一、Agent / Query Loop

### 1.1 核心循环参数

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_TURNS` | `agent/loop.rs` | 20 | 单次对话的最大轮数限制 |
| `MAX_EMPTY_RETRIES` | `agent/loop.rs` | 3 | 空响应重试次数 |
| `MAX_TOOL_CHARS` | `agent/loop.rs` | 30,000 | 工具输出字符数上限（超过则截断） |

#### `MAX_TURNS`

**机制**：每执行一次 tool_call 算作 1 turn，当 turn 数达到上限时强制退出 loop。

**作用**：
- 防止无限循环（LLM 一直调用工具不停止）
- 控制单次对话的最大工作量

**调优建议**：
- 复杂任务可调大（如 50-100）
- 简单问答可调小（如 10）

#### `MAX_EMPTY_RETRIES`

**机制**：当 LLM 返回空响应（非 stop_reason 正常结束）时，重试次数。

**作用**：
- 处理 LLM 异常情况
- 避免因网络抖动导致对话中断

---

## 二、Token Budget / Compact

### 2.1 Token Budget 双阈值

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `budget_trigger_pct` | `config.rs` | 0.9 (90%) | 触发 Compact 的水位线 |
| `compact_target_pct` | `config.rs` | 0.6 (60%) | Compact 后的目标保留比例 |

#### `budget_trigger_pct`

**机制**：当 `input_tokens / context_window > budget_trigger_pct` 时，触发 Compact。

**作用**：
- 预留 context 空间，防止 OOM
- 90% 是经验值：留 10% 空间给 system prompt 和响应

**调优建议**：
- 需要更多 context → 调低（如 0.85）
- 追求稳定性 → 调高（如 0.95）

#### `compact_target_pct`

**机制**：Compact 后保留最近 60% 的消息。

**作用**：
- 控制 Compact 后 context 大小
- 60% 意味着压缩掉 40% 的早期消息

**调优建议**：
- 长对话场景 → 调低（如 0.5）
- 短对话场景 → 调高（如 0.7）

### 2.2 Compact 双层熔断

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `GRACEFUL_THRESHOLD` | `compact.rs` | 0.85 (85%) | Graceful 模式上限 |
| `FORCEFUL_THRESHOLD` | `compact.rs` | 0.95 (95%) | Forceful 模式触发线 |

#### 双层熔断机制

```
Token 水位 < 85%  →  正常，不触发 Compact
85% ~ 95%      →  Graceful 模式：调用 LLM 生成结构化 JSON
> 95%           →  Forceful 模式：直接丢弃旧消息
```

**Graceful 模式**：
- 调用 LLM 生成结构化 JSON（archived_topics + extracted_preferences + active_summary）
- 写入记忆系统
- 保留对话上下文

**Forceful 模式**：
- 直接 `Vec::drain` 丢弃最旧的 40% 消息
- 不调用 LLM
- 插入警告消息

---

## 三、话题状态机 (Topic State Machine)

### 3.1 话题状态

| 状态 | 说明 |
|:---|:---|
| `Started` | 新话题启动 |
| `Active` | 话题进行中 |
| `Suspended` | 话题暂停，可恢复 |
| `Archived` | 话题结束，已归档 |

### 3.2 切换信号词

| 参数 | 文件 | 默认值 |
|:---|:---|:---|
| `TOPIC_SWITCH_SIGNALS` | `topic_state.rs` | 好/搞定/下一个/换个话题/先这样/好了/结束/完成 |

**机制**：用户消息包含信号词时，自动归档当前话题。

### 3.3 自动挂起

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `auto_suspend_turns` | `topic_state.rs` | 10 | 连续 N 轮无工具调用后自动挂起 |

---

## 四、张力值追踪 (Tension Tracker)

### 4.1 张力值公式

```
tension = intimacy * 0.4 + trust * 0.2 + emotion_weight + context_weight + intent_boost - gap_penalty
```

| 变量 | 权重 | 说明 |
|:---|:---|:---|
| `intimacy` | 40% | 用户亲密度 (0-100) |
| `trust` | 20% | 信任度 (0-100) |
| `emotion_weight` | 20% | 情绪权重 |
| `context_weight` | 20% | 场景权重 |
| `intent_boost` | +N | 意图加成 |
| `gap_penalty` | -15/-25 | 长时间未交互惩罚 |

### 4.2 情绪权重

| 情绪 | 权重值 |
|:---|:---|
| `Calm` | 0 |
| `Excited` | 15 |
| `Anxious` | 25 |
| `Frustrated` | 30 |
| `Tense` | 35 |

### 4.3 场景权重

| 场景 | 权重值 |
|:---|:---|
| `Morning` | 5 |
| `Afternoon` | 10 |
| `Evening` | 15 |
| `Night` | 25 |
| `Working` | 20 |

### 4.4 模式切换

| 张力值 | 模式 | 响应风格 |
|:---|:---|:---|
| < 50 | `Normal` | 简洁直接，专注任务 |
| 50-75 | `SoftIntimate` | 稍温和，轻关怀 |
| > 75 | `HighIntimate` | 明显关怀，注意情绪 |

---

## 五、Workspace Bootstrap

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_PER_FILE_CHARS` | `loader.rs` | 20,000 | 单个文件最大字符数 |
| `MAX_TOTAL_CHARS` | `loader.rs` | 150,000 | 所有文件总字符数上限 |
| `HEAD_RATIO` | `loader.rs` | 0.7 (70%) | 截断时头部保留比例 |
| `TAIL_RATIO` | `loader.rs` | 0.2 (20%) | 截断时尾部保留比例 |
| `MIN_FILE_BUDGET` | `loader.rs` | 64 | 文件最小预算 |

#### 截断逻辑

当文件超过 `MAX_PER_FILE_CHARS` 时：
```
保留头部 = MAX_PER_FILE_CHARS * 70% = 14,000 chars
保留尾部 = MAX_PER_FILE_CHARS * 20% = 4,000 chars
省略中间 = 2,000 chars
```

---

## 六、Session Search

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_TRANSCRIPT_CHARS` | `search.rs` | 1,000 | 单个 session 的 transcript 最大字符数 |
| `MAX_MESSAGES_TO_SCAN` | `search.rs` | 200 | 每个 session 最多扫描的消息数 |
| `MAX_SESSIONS_TO_SEARCH` | `search.rs` | 50 | 最多搜索的 session 数 |

---

## 七、Tools

### 7.1 bash

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_BASH_OUTPUT_CHARS` | `constants.rs` | 20,000 | bash 输出最大字符数 |
| `BASH_HEAD_CHARS` | `constants.rs` | 8,000 | bash 截断头部保留 |
| `BASH_TAIL_CHARS` | `constants.rs` | 8,000 | bash 截断尾部保留 |

### 7.2 browser

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_BROWSER_OUTPUT_CHARS` | `constants.rs` | 15,000 | browser 输出最大字符数 |
| `BROWSER_HEAD_CHARS` | `constants.rs` | 5,000 | browser 截断头部保留 |
| `BROWSER_TAIL_CHARS` | `constants.rs` | 5,000 | browser 截断尾部保留 |
| `CDP_PORT` | `browser.rs` | 19222 | Chrome DevTools Protocol 端口 |

### 7.3 read_file

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_FILE_SIZE` | `read_file.rs` | 1,048,576 (1MB) | 读取文件最大字节数 |

### 7.4 glob

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_RESULTS` | `glob.rs` | 100 | 最多返回的结果数 |

### 7.5 grep

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_RESULTS` | `grep.rs` | 200 | 最多返回的匹配行数 |
| `VCS_DIRS` | `grep.rs` | .git/.svn/.hg/.sl | 自动排除的版本控制目录 |

---

## 八、Bash 安全

### 8.1 灾难性命令阻止

| 参数 | 文件 | 默认值 |
|:---|:---|:---|
| `BLOCKED_PATTERNS` | `security/constants.rs` | rm -rf /, mkfs, dd if=, shutdown, reboot, halt, poweroff 等 |

### 8.2 操作符阻止

| 参数 | 文件 | 默认值 |
|:---|:---|:---|
| `BLOCKED_OPERATORS` | `security/constants.rs` | $(, `, <(, >( |

### 8.3 Zsh 危险命令

| 参数 | 文件 | 说明 |
|:---|:---|:---|
| `ZSH_DANGEROUS_COMMANDS` | `security/constants.rs` | zmodload, emulate, zsysopen, zpty, ztcp, mapfile 等 20+ 个 |

### 8.4 沙箱模式白名单

| 参数 | 文件 | 说明 |
|:---|:---|:---|
| `SANDBOX_ALLOWED` | `security/constants.rs` | 沙箱模式下允许的 50+ 个安全命令 |

---

## 九、Memory / 记忆系统

### 9.1 MEMORY.md 白板

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `MAX_TOPICS` | `memory_board.rs` | 20 | MEMORY.md 中最大话题数 |

### 9.2 Dream 整理

| 参数 | 文件 | 默认值 | 说明 |
|:---|:---|:---|:---|
| `DREAM_LOCK_TIMEOUT` | `dream.rs` | 24h | 两次整理的最小间隔 |
| `MIN_SESSIONS_FOR_DREAM` | `dream.rs` | 5 | 触发整理所需的最小 session 数 |

---

## 十、截断警告文本

| 参数 | 文件 | 默认值 |
|:---|:---|:---|
| `BASH_TRUNCATION_WARNING` | `constants.rs` | `[... 约 N 字符因过长已省略...]` |
| `BROWSER_TRUNCATION_WARNING` | `constants.rs` | `[... 页面内容因过长已省略...]` |

---

## 十一、完整参数索引

### 按模块分类

```
Agent
├── MAX_TURNS = 20
├── MAX_EMPTY_RETRIES = 3
└── MAX_TOOL_CHARS = 30,000

Token Budget
├── budget_trigger_pct = 0.9 (90%)
├── compact_target_pct = 0.6 (60%)
├── GRACEFUL_THRESHOLD = 0.85 (85%)
└── FORCEFUL_THRESHOLD = 0.95 (95%)

Topic State
├── TOPIC_SWITCH_SIGNALS = ["好", "搞定", "下一个", ...]
└── auto_suspend_turns = 10

Tension Tracker
├── intimacy_weight = 0.4
├── trust_weight = 0.2
├── emotion_weight = [0, 35]
├── context_weight = [5, 25]
└── gap_penalty = [0, 25]

Workspace Bootstrap
├── MAX_PER_FILE_CHARS = 20,000
├── MAX_TOTAL_CHARS = 150,000
├── HEAD_RATIO = 0.7
├── TAIL_RATIO = 0.2
└── MIN_FILE_BUDGET = 64

Session Search
├── MAX_TRANSCRIPT_CHARS = 1,000
├── MAX_MESSAGES_TO_SCAN = 200
└── MAX_SESSIONS_TO_SEARCH = 50

Tools
├── bash: MAX_OUTPUT = 20,000, HEAD = 8,000, TAIL = 8,000
├── browser: MAX_OUTPUT = 15,000, HEAD = 5,000, TAIL = 5,000
├── read_file: MAX_SIZE = 1,048,576 (1MB)
├── glob: MAX_RESULTS = 100
└── grep: MAX_RESULTS = 200, VCS_DIRS = [.git, .svn, .hg, .sl]

Memory
├── MAX_TOPICS = 20
├── DREAM_LOCK_TIMEOUT = 24h
└── MIN_SESSIONS_FOR_DREAM = 5
```

### 按字母排序

| 参数 | 模块 | 默认值 |
|:---|:---|:---|
| `auto_suspend_turns` | Topic | 10 |
| `BASH_HEAD_CHARS` | bash | 8,000 |
| `BASH_TAIL_CHARS` | bash | 8,000 |
| `BASH_TRUNCATION_WARNING` | bash | 警告文本 |
| `BROWSER_HEAD_CHARS` | browser | 5,000 |
| `BROWSER_TAIL_CHARS` | browser | 5,000 |
| `BROWSER_TRUNCATION_WARNING` | browser | 警告文本 |
| `budget_trigger_pct` | Token | 0.9 |
| `CDP_PORT` | browser | 19222 |
| `compact_target_pct` | Token | 0.6 |
| `context_weight` | Tension | 5-25 |
| `emotion_weight` | Tension | 0-35 |
| `FORCEFUL_THRESHOLD` | Compact | 0.95 |
| `gap_penalty` | Tension | 0-25 |
| `GRACEFUL_THRESHOLD` | Compact | 0.85 |
| `HEAD_RATIO` | Bootstrap | 0.7 |
| `intimacy_weight` | Tension | 0.4 |
| `MAX_BASH_OUTPUT_CHARS` | bash | 20,000 |
| `MAX_BROWSER_OUTPUT_CHARS` | browser | 15,000 |
| `MAX_EMPTY_RETRIES` | Agent | 3 |
| `MAX_FILE_SIZE` | read_file | 1,048,576 |
| `MAX_MESSAGES_TO_SCAN` | Search | 200 |
| `MAX_PER_FILE_CHARS` | Bootstrap | 20,000 |
| `MAX_RESULTS` | glob | 100 |
| `MAX_RESULTS` | grep | 200 |
| `MAX_SESSIONS_TO_SEARCH` | Search | 50 |
| `MAX_TIME_BUDGET` | Bootstrap | 64 |
| `MAX_TOOL_CHARS` | Agent | 30,000 |
| `MAX_TRANSCRIPT_CHARS` | Search | 1,000 |
| `MAX_TOTAL_CHARS` | Bootstrap | 150,000 |
| `MAX_TURNS` | Agent | 20 |
| `MIN_FILE_BUDGET` | Bootstrap | 64 |
| `MIN_SESSIONS_FOR_DREAM` | Dream | 5 |
| `TAIL_RATIO` | Bootstrap | 0.2 |
| `TOPIC_SWITCH_SIGNALS` | Topic | 信号词列表 |
| `trust_weight` | Tension | 0.2 |
| `VCS_DIRS` | grep | 版本控制目录 |
