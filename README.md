# NOVA

**Rust 重写的 OpenClaw Agent + Claude Code 16 策略 + 赛博朋克 TUI**

## 架构

```
nova/
├── nova-core/      # 核心运行时库（Agent Loop, Tools, Session, Memory, Hooks...）
├── nova-api/       # LLM API 客户端（Anthropic 兼容 + SSE 流式）
├── nova-ipc/       # 进程间通信（Unix Domain Socket, JSON lines）
├── nova-daemon/    # 守护进程（nova run / nova stop）
└── nova-tui/       # 赛博朋克 TUI 客户端（kiko）
```

## 快速开始

```bash
# 配置
mkdir -p ~/.nova
cat > ~/.nova/config << 'EOF'
api_key = "<your-minimax-api-key>"
model = "MiniMax-M2.7"
api_base_url = "https://api.minimaxi.com/anthropic"
EOF

# 编译
cargo build --release

# 启动守护进程
./target/release/nova-daemon run &

# 连接 TUI
./target/release/nova-tui
```

## 已实现的 Claude Code 策略

| # | 策略 | 模块 |
|:--|:---|:---|
| 1 | Query Loop | `agent/loop.rs` |
| 2 | Token Budget 双阈值 | `token/budget.rs` |
| 3 | Compact 对话压缩 | `token/compact.rs` |
| 4 | Forked Agent | `agent/forked.rs` |
| 5 | PostSampling Hooks | `hooks/post_sampling.rs` |
| 6 | StopHooks | `hooks/stop.rs` |
| 7 | 双写互斥记忆 | `memory/dual_write.rs` |
| 8 | 工具池稳定排序 | `tools/registry.rs` |
| 9 | Team 系统 | `team/` |
| 10 | Subagent spawn | `subagent/` |
| 11 | SideQuery | `sidequery/` |
| 12 | autoDream | `dream/` |
| 13 | Worktree 隔离 | `worktree/` |
| 14 | Coordinator 模式 | `coordinator/` |
| 15 | Paste Store | `paste/` |
| 16 | Session History JSONL | `session/` |

## 附加功能

- **赛博朋克 TUI** — ratatui 三段式布局，青/紫/品红配色
- **Heartbeat** — 周期性后台任务调度
- **Skills** — 可扩展 prompt 模板 + auto-trigger
- **Sandbox** — 四级安全策略（None/ReadOnly/Restricted/Full）
- **Retry Policy** — 指数退避重试
- **Workspace** — 完整 OpenClaw 10 文件加载（SOUL/IDENTITY/USER/AGENTS/MEMORY/STATE/TOOLS/TASKS/HEARTBEAT/memory/）

## 工具

| 工具 | 描述 |
|:---|:---|
| `bash` | 受限模式 shell（黑名单 + 路径保护 + 禁止提权） |
| `read_file` | 文件读取（支持行范围） |
| `write_file` | 文件写入（overwrite/append） |
| `glob` | 文件搜索 |

## 技术栈

- Rust 2021 edition
- tokio 异步运行时
- ratatui + crossterm TUI
- reqwest HTTP + SSE 流式
- serde + serde_json 序列化
- Unix Domain Socket IPC
