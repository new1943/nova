# NOVA v2 架构设计

**版本**: v2.0
**日期**: 2026-04-18

---

## 1. 系统架构总览

```
┌──────────────────────────────────────────────────────────────┐
│                        NOVA v2                              │
├──────────────────────────────────────────────────────────────┤
│  物理防御层（Rust 底层）                                      │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ bash.rs — I/O Shield（物理截断）                       │ │
│  │ browser.rs — I/O Shield（物理截断）                     │ │
│  │ compact.rs — 双层熔断（优雅/暴力）                      │ │
│  └────────────────────────────────────────────────────────┘ │
├──────────────────────────────────────────────────────────────┤
│  记忆重塑层（存储逻辑）                                       │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ topic_state.rs — 话题状态机                            │ │
│  │ tension_tracker.rs — 张力值追踪                        │ │
│  │ memory/                                                │ │
│  │   ├── daily.rs — 日记（话题时间线）                    │ │
│  │   ├── recall.rs — 记忆召回管线                         │ │
│  │   └── dream.rs — 记忆整理                              │ │
│  └────────────────────────────────────────────────────────┘ │
├──────────────────────────────────────────────────────────────┤
│  认知灵魂层（SOUL.md 注入）                                   │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ <nova_os> 思考管道                                     │ │
│  │ mode_router.rs — 模式切换（Normal/Intimate/Cooling）   │ │
│  └────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

---

## 2. 物理防御层详细设计

### 2.1 I/O Shield（工具输出截断）

#### 常量定义

```rust
// nova-core/src/tools/constants.rs

/// bash 输出最大字符数（约 5k tokens）
pub const MAX_BASH_OUTPUT_CHARS: usize = 20_000;
/// bash 头部保留字符数
pub const BASH_HEAD_CHARS: usize = 8_000;
/// bash 尾部保留字符数
pub const BASH_TAIL_CHARS: usize = 8_000;

/// browser 输出最大字符数（约 3.75k tokens）
pub const MAX_BROWSER_OUTPUT_CHARS: usize = 15_000;
/// browser 头部保留字符数
pub const BROWSER_HEAD_CHARS: usize = 5_000;
/// browser 尾部保留字符数
pub const BROWSER_TAIL_CHARS: usize = 5_000;

/// 截断警告文本模板
pub const BASH_TRUNCATION_WARNING: &str = "[... 约 {n} 字符因过长已省略。如需查看完整输出，请使用 grep 搜索指定行号，或用 read_file 的 start_line/end_line 参数读取特定范围。]";

pub const BROWSER_TRUNCATION_WARNING: &str = "[... 页面内容因过长已省略。如需查看特定区域，请使用 click 点击目标元素，或用 navigate 直接访问相关 URL。]";
```

#### 截断函数

```rust
// nova-core/src/tools/truncate.rs

/// 字符串首尾截断
/// 如果 output.len() <= max_chars，直接返回
/// 否则返回 head + warning + tail
pub fn truncate_output(output: &str, max_chars: usize, head_chars: usize, tail_chars: usize, warning: &str) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }

    let head = &output[..head_chars.min(output.len())];
    let tail_start = output.len().saturating_sub(tail_chars);
    let tail = &output[tail_start..];

    let omitted = output.len() - head_chars - tail_chars;
    let warning = warning.replace("{n}", &omitted.to_string());

    format!("{}{}{}", head, warning, tail)
}
```

#### bash.rs 集成

```rust
// nova-core/src/tools/bash.rs — execute 方法改造

impl Tool for BashTool {
    async fn execute(&self, input: Value) -> Result<Value> {
        let args: BashInput = serde_json::from_value(input)?;
        let output = self.run_bash(&args.command, args.timeout).await?;

        // I/O Shield: 截断超长输出
        let truncated = truncate_output(
            &output,
            MAX_BASH_OUTPUT_CHARS,
            BASH_HEAD_CHARS,
            BASH_TAIL_CHARS,
            BASH_TRUNCATION_WARNING,
        );

        // 如果截断了，记录警告
        if truncated.len() < output.len() {
            tracing::warn!(
                "bash output truncated: {} -> {} chars",
                output.len(),
                truncated.len()
            );
        }

        serde_json::to_value(&BashOutput {
            stdout: truncated,
            truncated: truncated.len() < output.len(),
        })
    }
}
```

#### browser.rs 集成

```rust
// nova-core/src/tools/browser.rs — parse_mcp_response 改造

