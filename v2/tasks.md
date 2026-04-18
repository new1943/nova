# NOVA v2 任务清单

**版本**: v2.0
**日期**: 2026-04-18
**目标**: 智能体防爆与进化
**状态**: 设计完成，待开发

---

## 任务总览

| ID | 任务 | 优先级 | 预估 | 依赖 | 状态 |
|:---|:---|:---|:---|:---|:---|
| T01 | 截断常量定义 (constants.rs) | P0 | 0.5h | — | ❌ 未开始 |
| T02 | 截断函数实现 (truncate.rs) | P0 | 1h | T01 | ❌ 未开始 |
| T03 | bash.rs I/O Shield 集成 | P0 | 1h | T02 | ❌ 未开始 |
| T04 | browser.rs I/O Shield 集成 | P0 | 1h | T02 | ❌ 未开始 |
| T05 | compact.rs 双层熔断机制 | P0 | 3h | T01-T04 | ❌ 未开始 |
| T06 | compact.rs JSON 结构化输出 | P0 | 4h | T05 | ❌ 未开始 |
| T07 | 话题状态机 (topic_state.rs) | P1 | 3h | — | ❌ 未开始 |
| T08 | daily.rs 话题时间线格式 | P1 | 2h | T07 | ❌ 未开始 |
| T09 | MEMORY.md 白板化 (memory_board.rs) | P1 | 2h | T06, T07 | ❌ 未开始 |
| T10 | 张力值追踪器 (tension_tracker.rs) | P2 | 2h | — | ❌ 未开始 |
| T11 | 模式路由 (mode_router.rs) | P2 | 2h | T10 | ❌ 未开始 |
| T12 | SOUL.md 状态机 Prompt 更新 | P2 | 1h | T11 | ❌ 未开始 |
| T13 | <nova_os> 思考管道 | P2 | 2h | T11, T12 | ❌ 未开始 |
| T14 | 集成测试 | P0 | 4h | T01-T06 | ❌ 未开始 |
| T15 | Async Rust 锁安全改造 | P0 | 1h | T07, T09 | ❌ 未开始 |
| T16 | TUI 客户端 <nova_os> 拦截器 | P1 | 2h | T13 | ❌ 未开始 |
| T17 | JSON 解析 Markdown 清洗函数 | P0 | 0.5h | T06 | ❌ 未开始 |
| T18 | Token 精确计数（tiktoken 集成） | P0 | 2h | T05 | ❌ 未开始 |

---

## Phase 1：物理防御层

### T01: 截断常量定义

**文件**: `nova-core/src/tools/constants.rs`（新建）

**内容**:
```rust
pub const MAX_BASH_OUTPUT_CHARS: usize = 20_000;
pub const BASH_HEAD_CHARS: usize = 8_000;
pub const BASH_TAIL_CHARS: usize = 8_000;

pub const MAX_BROWSER_OUTPUT_CHARS: usize = 15_000;
pub const BROWSER_HEAD_CHARS: usize = 5_000;
pub const BROWSER_TAIL_CHARS: usize = 5_000;

pub const BASH_TRUNCATION_WARNING: &str = "[... 约 {n} 字符因过长已省略。如需查看完整输出，请使用 grep 搜索指定行号，或用 read_file 的 start_line/end_line 参数读取特定范围。]";

pub const BROWSER_TRUNCATION_WARNING: &str = "[... 页面内容因过长已省略。如需查看特定区域，请使用 click 点击目标元素，或用 navigate 直接访问相关 URL。]";
```

**验收标准**: 编译通过，常量值符合 requirements.md 设计

---

### T02: 截断函数实现

**文件**: `nova-core/src/tools/truncate.rs`（新建）

**函数签名**:
```rust
pub fn truncate_output(
    output: &str,
    max_chars: usize,
    head_chars: usize,
    tail_chars: usize,
    warning: &str,
) -> String
```

