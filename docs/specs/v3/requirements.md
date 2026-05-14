# Nova Skill 自迭代方案

**版本**: v1.0
**日期**: 2026-04-20
**参考**: Hermes Agent 源码
**状态**: 待实现

---

## 一、目标

让 Nova Agent 能够**动态创建、更新、删除 skills**，将成功的经验固化为可复用技能。

---

## 二、核心概念

### Skill = 程序性记忆

| 类型 | 内容 | 例子 |
|:---|:---|:---|
| **事实性记忆** | 偏好、配置 | MEMORY.md |
| **程序性记忆** | 如何做某类任务 | Skill |

### Skill 自迭代的核心价值

- **复杂任务成功后** → 保存为 skill
- **使用 skill 时发现问题** → 立即 patch
- **避免重复同样的错误** → 经验固化

---

## 三、Skill 目录结构

```
~/.nova/skills/
├── my-skill/
│   ├── SKILL.md           # 主指令（必需）
│   ├── references/        # 参考文档
│   │   └── api.md
│   ├── templates/         # 模板
│   │   └── template.md
│   ├── scripts/          # 脚本
│   └── assets/           # 资产
└── category/
    └── another-skill/
        └── SKILL.md
```

---

## 四、SKILL.md 格式

```yaml
---
name: skill-name              # 必需，max 64 chars
description: 简短描述        # 必需，max 1024 chars
version: 1.0.0
platforms: [macos, linux]    # 可选，平台过滤
---

# Skill Title

Full instructions here...
```

---

## 五、工具设计

### 5.1 skill_manage

**管理 skills 的创建、编辑、补丁、删除**

```rust
pub enum SkillAction {
    Create,
    Edit,
    Patch,
    Delete,
    WriteFile,
    RemoveFile,
}

pub struct SkillManageInput {
    pub action: SkillAction,
    pub name: String,
    pub content: Option<String>,      // for create/edit
    pub old_string: Option<String>,   // for patch
    pub new_string: Option<String>,   // for patch
    pub file_path: Option<String>,     // for write_file/remove_file
    pub file_content: Option<String>, // for write_file
    pub replace_all: bool,
}
```

### 5.2 skills_list

**列出所有 skills（minimal metadata，token 高效）**

```rust
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub category: Option<String>,
}

pub fn skills_list() -> Vec<SkillMeta>
```

### 5.3 skill_view

**查看 skill 内容（渐进式披露）**

```rust
pub fn skill_view(name: &str) -> String
pub fn skill_view_with_file(name: &str, file_path: &str) -> String
```

---

## 六、fuzzy_match — 模糊补丁匹配

### 8 策略链（从精确到模糊）

| 策略 | 说明 |
|:---|:---|
| 1. exact | 精确字符串匹配 |
| 2. line_trimmed | 每行去首尾空白 |
| 3. whitespace_normalized | 多空格/tab 压缩成单空格 |
| 4. indentation_flexible | 完全忽略缩进差异 |
| 5. escape_normalized | `\n` → 实际换行 |
| 6. trimmed_boundary | 只 trim 首尾行 |
| 7. unicode_normalized | 智能引号/破折号/省略号 |
| 8. block_anchor | 匹配首尾行，中间用相似度 |

### 策略阈值

- **唯一匹配**：50% 相似度即可
- **多个候选**：70% 相似度
- **超过阈值**：返回错误让用户补充上下文

---

## 七、自迭代 Guidance（SKILLS_GUIDANCE）

```python
SKILLS_GUIDANCE = (
    "After completing a complex task (5+ tool calls), fixing a tricky error, "
    "or discovering a non-trivial workflow, save the approach as a "
    "skill with skill_manage so you can reuse it next time.\n"
    "When using a skill and finding it outdated, incomplete, or wrong, "
    "patch it immediately with skill_manage(action='patch') — don't wait to be asked. "
    "Skills that aren't maintained become liabilities."
)
```

### 创建 skill 的时机

- 复杂任务成功（5+ tool calls）
- 克服了错误
- 用户纠正的方法有效
- 发现非平凡工作流
- 用户要求记住某流程

### 更新 skill 的时机

- 指令过时/错误
- 使用时遇到未覆盖的问题
- 发现 OS 特定的失败
- **立即 patch，不要等被问**

### 删除 skill 的时机

- Skill 不再适用
- 有更好的替代方案

---

## 八、安全机制

### 8.1 路径安全

```rust
// 禁止路径穿越
if has_traversal_component(path) {
    return Err("Path traversal not allowed");
}

// 文件必须在允许的子目录
allowed_dirs = ["references", "templates", "scripts", "assets"]
```

### 8.2 内容安全

```rust
// SKILL.md 必须以 YAML frontmatter 开始
if !content.starts_with("---") {
    return Err("SKILL.md must start with YAML frontmatter");
}

// 内容大小限制
const MAX_SKILL_CONTENT_CHARS: usize = 100_000;
```

### 8.3 原子写入 + 回滚

```rust
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    // 1. 创建临时文件
    let temp = tempfile::mkstemp(dir=path.parent)?;
    // 2. 写入
    write!(temp, "{}", content)?;
    // 3. 原子替换
    std::fs::rename(temp, path)?;
}
```

失败时回滚原始内容。

---

## 九、缓存失效

```rust
// skill_manage 成功后调用
fn invalidate_skill_cache() {
    // 清除内存缓存
    skill_cache.clear();
    // 清除磁盘快照
    std::fs::remove_file(".skills_prompt_snapshot.json");
}
```

---

## 十、渐进式披露

| Tier | 工具 | 返回内容 | Token 消耗 |
|:---|:---|:---|:---|
| 1 | `skills_list()` | name + description | 低 |
| 2 | `skill_view(name)` | 完整 SKILL.md | 中 |
| 3 | `skill_view(name, file_path)` | 具体参考文件 | 按需 |

---

## 十一、参数限制

| 参数 | 限制 |
|:---|:---|
| Skill 名称 | max 64 chars |
| 描述 | max 1024 chars |
| SKILL.md 内容 | max 100,000 chars |
| 单个支持文件 | max 1 MiB |

---

## 十二、实现文件

```
nova-core/src/skills/
├── loader.rs          # 已有：SkillsLoader
├── mod.rs            # 已有
├── manager.rs        # 新增：skill_manage 工具
├── fuzzy.rs          # 新增：fuzzy_match
├── cache.rs          # 新增：缓存管理
└── security.rs      # 新增：安全扫描
```

---

## 十三、Hermes Agent 源码参考

### 源码位置

```
~/Documents/openclaw/projects/hermes-agent/
├── tools/
│   ├── skill_manager_tool.py   # skill_manage 核心
│   ├── skills_tool.py         # skills_list / skill_view
│   ├── skills_sync.py          # 同步机制
│   ├── skills_guard.py         # 安全扫描
│   ├── skills_hub.py          # Hub 集成
│   └── fuzzy_match.py          # 模糊匹配
└── agent/
    ├── prompt_builder.py       # Skill 注入
    ├── skill_utils.py          # Skill 工具函数
    └── skill_memory.py         # 记忆集成
```

### 目录结构示例

```
~/.hermes/skills/
├── mlops/
│   ├── axolotl/
│   │   ├── SKILL.md
│   │   └── references/
│   └── kubeflow/
│       └── SKILL.md
├── devops/
│   └── docker-best-practices/
│       └── SKILL.md
└── coding/
    └── tdd-template/
        ├── SKILL.md
        └── templates/
            └── test_template.py
```