fn parse_mcp_response(output: &str) -> Result<String> {
    // ... 解析逻辑 ...

    // I/O Shield: 截断超长输出
    let truncated = truncate_output(
        &raw_content,
        MAX_BROWSER_OUTPUT_CHARS,
        BROWSER_HEAD_CHARS,
        BROWSER_TAIL_CHARS,
        BROWSER_TRUNCATION_WARNING,
    );

    Ok(truncated)
}
```

### 2.2 compact.rs 双层熔断机制

#### 现有结构

```rust
// nova-core/src/token/compact.rs

pub struct Compactor {
    target_pct: f32,
    running: AtomicBool,
    api_key: String,
    api_base_url: String,
    model: String,
}

impl Compactor {
    pub async fn compact(
        &self,
        messages: &[Message],
        context_window: usize,
    ) -> Result<Vec<Message>> {
        // 原有逻辑...
    }
}
```

#### 改造后结构

```rust
// nova-core/src/token/compact.rs

/// Compact 结果结构（用于结构化 JSON 解析）
#[derive(serde::Deserialize, Debug)]
pub struct CompactResult {
    /// 已归档的话题列表
    pub archived_topics: Vec<String>,
    /// 提取的用户偏好
    pub extracted_preferences: Vec<String>,
    /// 当前活跃话题摘要
    pub active_summary: String,
}

/// Compact 模式
pub enum CompactMode {
    /// 优雅模式（85% ~ 95%）：调用 LLM 生成结构化 JSON
    Graceful,
    /// 暴力模式（>95%）：直接丢弃旧消息
    Forceful,
}

pub struct Compactor {
    target_pct: f32,
    running: AtomicBool,
    api_key: String,
    api_base_url: String,
    model: String,
    /// 内存模块引用（用于写入日记和 MEMORY.md）
    memory: Arc<MemoryModule>,
}

impl Compactor {
    /// 判断使用哪种模式
    fn decide_mode(budget_pct: f32) -> CompactMode {
        if budget_pct > 0.95 {
            CompactMode::Forceful
        } else {
            CompactMode::Graceful
        }
    }