**逻辑**:
1. 如果 `output.len() <= max_chars`，直接返回
2. 否则：`head + warning(含实际省略字数) + tail`

**验收标准**:
- `truncate_output("abc", 10, 4, 3, "...")` → `"abc"`
- `truncate_output("abcdefghij", 6, 3, 2, "[..N..]")` → `"abc[..4..]ij"`

---

### T03: bash.rs I/O Shield 集成

**文件**: `nova-core/src/tools/bash.rs`

**改造点**:
1. import `constants.rs` 和 `truncate.rs`
2. 在 `execute()` 方法的输出返回前，调用 `truncate_output()`
3. 如果截断了，记录 `tracing::warn!`

**验收标准**:
- 正常输出不受影响
- 超长输出被截断，头尾保留
- 日志显示截断前后长度

---

### T04: browser.rs I/O Shield 集成

**文件**: `nova-core/src/tools/browser.rs`

**改造点**:
1. import `constants.rs` 和 `truncate.rs`
2. 在 `parse_mcp_response()` 返回前，调用 `truncate_output()`

**验收标准**: 同 T03

---

### T05: compact.rs 双层熔断机制

**文件**: `nova-core/src/token/compact.rs`

**改造点**:

1. 新增 `CompactMode` 枚举:
   ```rust
   pub enum CompactMode {
       Graceful,   // 85% ~ 95%
       Forceful,   // >95%
   }
   ```

2. 新增 `decide_mode()` 函数:
   ```rust
   fn decide_mode(budget_pct: f32) -> CompactMode
   ```

3. 改造 `compact()` 签名:
   ```rust
   pub async fn compact(
       &self,
       messages: &[Message],
       context_window: usize,
       budget_pct: f32,
   ) -> Result<Vec<Message>>
   ```

4. 新增 `graceful_compact()` 和 `forceful_compact()` 方法

5. 保留原有 `summarize()` 作为 fallback

**验收标准**:
- 水位 92% 时走 Graceful 模式
- 水位 97% 时走 Forceful 模式
- JSON 解析失败时回退到 forceful

---

### T06: compact.rs JSON 结构化输出

**文件**: `nova-core/src/token/compact.rs`

**新增类型**:
```rust
#[derive(serde::Deserialize, Debug)]
pub struct CompactResult {
    pub archived_topics: Vec<String>,
    pub extracted_preferences: Vec<String>,
    pub active_summary: String,
}
```

**新增方法**:
1. `llm_structured_summary()` — 调用 LLM 生成 JSON
2. `write_memory()` — 写入日记和 MEMORY.md
3. `graceful_compact()` — 完整优雅压缩流程

**Prompt**:
```
你正在执行记忆整理。请分析历史对话，输出严谨JSON：
{
  "archived_topics": ["话题名1", "话题名2"],
  "extracted_preferences": ["偏好1", "偏好2"],
  "active_summary": "当前话题的一句话描述"
}
要求：
- archived_topics：已完结或明显不再讨论的话题
- extracted_preferences：极其确定的用户偏好和事实，切勿臆测
- active_summary：当前仍在继续的话题摘要
只输出JSON，不要其他文字。
```

**验收标准**:
- LLM 输出正确的 JSON 格式
- `archived_topics` 写入 `memories/YYYY-MM-DD.md`
- `extracted_preferences` 写入 `MEMORY.md`
- `active_summary` 插入到压缩后的消息中

---

## Phase 1.5：记忆重塑层

### T07: 话题状态机

**文件**: `nova-core/src/memory/topic_state.rs`（新建）

**类型**:
```rust
pub enum TopicStatus {
    Started,
    Active,
    Suspended,
    Archived,
}

pub struct Topic {
    pub id: String,
    pub name: String,
    pub status: TopicStatus,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub summary: Option<String>,
    pub conclusions: Vec<String>,
}

pub enum TopicTransition {
    Continue,
    NewTopic,
    Suspend,
    Archive,
}
```

