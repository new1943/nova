# SKILL_SELF 设计文档

**版本**: v1.0
**日期**: 2026-04-21
**参考**: Hermes Agent `skill_manager_tool.py`, `skills_tool.py`, `fuzzy_match.py`
**状态**: 规划中

---

## 一、目标

让 Nova Agent 能够**动态创建、更新、删除 skills**，将成功的经验固化为可复用技能。

---

## 二、架构概述

```
nova-core/src/skills/
├── loader.rs          # 已有：SkillsLoader（只读加载）
├── mod.rs             # 已有
├── manager.rs        # 新增：skill_manage 工具
├── lister.rs         # 新增：skills_list 工具
├── viewer.rs         # 新增：skill_view 工具
├── fuzzy.rs          # 新增：fuzzy_match 引擎
├── security.rs       # 新增：路径安全 + 注入检测
└── cache.rs          # 新增：缓存失效机制
```

**技能目录**: `~/.nova/skills/<name>/SKILL.md`

---

## 三、核心数据结构

### 3.1 SkillAction 枚举

```rust
pub enum SkillAction {
    Create,
    Edit,
    Patch,
    Delete,
    WriteFile,
    RemoveFile,
}
```

### 3.2 SkillManageInput

```rust
pub struct SkillManageInput {
    pub action: SkillAction,
    pub name: String,               // max 64 chars
    pub content: Option<String>,     // for create/edit (full SKILL.md)
    pub old_string: Option<String>,  // for patch
    pub new_string: Option<String>, // for patch
    pub file_path: Option<String>,   // for write_file/remove_file/patch
    pub file_content: Option<String>, // for write_file
    pub category: Option<String>,    // for create
    pub replace_all: bool,
}
```

### 3.3 SkillMeta（用于 skills_list）

```rust
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub category: Option<String>,
}
```

### 3.4 SKILL.md Frontmatter

```yaml
---
name: skill-name              # 必需，max 64 chars
description: 简短描述        # 必需，max 1024 chars
version: 1.0.0               # 可选
platforms: [macos, linux]    # 可选
---

# Skill Title

Full instructions here...
```

---

## 四、工具设计

### 4.1 skill_manage

**职责**: 管理 skills 的创建、编辑、补丁、删除

| Action | 必需参数 | 说明 |
|:---|:---|:---|
| create | name, content | 创建新 skill（含 frontmatter 验证） |
| edit | name, content | 全量重写 SKILL.md |
| patch | name, old_string, new_string | 模糊补丁（自动选择策略） |
| delete | name | 删除 skill 目录 |
| write_file | name, file_path, file_content | 写支持文件 |
| remove_file | name, file_path | 删除支持文件 |

### 4.2 skills_list

**职责**: 列出所有 skills（minimal metadata，token 高效）

返回 `Vec<SkillMeta>`，仅含 name/description/category。

### 4.3 skill_view

**职责**: 渐进式披露 skill 内容

| Tier | 调用方式 | 返回内容 |
|:---|:---|:---|
| 1 | `skill_view(name)` | 完整 SKILL.md + linked_files 列表 |
| 2 | `skill_view(name, file_path)` | 具体支持文件内容 |

---

## 五、fuzzy_match 引擎

### 8 策略链（按优先级）

| # | 策略 | 说明 |
|:--|:--|:--|
| 1 | exact | 精确字符串匹配 |
| 2 | line_trimmed | 每行去首尾空白 |
| 3 | whitespace_normalized | 多空格/tab 压缩成单空格 |
| 4 | indentation_flexible | 完全忽略缩进差异 |
| 5 | escape_normalized | `\n` → 实际换行 |
| 6 | trimmed_boundary | 只 trim 首尾行 |
| 7 | unicode_normalized | 智能引号/破折号/省略号 |
| 8 | block_anchor | 匹配首尾行，中间用相似度 |

### 阈值规则

- **唯一匹配**: 50% 相似度
- **多个候选**: 70% 相似度
- **超过阈值**: 返回错误让用户补充上下文

---

## 六、安全机制

### 6.1 路径安全

```rust
// 禁止路径穿越
if has_traversal_component(path) {
    return Err("Path traversal not allowed");
}

// 文件必须在允许的子目录
allowed_dirs = ["references", "templates", "scripts", "assets"]
```

### 6.2 内容安全

```rust
// SKILL.md 必须以 YAML frontmatter 开始
if !content.starts_with("---") {
    return Err("SKILL.md must start with YAML frontmatter");
}

// 内容大小限制
const MAX_SKILL_CONTENT_CHARS: usize = 100_000;
```

### 6.3 注入检测

检测 SKILL.md 中的 prompt injection 模式：
- `ignore previous instructions`
- `you are now`
- `<system>`
- `]]>`

### 6.4 原子写入 + 回滚

```rust
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let temp = tempfile::mkstemp(dir=path.parent)?;
    write!(temp, "{}", content)?;
    std::fs::rename(temp, path)?;
}
```

失败时回滚原始内容。

---

## 七、缓存失效

```rust
// skill_manage 成功后调用
fn invalidate_skill_cache() {
    // 重新加载所有 skills
    skills_loader.load_all();
}
```

---

## 八、与现有模块的交互

### 8.1 与 SkillsLoader 的关系

现有 `SkillsLoader` 保持只读功能，新增 `SkillManager` 处理写操作。

### 8.2 与 ToolRegistry 的集成

在 `nova-daemon/src/main.rs::make_tools()` 中注册：

```rust
SkillManageTool::register_builtin(&mut registry);
SkillsListTool::register_builtin(&mut registry);
SkillViewTool::register_builtin(&mut registry);
```

### 8.3 与 QueryLoop 的交互

现有的 skill 注入逻辑（`/skillname` + auto-trigger）保持不变。

---

## 九、参数限制

| 参数 | 限制 |
|:---|:---|
| Skill 名称 | max 64 chars |
| 描述 | max 1024 chars |
| SKILL.md 内容 | max 100,000 chars |
| 单个支持文件 | max 1 MiB |

---

## 十、Hermes 源码参考

```
~/Documents/openclaw/projects/hermes-agent/
├── tools/
│   ├── skill_manager_tool.py   # 核心参考
│   ├── skills_tool.py          # skills_list / skill_view 参考
│   └── fuzzy_match.py          # fuzzy_match 参考
└── tools/path_security.py      # 路径安全参考
```