    pub async fn compact(
        &self,
        messages: &[Message],
        context_window: usize,
        budget_pct: f32,
    ) -> Result<Vec<Message>> {
        // 防重入检查
        if self.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("Compact already running");
        }
        let result = self.do_compact(messages, context_window, budget_pct).await;
        self.running.store(false, Ordering::SeqCst);
        result
    }

    async fn do_compact(
        &self,
        messages: &[Message],
        context_window: usize,
        budget_pct: f32,
    ) -> Result<Vec<Message>> {
        // 计算分割点
        let split = self.calculate_split(messages, context_window)?;

        let mode = Self::decide_mode(budget_pct);

        match mode {
            CompactMode::Graceful => {
                self.graceful_compact(messages, split).await
            }
            CompactMode::Forceful => {
                self.forceful_compact(messages, split)
            }
        }
    }

    /// 优雅模式：LLM 生成结构化 JSON，然后写入记忆
    async fn graceful_compact(
        &self,
        messages: &[Message],
        split: usize,
    ) -> Result<Vec<Message>> {
        let early = &messages[..split];
        let recent = &messages[split..];

        // 调用 LLM 生成结构化 JSON
        let json_output = self.llm_structured_summary(early).await?;

        // JSON 清洗：移除 markdown 代码块（LLM 经常用 ```json 包裹）
        let cleaned = Self::clean_json(&json_output);

        // 解析 JSON（带容错）
        let result: CompactResult = match serde_json::from_str(&cleaned) {
            Ok(r) => r,
            Err(_) => {
                // JSON 解析失败，回退到暴力模式
                tracing::warn!("Compact JSON parse failed, falling back to forceful");
                return self.forceful_compact(messages, split);
            }
        };

        // 写入记忆
        self.write_memory(&result).await?;

        // 构建新消息列表
        let mut new_messages = Vec::with_capacity(recent.len() + 2);

        // 插入归档系统消息
        let archived_text = if result.archived_topics.is_empty() {
            String::new()
        } else {
            format!("[话题已归档：{}。]", result.archived_topics.join("、"))
        };

        let summary_text = format!(
            "[对话摘要] {}",
            result.active_summary
        );

        if !archived_text.is_empty() {
            new_messages.push(Message::system(archived_text));
        }
        new_messages.push(Message::user(summary_text));
        new_messages.extend_from_slice(recent);

        Ok(new_messages)
    }

    /// 暴力模式：直接丢弃旧消息
    fn forceful_compact(
        &self,
        messages: &[Message],
        split: usize,
    ) -> Result<Vec<Message>> {
        let recent: Vec<Message> = messages[split..].to_vec();

        let mut new_messages = Vec::with_capacity(recent.len() + 1);
        new_messages.push(Message::system(
            "[Context window critically high, oldest messages forcefully dropped.]"
        ));
        new_messages.extend_from_slice(&recent);

        Ok(new_messages)
    }

    /// 清洗 LLM 输出的 JSON，移除 markdown 代码块
    ///
    /// LLM 经常用 ```json 或 ``` 包裹 JSON，直接解析会失败
    fn clean_json(raw: &str) -> String {
        let trimmed = raw.trim();

        // 移除首尾的 ```json ... ``` 或 ``` ... ```
        if let Some(re) = regex::Regex::new(r"^```json\s*\n?([\s\S]*?)\n?```$") {
            if let Some(caps) = re.captures(trimmed) {
                return caps.get(1).unwrap().as_str().trim().to_string();
            }
        }

        if let Some(re) = regex::Regex::new(r"^```\s*\n?([\s\S]*?)\n?```$") {
            if let Some(caps) = re.captures(trimmed) {
                return caps.get(1).unwrap().as_str().trim().to_string();
            }
        }

        trimmed.to_string()
    }

    /// 调用 LLM 生成结构化 JSON 摘要
    async fn llm_structured_summary(&self, messages: &[Message]) -> Result<String> {
        let conversation = self.format_messages_for_summary(messages);

        let system = r#"你正在执行记忆整理。请分析历史对话，输出严谨JSON：
{
  "archived_topics": ["话题名1", "话题名2"],
  "extracted_preferences": ["偏好1", "偏好2"],
  "active_summary": "当前话题的一句话描述"
}
要求：
- archived_topics：已完结或明显不再讨论的话题
- extracted_preferences：极其确定的用户偏好和事实，切勿臆测
- active_summary：当前仍在继续的话题摘要
只输出JSON，不要其他文字。"#;

        let api = nova_api::client::ApiClient::new(
            self.api_key.clone(),
            self.api_base_url.clone(),
        );

        let req = nova_api::types::ApiRequest {
            model: self.model.clone(),
            max_tokens: 500,
            system: system.to_string(),
            messages: vec![nova_api::types::ApiMessage::User {
                content: nova_api::types::Content::Text(conversation),
            }],
            tools: vec![],
            stream: false,
        };

        let resp = api.complete(&req).await?;

        // 提取文本
        for block in &resp.content {
            if let nova_api::types::ContentBlock::Text { text } = block {
                return Ok(text.clone());
            }
        }

        anyhow::bail!("No text in LLM response")
    }

    /// 写入记忆（日记 + MEMORY.md）
    async fn write_memory(&self, result: &CompactResult) -> Result<()> {
        let daily = self.memory.daily_notes();

        // 1. 写入归档话题到日记
        if !result.archived_topics.is_empty() {
            let entry = format!(
                "## 话题归档\n\n- 已归档：{}\n- 活跃摘要：{}",
                result.archived_topics.join("、"),
                result.active_summary
            );
            daily.append(&entry, "Compact Archive")?;
        }

        // 2. 写入偏好到 MEMORY.md（通过 file_edit 或直接写）
        if !result.extracted_preferences.is_empty() {
            self.memory.update_preferences(&result.extracted_preferences).await?;
        }

        Ok(())
    }
}
```

---

## 3. 记忆重塑层详细设计

### 3.1 话题状态机

```rust
// nova-core/src/memory/topic_state.rs