**TopicTracker 方法**:
- `on_user_message(content: &str) -> TopicTransition`
- `on_compact(result: &CompactResult)`
- `current_topic() -> Option<Topic>`
- `active_topics() -> Vec<Topic>`

**验收标准**:
- 用户说"好，换个话题"时触发 NewTopic
- compact 归档时触发 Archive
- 话题状态正确追踪

---

### T08: daily.rs 话题时间线格式

**文件**: `nova-core/src/memory/daily.rs`

**新增类型**:
```rust
pub struct DiaryEntry {
    pub time: String,
    pub topic: String,
    pub status: TopicStatus,
    pub description: Option<String>,
    pub conclusions: Vec<String>,
}
```

**新增方法**:
- `append_topic_entry(entry: &DiaryEntry)`
- `append_compact_result(result: &CompactResult)`

**格式示例**:
```markdown
# 2026-04-18

## 14:00 — 婴幼儿辅食与冲泡 [🔵 进行中]

## 13:00 — 鱼缸自动换水系统排查 [⚫ 已归档]
- 已归档：鱼缸自动换水系统排查
- 活跃摘要：用户确认每天2次、每次10L的配置
```

**验收标准**:
- 话题有状态标识（🟢🔵🟡⚫）
- compact 结果正确写入日记
- 时间戳准确

---

### T09: MEMORY.md 白板化

**文件**: `nova-core/src/memory/memory_board.rs`（新建）

**类型**:
```rust
pub struct TopicSummary {
    pub name: String,
    pub status: TopicStatus,
    pub summary: String,
    pub updated_at: String,
}

pub struct MemoryBoard {
    path: PathBuf,
    active_topics: RwLock<Vec<TopicSummary>>,
    permanent_preferences: RwLock<Vec<String>>,
}
```

**方法**:
- `load() -> Result<()>`
- `update_from_compact(result: &CompactResult) -> Result<()>`
- `save() -> Result<()>`
- `render_to_markdown() -> String`

**验收标准**:
- 只保留 Active/Suspended 话题
- 偏好永久保留
- 文件 <200 行

---

## Phase 2：认知灵魂层

### T10: 张力值追踪器

**文件**: `nova-core/src/memory/tension_tracker.rs`（新建）

**类型**:
```rust
pub struct UserState {
    pub intimacy: u8,
    pub trust: u8,
    pub dependency: u8,
    pub last_gap: InteractionGap,
    pub history_tags: Vec<String>,
}

pub struct SessionState {
    pub emotion: Emotion,
    pub context: Context,
    pub user_intent: UserIntent,
    pub tension: u8,
}

pub enum Emotion { Calm, Excited, Anxious, Frustrated, Tense }
pub enum Context { Morning, Afternoon, Evening, Night, Working }
pub enum UserIntent { Task, Casual, Seeking, Emotional }
```

**验收标准**:
- 张力值计算公式正确
- 情绪检测基于关键词
- 场景基于时间判断

---

### T11: 模式路由

**文件**: `nova-core/src/memory/mode_router.rs`（新建）

**模式**:
```rust
pub enum Mode {
    Normal,
    SoftIntimate,
    HighIntimate,
    Cooling,
}
```

**验收标准**:
- tension < 50 → Normal
- tension 50-75 → SoftIntimate
- tension > 75 → HighIntimate
- 回撤条件满足 → Cooling

---

### T12: SOUL.md 状态机 Prompt

**文件**: `~/.nova/SOUL.md`（用户目录下）

**新增内容**:
- 话题感知说明
- 状态响应表
- 主动机制
- 冷淡机制

**验收标准**:
- Nova 表现出状态感知
- 不同模式下语气有差异

---

### T13: <nova_os> 思考管道

**文件**: `nova-core/src/agent/prompt.rs`

**新增内容**:
- `<nova_os>` XML 标签
- 三维状态推演模板
- 响应策略决策

