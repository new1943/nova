# 记忆系统问题记录

> 日期：2026-04-16
> 背景：在 GitHub Trending 浏览中注意到 `forrestchang/andrej-karpathy-skills` 项目，进而触发关于 Nova 记忆系统的回顾与讨论。

---

## 一、双写互斥记忆 → 三层记忆的演进

### 旧方案：双写互斥（Strategy 7）

**设计目的**：解决主 agent 和 forked agent 重复写入记忆的问题。

**核心机制**：

```
每个 turn 开始：
  dual_write.clear_marker(session_id)

主 agent 写入后：
  dual_write.mark_written(session_id)

forked agent 写入前：
  if dual_write.has_writes_since(session_id) {
    return; // 跳过
  }
  // 写入 + mark_written
```

**文件**：`nova-core/src/memory/dual_write.rs`、`hooks/post_sampling.rs`、`hooks/stop.rs`

**问题**：基于 JSONL 追加写入，LLM 无法直接读取和理解记忆内容。

---

### 新方案：三层记忆（T21）

**验收标准**：
- 层1 MEMORY.md：始终注入 system prompt，LLM 可主动维护
- 层2 日记：Compact 前和 Session 结束时自动写入 `memories/YYYY-MM-DD.md`，经 LLM 摘要
- 层3 JSONL：完整日志，通过 `session_search` 召回

**实现状态**：

| 层级 | 触发时机 | 状态 |
|------|---------|------|
| 层1 MEMORY.md | LLM 主动写 / Dream 整理 | ❌ LLM 不写，Dream 不自动跑 |
| 层2 日记 | Compact 前 + Session 结束时 | ✅ 正常工作 |
| 层3 JSONL | 每条消息实时追加 | ✅ 正常工作 |

**相关文件**：
- `nova-core/src/memory/dream.rs` — DreamEngine，4-phase consolidation
- `nova-core/src/memory/daily.rs` — DailyNotes，日记写入
- `nova-core/src/memory/recall.rs` — MemoryRecall，日记召回管线
- `nova-core/src/session/history.rs` — SessionHistory，JSONL 追加
- `nova-core/src/session/search.rs` — AgenticSessionSearch，跨 Session 语义搜索
- `nova-daemon/src/main.rs` — `write_session_diary()` 在 Compact 前和 Session 结束时调用

**记忆 Guidance**（`nova-core/src/workspace/loader.rs`）：

> 偏好 > 程序细节，任务日志不进记忆，靠 session_search 召回。

**停用旧系统**（tasks.md T21.1）：
> 移除 daemon 中 DualWriteMemory / MemoryExtractHook / MemoryExtractStopHook 的注册，不再写 JSONL。

⚠️ **待确认**：旧代码文件仍存在于 `memory/dual_write.rs`、`hooks/post_sampling.rs`、`hooks/stop.rs`，但 daemon 应已不再注册。需要验证。

---

## 二、层1 MEMORY.md 的缺失问题

### 问题1：LLM 不主动写入 MEMORY.md

**现象**：三层记忆 Guidance 里鼓励 LLM 主动写，但实际对话中 LLM 从不写。

**Karpathy 的启示**（来自 `forrestchang/andrej-karpathy-skills`）：

> "Don't tell it what to do, give it success criteria and watch it go."
> "LLMs are exceptionally good at looping until they meet specific goals."

**类比**：与其在 prompt 里堆"不要重复写入"之类的约束，不如把规则内置。

**可能的解决方向**：
- Session 结束时强制 LLM 写一笔 MEMORY.md（类似写日记的逻辑）
- 或者让 LLM 在每个重要决策点主动写

### 问题2：Dream 没有心跳自动触发

**现状**：
- `DreamEngine` 代码已完成
- 触发条件：`should_dream()` — 锁文件过期（>24h）且 >=5 个新 session，或手动 `/dream`
- **daemon 完全没有启动 Dream 的循环**
- `HeartbeatScheduler`（`nova-core/src/heartbeat/scheduler.rs`）也已写好，但**daemon 没有使用它**

**相关代码**：

```rust
// nova-daemon/src/main.rs — Dream 只在这里被检查（每 turn 一次）
if dream_engine.should_dream() {
    let de = dream_engine.clone();
    let mtime = session.token_stats.memory_mtime;
    tokio::spawn(async move {
        if let Err(e) = de.dream(mtime).await { ... }
    });
}
```

但 `should_dream()` 需要锁文件过期 + >=5 新 session，门槛较高，不会频繁触发。

---

## 三、Karpathy Skills 的启示

**项目**：`forrestchang/andrej-karpathy-skills`（今日 GitHub Trending #1，+9,646 stars）

**核心**：基于 Karpathy 对 LLM 编程缺点的观察，总结 4 条原则改进 Claude Code：

| 原则 | 解决什么问题 |
|------|-------------|
| Think Before Coding | 错误假设、隐藏困惑、遗漏权衡 |
| Simplicity First | 过度复杂化、臃肿抽象 |
| Surgical Changes | 改动无关代码、side effect 修改不理解的内容 |
| Goal-Driven Execution | 设定成功标准，用测试驱动循环验证 |

**与 Nova 的关联**：
- Nova 的记忆 Guidance 已经在践行"给目标而不是给步骤"
- Dream 的 4-phase consolidation 也是类似思路：读取日记 → 用 LLM 蒸馏 → 重写 MEMORY.md

---

## 四、待解决问题（已通过 T23 方案解决）

- **痛点**：期望 Session 结束时强制写，但实际上用户经常不会主动 `/exit`，导致写时机难以捕捉。若高频判定又太费 Token。
- **解决方案：基于“闲时超时与压缩阻截”的双写互斥归档方案 (T23)**

### 4.1. 核心设计：把时间的空隙作为话题断点

我们不依赖用户主动结束，而是部署两道静默“捕网”：
1. **闲时扫描（时间断点检测）**：依托 `nova-daemon` 后台，每隔几分钟轮询活跃 Session。若距上次发言时间 > 15分钟，且游标后有新消息，则视为隐性话题断点。
2. **压缩阻截（空间断点检测）**：依托 `loop.rs`，当触发 `NeedsCompact` 即将折叠上下文时前置触发。

### 4.2. 双重互斥管线 (Mutex Pipeline)

遇到断点时，提取本次未处理的增量条目并检查 `memory_updated_mutex`：
- **`true`（已被主动写入跳过）**：说明主对话期间，LLM 非常尽职地主动修改了 `MEMORY.md`。此时只需重置游标至最新即可结束扫尾（无缝跳过，防幻觉、省 Token）。
- **`false`（兜底合并与补漏）**：唤醒后台影子引擎 (`SideQuery`)，传入增量材料与严格过滤 Prompt。屏蔽一切日常杂碎逻辑，只准将“持久化用户偏好（如‘用 Rust 代替 Node’）或系统级跨越状态”合并进层1工作记忆中。

这种“主巡防兼顾+兜底截断（互斥短路）”的自适应断网机制，彻底抹平了 LLM “健忘/污染 System Prompt 资源”的设计矛盾，完全顺应克制原则与第一性经济思维。
