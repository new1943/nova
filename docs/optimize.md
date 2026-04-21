# 竞品/参考项目：hermes-rs

> 来源：微信公众号「老码小张」- 2026年4月1日
> 原文：https://mp.weixin.qq.com/s/Su6tSY12iOJ3Ejh0WkTR-w
> 项目：https://github.com/coder-brzhang/hermes-rs

---

## 一、项目背景

hermes-agent 是 Nous Research 开源的自进化 AI Agent 框架，原 Python 版约 5 万行代码，`run_agent.py` 单文件近 8500 行。

用 Rust 重写后：~5000 行代码，13 个 crate / 66 个文件，编译产物为单个 25MB 二进制。

| 指标 | Python 版 | Rust 重写版 |
|------|-----------|------------|
| 代码行数 | ~50,000 | ~5,000 |
| 模块数 | 100+ 文件 | 13 crate / 66 文件 |
| 编译产物 | 需要 Python runtime | 单个 25MB 二进制 |
| 类型安全 | 运行时（mypy 可选） | 编译时保证 |
| async 模型 | asyncio + threading 混合 | 纯 tokio async |
| 错误处理 | try/except | Result<T, E> 全链路 |
| 空载内存 | 200MB+ | 大幅降低 |
| 冷启动 | 3-5 秒 | 大幅缩短 |

---

## 二、hermes-agent 核心特性

- **自进化技能系统**：Agent 从完成的任务中自动生成技能，并在后续使用中不断优化
- **18+ 消息平台适配**：Telegram、Discord、Slack、WhatsApp、Signal、微信企业版等
- **MCP 协议原生支持**：可接入任何 MCP 工具服务器
- **多 LLM Provider**：OpenRouter、OpenAI、Anthropic、本地模型，一键切换

---

## 三、架构设计亮点

### 1. 13 个 crate 的 DAG 依赖管理

```
hermes-cli
└─ hermes-agent
   ├─ hermes-llm ──── hermes-config ──── hermes-core
   ├─ hermes-tools ── hermes-terminal ── hermes-security
   ├─ hermes-mcp
   └─ hermes-skills
      └─ hermes-gateway
         └─ hermes-cron
```

各 crate 职责：
- `hermes-core` — 共享类型：Message、ToolCall、Platform、Error
- `hermes-config` — YAML 配置 + .env + SOUL.md 人格加载
- `hermes-security` — 注入扫描、环境变量过滤、路径防护
- `hermes-state` — SQLite + FTS5 全文搜索
- `hermes-llm` — LLM 客户端：OpenAI 兼容 + SSE 流式
- `hermes-terminal` — 终端执行后端：Local + Docker
- `hermes-skills` — 技能系统：SKILL.md 解析、CRUD
- `hermes-tools` — 工具注册表 + 9 个内置工具
- `hermes-mcp` — MCP 协议客户端（stdio + JSON-RPC）
- `hermes-agent` — 核心 Agent 循环：对话编排、上下文压缩
- `hermes-gateway` — 网关 + 5 个平台适配器
- `hermes-cron` — 定时任务调度器
- `hermes-cli` — 交互式终端 UI

**拆分理由**：
- 增量编译极快——改一个工具不需要重编译整个 Agent
- 依赖隔离——`hermes-gateway` 可以不依赖 `hermes-terminal`
- 按需编译——不需要 Telegram？`cargo build --no-default-features`

### 2. 4 个核心 Trait 统领全局

```rust
// 1. LLM 调用抽象
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, LlmError>;
    async fn stream(&self, req: &CompletionRequest, tx: mpsc::Sender<StreamDelta>) -> Result<CompletionResponse, LlmError>;
}

// 2. 工具执行抽象
#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<String, ToolError>;
}

// 3. 执行环境抽象（本地 vs Docker）
#[async_trait]
pub trait TerminalBackend: Send + Sync {
    async fn execute(&self, cmd: &str, cwd: Option<&str>, timeout: Option<Duration>) -> Result<ExecResult, TerminalError>;
    async fn cleanup(&self) -> Result<(), TerminalError>;
}

// 4. 消息平台抽象
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn platform(&self) -> Platform;
    async fn connect(&mut self) -> Result<(), GatewayError>;
    async fn send(&self, chat_id: &str, content: &str, reply_to: Option<&str>) -> Result<SendResult, GatewayError>;
    fn set_message_handler(&mut self, handler: MessageHandler);
}
```