**验收标准**:
- system prompt 包含 `<nova_os>` 块
- 前端/TUI 可折叠该块

---

## Phase 3：集成测试

### T14: 集成测试

**测试场景**:

| # | 场景 | 预期结果 |
|:---|:---|:---|
| 1 | bash 输出 50k 字符 | 截断为 ~20k，头尾保留 |
| 2 | browser snapshot 超长 | 截断为 ~15k |
| 3 | Token 水位 92% 触发 compact | Graceful 模式，生成 JSON，写入日记 |
| 4 | Token 水位 97% 触发 compact | Forceful 模式，直接丢弃旧消息 |
| 5 | 用户说"好，换个话题" | 旧话题归档，新话题开始 |
| 6 | compact 提取偏好 | 偏好写入 MEMORY.md |
| 7 | 张力值 > 75 | 模式切换到 HighIntimate |

**验收标准**: 全部测试通过

---

## 任务依赖关系

```
T01 ─┬─► T02 ─┬─► T03
     │         └─► T04
     │
     └─► T05 ─┬─► T06 ─┬─► T07 ─┬─► T08
              │         │         └─► T09
              │         │
              │         └─────────────────────► T10 ─► T11 ─► T12 ─► T13
              │
              └────────────────────────────────────────────► T14

T01-T06: Phase 1（物理防御层）
T07-T09: Phase 1.5（记忆重塑层）
T10-T13: Phase 2（认知灵魂层）
T14-T18: Phase 3（补bug + 集成测试）

---

## Phase 4：Bug 修复与补充

### T15: Async Rust 锁安全改造

**优先级**: P0

**问题**: 在 `topic_state.rs` 和 `memory_board.rs` 中使用了 `std::sync::RwLock`，在 async 上下文中持有锁跨越 `.await` 会导致编译错误（Send 约束）或死锁。

**改造点**:

```rust
// 错误用法 ❌
pub struct TopicTracker {
    current_topic: RwLock<Option<Topic>>,  // std::sync::RwLock
}

// 正确用法 ✅
use tokio::sync::RwLock;

pub struct TopicTracker {
    current_topic: RwLock<Option<Topic>>,  // tokio::sync::RwLock
}
```

**需要检查的文件**:
- `topic_state.rs` — `current_topic`, `history`
- `memory_board.rs` — `active_topics`, `permanent_preferences`
- `tension_tracker.rs` — `current_session_state`
- `mode_router.rs` — `current_mode`

**验收标准**:
- `cargo check` 编译通过，无 Send 约束错误
- 锁不会跨 `.await` 点持有

---

### T16: TUI 客户端 <nova_os> 拦截器

**优先级**: P1

**问题**: `prompt.rs` 输出的 `<nova_os>` 标签如果直接显示在 TUI，会暴露内部推演过程。

**改造点**:

```rust
// nova-tui/src/ui.rs

/// 过滤 <nova_os> 标签，只显示最终回复
fn filter_nova_os(content: &str) -> String {
    // 方案1：正则移除
    let re = regex::Regex::new(r"<nova_os>[\s\S]*?</nova_os>").unwrap();
    re.replace_all(content, "").to_string()

    // 方案2：只保留最后一个 </nova_os> 后的内容
    // ...
}
```

**或在 Discord 客户端**:

```rust
// nova-discord/src/main.rs

fn filter_nova_os(content: &str) -> String {
    // 移除 <nova_os>...</nova_os> 块
    let re = regex::Regex::new(r"<nova_os>[\s\S]*?</nova_os>").unwrap();
    re.replace_all(content, "").trim().to_string()
}
```

**验收标准**:
- TUI 显示时不暴露 `<nova_os>` 内容
- Discord 回复时不暴露 `<nova_os>` 内容
- 用户只看到最终回复

---

### T17: JSON 解析 Markdown 清洗函数

**优先级**: P0

**问题**: LLM 经常用 markdown 代码块包裹 JSON（`json`），直接解析会失败。

**改造点**:

```rust
// nova-core/src/token/compact.rs