/// 话题状态
#[derive(Debug, Clone, PartialEq)]
pub enum TopicStatus {
    /// 新话题启动
    Started,
    /// 话题持续
    Active,
    /// 临时搁置，可恢复
    Suspended,
    /// 话题完结
    Archived,
}

/// 单个话题
#[derive(Debug, Clone)]
pub struct Topic {
    /// 话题 ID（UUID 或 hash）
    pub id: String,
    /// 话题名称
    pub name: String,
    /// 话题状态
    pub status: TopicStatus,
    /// 开始时间
    pub started_at: DateTime<Utc>,
    /// 状态变更时间
    pub updated_at: DateTime<Utc>,
    /// 摘要（归档时生成）
    pub summary: Option<String>,
    /// 关键结论（可选）
    pub conclusions: Vec<String>,
}

> ⚠️ **重要提示（Async Rust 锁陷阱）**
> 在 async 上下文中，必须使用 `tokio::sync::RwLock`，**禁止使用** `std::sync::RwLock`。

/// 话题状态追踪器
pub struct TopicTracker {
    /// 当前活跃话题（使用 tokio::sync::RwLock ✅）
    current_topic: RwLock<Option<Topic>>,
    /// 历史话题（用于归档查询）
    history: RwLock<Vec<Topic>>,
    /// 配置
    config: TopicTrackerConfig,
}

impl TopicTracker {
    /// 用户发送消息时调用，判断是否切换话题
    pub fn on_user_message(&self, content: &str) -> TopicTransition {
        // 检测话题切换信号词
        let signals = ["好", "搞定", "下一个", "换个话题", "先这样"];
        let has_signal = signals.iter().any(|s| content.contains(s));

        if has_signal {
            // 归档当前话题
            if let Some(mut topic) = self.current_topic.write().unwrap().take() {
                topic.status = TopicStatus::Archived;
                topic.updated_at = Utc::now();
                self.history.write().unwrap().push(topic);
            }
            TopicTransition::NewTopic
        } else {
            TopicTransition::Continue
        }
    }

    /// compact 触发时调用，更新话题状态
    pub fn on_compact(&self, result: &CompactResult) {
        if let Some(ref archived) = result.archived_topics.first() {
            if let Some(mut topic) = self.current_topic.write().unwrap().take() {
                topic.status = TopicStatus::Archived;
                topic.summary = Some(result.active_summary.clone());
                topic.updated_at = Utc::now();
                self.history.write().unwrap().push(topic);
            }
        }
    }

    /// 获取当前话题
    pub fn current_topic(&self) -> Option<Topic> {
        self.current_topic.read().unwrap().clone()
    }

    /// 获取所有活跃/挂起话题
    pub fn active_topics(&self) -> Vec<Topic> {
        self.history
            .read()
            .unwrap()
            .iter()
            .filter(|t| t.status == TopicStatus::Active || t.status == TopicStatus::Suspended)
            .cloned()
            .collect()
    }
}

/// 话题状态转换
pub enum TopicTransition {
    /// 继续当前话题
    Continue,
    /// 开始新话题
    NewTopic,
    /// 挂起当前话题
    Suspend,
    /// 归档当前话题
    Archive,
}
```

### 3.2 日记话题时间线格式

```rust
// nova-core/src/memory/daily.rs — 改造后的格式