**设计优势**：Python 版用 `if/elif` 分支处理不同实现，Rust 用 trait 绑定——编译器强制你实现所有情况，漏一个分支就编译不过。

### 3. 消灭 async/sync 桥接

Python 版的噩梦：
```python
# 50 行处理：主线程用持久化事件循环，工作线程各起一个，嵌套上下文再起一个
def _run_async():
    if in_main_thread: ...
    elif in_worker: ...
    elif in_gateway: ...
```

Rust 版：整个应用运行在同一个 tokio runtime 上，所有工具 `async fn`，并行工具执行用 `JoinSet`，Gateway 消息传递用 `mpsc` channel，没有线程局部存储，没有事件循环生命周期管理。

### 4. 配置零迁移 + 分层覆盖

```yaml
# 全局配置 ~/.hermes/config.yaml
model:
  default: "anthropic/claude-sonnet-4"
base_url: "https://openrouter.ai/api/v1"
terminal:
  backend: "local"
  timeout: 180
mcp_servers:
  filesystem:
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
```

- 用 `#[serde(default)]` 确保任何配置子集都能正确加载
- 从 Python 切换到 Rust 版本，**不需要改任何配置**
- 支持**项目本地配置**——`.hermes/config.yaml` 自动覆盖全局配置

### 5. 类型安全兜底

```rust
pub struct SessionSource {
    pub platform: Platform,     // 枚举，不是字符串
    pub chat_id: String,
    pub user_id: Option<String>,
}
```

- 编译器强制区分 `chat_id` 和 `user_id`，防止类型混淆
- Rust 的 `Mutex<T>` 在编译时就不允许绕过，加锁不可省略
- `Result<T, E>` + `?` 操作符让错误传播自然且可追踪
- `Option<T>` 强制处理每一个可能为空的值

---

## 四、技术栈选型

| 功能 | 选型 | 理由 |
|------|------|------|
| 异步运行时 | tokio | 生态最大，性能最好 |
| HTTP 客户端 | reqwest | SSE streaming 支持好 |
| 序列化 | serde + serde_json + serde_yaml | Rust 序列化事实标准 |
| 数据库 | rusqlite (bundled) | 零外部依赖的 SQLite |
| 错误处理 | thiserror + anyhow | 库用 thiserror，应用层用 anyhow |
| CLI | clap | 声明式参数解析 |
| 日志 | tracing | 结构化日志，性能好 |
| Docker | bollard | 直接调 Docker Engine API |

---

## 五、对 Nova 的借鉴意义

### 可借鉴的点

1. **Trait 抽象系统**：把工具系统（ToolHandler）、执行环境（TerminalBackend）、消息平台（PlatformAdapter）用 trait 抽象出来
   - Nova 当前工具注册是 `HashMap<String, Arc<dyn Tool>>`
   - 可以进一步抽象为 `trait ToolBackend: Send + Sync`，支持运行时动态注册和编译时静态注册

2. **分层 crate 架构**：Nova 当前 5 个 crate（core/api/daemon/tui/ipc），可以进一步拆分出 `nova-tools`、`nova-memory`、`nova-skills`、`nova-gateway`

3. **配置分层覆盖**：hermes-gateway 支持全局 + 项目本地配置覆盖，Nova 的 workspace 文件注入已经有类似能力，但可以做得更显式

4. **技能系统设计**：hermes-skills 用 SKILL.md 解析 + CRUD，Nova 的 SkillsLoader 可以参考其抽象方式

5. **编译时安全优势**：Rust 的类型系统和所有权模型帮助提前发现数据竞争、错误传播、空值处理等问题

### 当前 Nova 缺少的

- 无 trait 抽象系统，扩展靠注册表 + if let
- 无多平台适配能力（TUI 专用）
- 无 MCP 原生支持
- Skills 系统较简单，无自进化能力
- 无 SQLite + FTS5 的结构化记忆召回（当前靠 JSONL + LLM 摘要）