/// 清洗 LLM 输出的 JSON，移除 markdown 代码块
fn clean_json(raw: &str) -> String {
    let trimmed = raw.trim();

    // 移除首尾的 ```json ... ``` 或 ``` ... ```
    let re = regex::Regex::new(r"^```json\s*\n([\s\S]*?)\n```$").unwrap();
    if let Some(caps) = re.captures(trimmed) {
        return caps.get(1).unwrap().as_str().trim().to_string();
    }

    let re2 = regex::Regex::new(r"^```\s*\n([\s\S]*?)\n```$").unwrap();
    if let Some(caps) = re2.captures(trimmed) {
        return caps.get(1).unwrap().as_str().trim().to_string();
    }

    // 移除行首的 markdown 列表符号（如 - 或 *）
    let re3 = regex::Regex::new(r"^[\-\*]\s+").unwrap();
    let lines: Vec<&str> = trimmed.lines().collect();
    let without_bullets: String = lines
        .iter()
        .map(|l| re3.replace_all(l, ""))
        .collect::<Vec<_>>()
        .join("\n");

    without_bullets.trim().to_string()
}
```

**验收标准**:
- ` ```json\n{"foo": "bar"}\n``` ` → `{"foo": "bar"}`
- `json\n{"foo": "bar"}\n` → `{"foo": "bar"}`
- 清洗后 `serde_json::from_str` 成功率 > 95%

---

### T18: Token 精确计数（tiktoken 集成）

**优先级**: P0

**问题**: 如果 `TokenBudget` 使用字符估算（除以 4），在 95% 临界点误差可达数千 tokens，导致防御线被击穿。

**改造点**:

```rust
// nova-core/src/token/counter.rs（新建）

use tiktoken_rs::CoreBPE;

pub struct TokenCounter {
    bpe: CoreBPE,
}

impl TokenCounter {
    pub fn new() -> Result<Self> {
        let bpe = tiktoken_rs::cl100k_base()?;
        Ok(Self { bpe })
    }

    /// 精确计算 token 数量
    pub fn count(&self, text: &str) -> usize {
        self.bpe.encode_ordinary(text).len()
    }

    /// 计算 messages 的总 token 数
    pub fn count_messages(&self, messages: &[Message]) -> usize {
        messages
            .iter()
            .map(|m| {
                let content = m.content.as_deref().unwrap_or("");
                // role + content + overhead
                4 + self.count(m.role.as_str()) + self.count(content)
            })
            .sum()
    }
}
```

**Cargo.toml 依赖**:

```toml
tiktoken-rs = "0.5"
```

**验收标准**:
- token 计数误差 < 1%（对比 API 返回的 usage）
- 95% 临界点触发正确
- 不影响现有 `TokenBudget` 接口

---

## 任务依赖关系（更新后）

```
T01 ─┬─► T02 ─┬─► T03
     │         └─► T04
     │
     └─► T05 ─┬─► T06 ─┬─► T07 ─┬─► T08
              │         │         │         └─► T09
              │         │         │
              │         │         └─────────────────────► T10 ─► T11 ─► T12 ─► T13
              │         │                                                           │
              │         └─────────────────────────────────────────────────────────────┼─► T15
              │                                                                   │
              └───────────────────────────────────────────────────────────────────────┼─► T16
              │                                                                   │
              └───────────────────────────────────────────────────────────────────────┼─► T17
              │                                                                   │
              └───────────────────────────────────────────────────────────────────────┘
                                                                                    │
                                                                                    └─► T14
```

**P0 任务**: T01, T02, T03, T04, T05, T15, T17, T18
**P1 任务**: T06, T07, T08, T09, T16
**P2 任务**: T10, T11, T12, T13
```