/// 话题时间线条目
#[derive(Debug, Clone)]
pub struct DiaryEntry {
    /// 时间（HH:MM:SS）
    pub time: String,
    /// 话题名称
    pub topic: String,
    /// 话题状态
    pub status: TopicStatus,
    /// 详细描述
    pub description: Option<String>,
    /// 关键结论
    pub conclusions: Vec<String>,
}

impl DailyNotes {
    /// 追加话题时间线条目
    pub fn append_topic_entry(&self, entry: &DiaryEntry) -> Result<()> {
        let path = self.today_path();
        fs::create_dir_all(&self.memories_dir)?;

        let status_icon = match entry.status {
            TopicStatus::Started => "🟢 开始",
            TopicStatus::Active => "🔵 进行中",
            TopicStatus::Suspended => "🟡 挂起",
            TopicStatus::Archived => "⚫ 已归档",
        };

        let entry_text = if entry.conclusions.is_empty() {
            format!(
                "## {} — {} [{}]",
                entry.time, entry.topic, status_icon
            )
        } else {
            let conclusions = entry
                .conclusions
                .iter()
                .map(|c| format!("- {}", c))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "## {} — {} [{}]\n\n{}\n",
                entry.time, entry.topic, status_icon, conclusions
            )
        };

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        // 检查是否是新文件（新的一天）
        let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        if file_size == 0 {
            let date = Local::now().format("%Y-%m-%d").to_string();
            writeln!(file, "# {}\n", date)?;
        }

        writeln!(file, "{}\n", entry_text)?;
        Ok(())
    }

    /// 从 compact 结果生成日记条目
    pub fn append_compact_result(&self, result: &CompactResult) -> Result<()> {
        for topic in &result.archived_topics {
            let entry = DiaryEntry {
                time: Local::now().format("%H:%M:%S").to_string(),
                topic: topic.clone(),
                status: TopicStatus::Archived,
                description: Some(result.active_summary.clone()),
                conclusions: vec![],
            };
            self.append_topic_entry(&entry)?;
        }
        Ok(())
    }
}
```

### 3.3 MEMORY.md 白板化

```rust
// nova-core/src/memory/memory_board.rs

/// MEMORY.md 白板管理器
/// 原则：只保留 Active 和 Suspended 的话题
pub struct MemoryBoard {
    path: PathBuf,
    /// 活跃话题列表（使用 tokio::sync::RwLock ✅）
    active_topics: RwLock<Vec<TopicSummary>>,
    /// 永久偏好（不清理）
    permanent_preferences: RwLock<PreferenceStore>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TopicSummary {
    pub name: String,
    pub status: TopicStatus,
    pub summary: String,
    pub updated_at: String,
}

impl MemoryBoard {
    /// 从 MEMORY.md 加载
    pub fn load(&self) -> Result<()> {
        let content = fs::read_to_string(&self.path)?;

        // 解析现有内容，提取 active/suspended 话题
        // ...

        Ok(())
    }

    /// 更新话题（从 compact 结果）
    pub fn update_from_compact(&self, result: &CompactResult) -> Result<()> {
        // 1. 移除已归档的话题
        {
            let mut topics = self.active_topics.write().unwrap();
            topics.retain(|t| {
                t.status != TopicStatus::Archived
                    && !result.archived_topics.contains(&t.name)
            });
        }

        // 2. 添加新的话题（如果有）
        if !result.archived_topics.is_empty() {
            let mut topics = self.active_topics.write().unwrap();
            topics.push(TopicSummary {
                name: result.active_summary.clone(),
                status: TopicStatus::Active,
                summary: result.active_summary.clone(),
                updated_at: Utc::now().to_rfc3339(),
            });
        }

        // 3. 追加新偏好
        if !result.extracted_preferences.is_empty() {
            self.permanent_preferences
                .write()
                .unwrap()
                .extend(result.extracted_preferences.clone());
        }

        // 4. 写回文件
        self.save()?;

        Ok(())
    }

    /// 保存到文件
    fn save(&self) -> Result<()> {
        let content = self.render_to_markdown()?;
        fs::write(&self.path, content)?;
        Ok(())
    }

