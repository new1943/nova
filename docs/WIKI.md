# Nova LLM Wiki 外挂

**版本**: v1.0
**日期**: 2026-04-21
**状态**: 待实践

---

## 一、背景

参考 Karpathy 的 LLM Wiki 模式（X 帖子浏览量 1700 万）。

> 原文: https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f

### Karpathy 的核心思想

- 不用 RAG 的"每次查询从零检索"
- LLM **增量构建和维护**一个持久化的 Wiki
- 知识被"编译"一次后保持最新
- Wiki 是**持久、累积的 artifact**

### 三层架构

| 层级 | 说明 |
|:---|:---|
| **Raw sources** | 原始文档（论文、博客、图片），不可变 |
| **Wiki** | LLM 生成的 Markdown 文件，完全由 LLM 维护 |
| **Schema** | 配置文件（如 AGENTS.md），告诉 LLM Wiki 结构和工作流 |

### 三种操作

| 操作 | 说明 |
|:---|:---|
| **Ingest** | 添加新源 → LLM 处理 → 更新 Wiki → 更新索引 → 追加日志 |
| **Query** | 查询 Wiki → 综合答案 → 答案可写回 Wiki |
| **Lint** | 定期体检：检测不一致、过期内容、孤儿页面 |

---

## 二、目标

构建一个**外挂知识库**，积累：
- 用户调研的链接和内容
- 我调研的结果
- Heartbeat 的自我发现

形成**双方共有的知识库**。

---

## 三、目录结构

```
~/.nova/wiki/
├── index.md                    # 总索引（内容导向）
├── log.md                      # 操作日志（时间导向）
├── research/                   # 调研内容
│   └── llm-wiki.md            # Karpathy LLM Wiki 调研
├── discoveries/                 # Heartbeat 发现
│   └── ...
└── notes/                      # 笔记
    └── ...
```

---

## 四、操作方式

**使用现有工具**，无需开发新工具。

| 操作 | 工具 | 说明 |
|:---|:---|:---|
| 存入 | write_file | 写入 `~/.nova/wiki/` 下的文件 |
| 查询 | read_file | 读取指定页面 |
| 搜索 | grep / glob | 关键词搜索 |
| 列目录 | glob | `~/.nova/wiki/**/*.md` |

---

## 五、与 Obsidian 的关系

| Karpathy 方案 | Nova 实现 |
|:---|:---|
| Obsidian 作为前端 | 可选，先生用 Obsidian 打开 `~/.nova/wiki/` |
| Vault 就是文件夹 | Nova 直接操作 `~/.nova/wiki/` |
| Obsidian Web Clipper | 手动收藏网页到 Vault |
| Graph View | Obsidian 内置 |

**Obsidian Vault 就是普通 Markdown 文件夹，无需 CLI。**

---

## 六、现状

目前已有：
- `~/.nova/wiki/research/llm-wiki.md` — Karpathy LLM Wiki 调研

---

## 七、参考

- Karpathy LLM Wiki: https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f
- InfoQ 解读: https://mp.weixin.qq.com/s/y0JLrC_Af9X0cA_t08ymjg