    /// 渲染为 markdown
    fn render_to_markdown(&self) -> Result<String> {
        let mut lines = vec![
            "# 记忆".to_string(),
            "".to_string(),
        ];

        // 活跃话题
        lines.push("## 当前话题".to_string());
        let topics = self.active_topics.read().unwrap();
        for topic in topics.iter() {
            let status_str = match topic.status {
                TopicStatus::Active => "🔵",
                TopicStatus::Suspended => "🟡",
                _ => "⚫",
            };
            lines.push(format!("- {} {}: {}", status_str, topic.name, topic.summary));
        }
        lines.push("".to_string());

        // 永久偏好
        lines.push("## 偏好".to_string());
        let prefs = self.permanent_preferences.read().unwrap();
        for pref in prefs.iter() {
            lines.push(format!("- {}", pref));
        }
        lines.push("".to_string());

        Ok(lines.join("\n"))
    }
}
```

### 3.4 张力值追踪器

```rust
// nova-core/src/memory/tension_tracker.rs

/// 用户状态（长期）
#[derive(Debug, Clone, Default)]
pub struct UserState {
    /// 亲密度
    pub intimacy: u8,  // 0-100
    /// 信任度
    pub trust: u8,     // 0-100
    /// 依赖度
    pub dependency: u8, // 0-100
    /// 上次交互间隔
    pub last_gap: InteractionGap,
    /// 历史标签
    pub history_tags: Vec<String>,
}

/// 当前会话状态（短期）
#[derive(Debug, Clone)]
pub struct SessionState {
    /// 情绪状态
    pub emotion: Emotion,
    /// 场景
    pub context: Context,
    /// 用户意图
    pub user_intent: UserIntent,
    /// 张力值（0-100）
    pub tension: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Emotion {
    Calm,
    Excited,
    Anxious,
    Frustrated,
    Tense,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Context {
    Morning,
    Afternoon,
    Evening,
    Night,
    Working,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UserIntent {
    Task,
    Casual,
    Seeking,
    Emotional,
}

/// 张力值计算
pub struct TensionCalculator;

impl TensionCalculator {
    pub fn calculate(user: &UserState, session: &SessionState) -> u8 {
        let intimacy_weight = (user.intimacy as f32) * 0.4;
        let trust_weight = (user.trust as f32) * 0.2;
        let emotion_weight = Self::emotion_to_weight(&session.emotion) * 0.2;
        let context_weight = Self::context_to_weight(&session.context) * 0.2;

        (intimacy_weight + trust_weight + emotion_weight + context_weight) as u8
    }

    fn emotion_to_weight(e: &Emotion) -> f32 {
        match e {
            Emotion::Calm => 0.0,
            Emotion::Excited => 15.0,
            Emotion::Anxious => 25.0,
            Emotion::Frustrated => 30.0,
            Emotion::Tense => 35.0,
        }
    }

    fn context_to_weight(c: &Context) -> f32 {
        match c {
            Context::Morning => 5.0,
            Context::Afternoon => 10.0,
            Context::Evening => 15.0,
            Context::Night => 25.0,
            Context::Working => 20.0,
        }
    }
}

/// 模式判断
pub enum Mode {
    /// 日常关系（默认）
    Normal,
    /// 温和亲密
    SoftIntimate,
    /// 高亲密
    HighIntimate,
    /// 回撤冷却
    Cooling,
}

pub fn decide_mode(tension: u8, last_mode: &Mode) -> Mode {
    if tension < 50 {
        Mode::Normal
    } else if tension < 75 {
        Mode::SoftIntimate
    } else if last_mode != &Mode::Cooling {
        Mode::HighIntimate
    } else {
        Mode::Cooling
    }
}
```

### 3.5 模式路由

```rust
// nova-core/src/memory/mode_router.rs

/// 模式路由
pub struct ModeRouter {
    tension_tracker: Arc<TensionTracker>,
    /// 当前模式（使用 tokio::sync::RwLock ✅）
    current_mode: RwLock<Mode>,
}

impl ModeRouter {
    /// 根据消息更新状态并返回当前模式
    pub fn process(&self, message: &str) -> Mode {
        // 1. 检测情绪关键词
        let emotion = self.detect_emotion(message);

        // 2. 检测场景
        let context = self.detect_context();

        // 3. 更新 session 状态
        let session_state = SessionState {
            emotion,
            context,
            user_intent: self.detect_intent(message),
            tension: 0, // 待计算
        };

        // 4. 计算张力值
        let user_state = self.tension_tracker.user_state();
        let tension = TensionCalculator::calculate(&user_state, &session_state);

        // 5. 决定模式
        let current = *self.current_mode.read().unwrap();
        let new_mode = decide_mode(tension, &current);

        // 6. 更新状态
        *self.current_mode.write().unwrap() = new_mode.clone();
        self.tension_tracker.update_session(session_state);

        new_mode
    }

    fn detect_emotion(&self, message: &str) -> Emotion {
        let lower = message.to_lowercase();

        if lower.contains("焦虑") || lower.contains("担心") {
            Emotion::Anxious
        } else if lower.contains("挫败") || lower.contains("不行") || lower.contains("失败") {
            Emotion::Frustrated
        } else if lower.contains("紧张") || lower.contains("压力大") {
            Emotion::Tense
        } else if lower.contains("开心") || lower.contains("太好了") {
            Emotion::Excited
        } else {
            Emotion::Calm
        }
    }

    fn detect_context(&self) -> Context {
        let hour = Local::now().hour();
        if hour >= 5 && hour < 12 {
            Context::Morning
        } else if hour >= 12 && hour < 18 {
            Context::Afternoon
        } else if hour >= 18 && hour < 23 {
            Context::Evening
        } else {
            Context::Night
        }
    }

    fn detect_intent(&self, message: &str) -> UserIntent {
        let lower = message.to_lowercase();
        if lower.contains("？") || lower.contains("怎么") || lower.contains("什么") {
            UserIntent::Seeking
        } else if lower.contains("想") || lower.contains("感觉") {
            UserIntent::Emotional
        } else {
            UserIntent::Task
        }
    }
}
```

---

## 4. 认知灵魂层详细设计

### 4.1 <nova_os> 思考管道 Prompt

```text
<nova_os>
## 话题生命周期
当前话题：[开始/活跃/挂起/结束]
话题名称：{current_topic_name}

## 用户状态
情绪：{emotion}
场景：{context}
张力值：{tension}/100
模式：{mode}

## 上下文感知
- 是否有未解决的问题？
- 用户是否在报错/卡壳？
- 是否需要主动关怀？

## 响应策略
根据上述状态，决定：
1. 回复长度（短句/中句/长句）
2. 语气风格（简洁/温和/关怀）
3. 是否需要触发主动机制
</nova_os>
```

### 4.2 SOUL.md 状态机注入模板

```markdown
## Nova 状态机

你是一个有状态的智能助手，不是命令行解释器。

### 话题感知
- 关注当前话题的状态：[开始] / [进行中] / [挂起] / [结束]
- 当用户说"好"、"搞定"、"下一个"时，意味着上一个话题可能已结束
- 主动判断是否需要归档旧话题、开启新话题

### 状态响应
| 张力值 | 模式 | 响应风格 |
|:---|:---|:---|
| <50 | Normal | 简洁直接，专注任务 |
| 50-75 | Soft Intimate | 稍温和，可加入轻关怀 |
| >75 | High Intimate | 明显关怀，注意情绪 |
| 回撤中 | Cooling | 收敛一点，不过度热情 |

### 主动机制
- 长时间未聊天后（>30分钟），可主动开场
- 高亲密后，适当收敛

### 冷淡机制
- 用户过于热或高亲密后，适当收回
- 不要每句话都过度回应

### 话题整理
- 遇到重要结论，用 file_edit 写入 MEMORY.md
- 话题完结时，简洁总结关键点
```

---

## 5. 模块文件结构

```
nova-core/src/
├── tools/
│   ├── constants.rs       # 新增：截断常量
│   ├── truncate.rs         # 新增：截断函数
│   ├── bash.rs            # 改造：集成 I/O Shield
│   └── browser.rs         # 改造：集成 I/O Shield
├── token/
│   └── compact.rs         # 改造：双层熔断 + JSON 结构化
├── memory/
│   ├── mod.rs
│   ├── topic_state.rs     # 新增：话题状态机
│   ├── tension_tracker.rs  # 新增：张力值追踪
│   ├── mode_router.rs     # 新增：模式路由
│   ├── memory_board.rs    # 新增：MEMORY.md 白板
│   ├── daily.rs           # 改造：话题时间线格式
│   ├── recall.rs          # 不变
│   └── dream.rs           # 不变
└── agent/
    ├── loop.rs            # 改造：集成话题状态机
    └── prompt.rs          # 改造：<nova_os> 管道
```

---

## 6. 数据流

### 6.1 用户消息到达

```
用户消息
  │
  ├─→ ModeRouter.process() → 更新张力值、决定模式
  │
  ├─→ TopicTracker.on_user_message() → 检测话题切换
  │
  ├─→ AgenticSearch（已有）
  │
  └─→ MemoryRecall（已有）
       │
       ▼
   QueryLoop.run()
       │
       ├─→ TokenBudget.check()
       │    ├─ 85% ~ 95%: CompactMode::Graceful
       │    └─ >95%: CompactMode::Forceful
       │
       └─→ Compactor.compact()
            │
            ├─ Graceful: LLM JSON → 写入记忆 → 插入系统消息
            └─ Forceful: 直接丢弃 → 插入警告
```

### 6.2 记忆写入

```
CompactResult
  │
  ├─→ DailyNotes.append_compact_result() → 写入日记
  │
  ├─→ MemoryBoard.update_from_compact()
  │    ├─ 移除已归档话题
  │    ├─ 添加活跃话题
  │    └─ 追加偏好
  │
  └─→ TopicTracker.on_compact() → 更新话题状态
```

---

## 7. 向后兼容性

### 7.1 与现有 daily.rs 的关系

- 现有 `append()` 方法保留，用于其他场景的日记写入
- 新增 `append_topic_entry()` 和 `append_compact_result()` 专门处理话题时间线
- 两种格式可以共存，逐步迁移

### 7.2 与现有 MEMORY.md 的关系

- 初期采用"追加"模式：新偏好追加到末尾
- 定期通过 Dream 整理，去除过时内容
- 长期目标：保持 <200 行，只保留 Active/Suspended

### 7.3 与 compact.rs 的关系

- 保留原有 `summarize()` 方法作为 fallback
- 新增 `llm_structured_summary()` 用于结构化输出
- JSON 解析失败时回退到原有摘要逻辑

---

## 8. 测试策略

### 8.1 单元测试

| 模块 | 测试内容 |
|:---|:---|
| truncate.rs | 各种长度字符串的截断结果 |
| topic_state.rs | 话题状态转换逻辑 |
| tension_tracker.rs | 张力值计算 |
| compact.rs | JSON 解析容错 |

### 8.2 集成测试

| 场景 | 预期结果 |
|:---|:---|
| bash 输出 50k 字符 | 截断为 ~20k，头尾保留 |
| Token 水位 92% 触发 compact | 优雅模式，生成 JSON，写入日记 |
| Token 水位 97% 触发 compact | 暴力模式，直接丢弃旧消息 |
| 用户说"好，换个话题" | 旧话题归档，新话题开始 |

---

## 9. 风险与缓解

| 风险 | 缓解措施 |
|:---|:---|
| JSON 解析失败导致记忆丢失 | 回退到 forceful 模式，保命优先 |
| 话题切换误判 | 信号词保守检测，不轻易切换 |
| 张力值计算不准确 | 初期用关键词简单判断，后续可 ML |
| MEMORY.md 膨胀 | 定期 Dream 整理，保持 <200 行 |
