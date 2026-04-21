# SKILL_SELF 实现任务

**版本**: v1.0
**日期**: 2026-04-21
**状态**: 已完成

---

## 任务列表

### Phase 1: 基础设施 ✅

- [x] **T1.1**: 扩展 `nova-core/src/skills/mod.rs`，导出新模块
- [x] **T1.2**: 实现 `security.rs` - 路径安全检查、frontmatter 验证、注入检测
- [x] **T1.3**: 实现 `cache.rs` - SkillsLoader 缓存失效机制

### Phase 2: fuzzy_match 引擎 ✅

- [x] **T2.1**: 实现 8 策略模糊匹配链（`fuzzy.rs`）
  - [x] exact
  - [x] line_trimmed
  - [x] whitespace_normalized
  - [x] indentation_flexible
  - [x] escape_normalized
  - [x] trimmed_boundary
  - [x] unicode_normalized
  - [x] block_anchor
- [x] **T2.2**: 实现 `fuzzy_find_and_replace()` 主函数

### Phase 3: 核心工具 ✅

- [x] **T3.1**: 实现 `manager.rs` - `skill_manage` 工具
  - [x] create 逻辑
  - [x] edit 逻辑
  - [x] patch 逻辑（含 fuzzy_match 集成）
  - [x] delete 逻辑
  - [x] write_file 逻辑
  - [x] remove_file 逻辑
- [x] **T3.2**: 实现 `lister.rs` - `skills_list` 工具
- [x] **T3.3**: 实现 `viewer.rs` - `skill_view` 工具（渐进式披露）

### Phase 4: 集成 ✅

- [x] **T4.1**: 在 `nova-core/src/tools/mod.rs` 中注册新工具
- [x] **T4.2**: 在 `nova-daemon/src/main.rs::make_tools()` 中注册 builtin 工具
- [x] **T4.3**: 验证 skill 注入机制（`/skillname` + auto-trigger）仍然正常

### Phase 5: 测试 ⏳

- [ ] **T5.1**: 单元测试 - fuzzy_match 各策略
- [ ] **T5.2**: 单元测试 - security 验证
- [ ] **T5.3**: 集成测试 - skill create/edit/patch/delete 流程

---

## 新增文件

```
nova-core/src/skills/
├── cache.rs       # SharedSkillsLoader + cache invalidation
├── fuzzy.rs       # 9策略模糊匹配引擎
├── lister.rs      # skills_list 工具
├── manager.rs     # skill_manage 工具
├── security.rs    # 路径安全 + frontmatter + 注入检测
└── viewer.rs     # skill_view 工具
```

## 修改文件

| 文件 | 变更 |
|:---|:---|
| `nova-core/src/skills/mod.rs` | 导出新模块 |
| `nova-core/src/tools/mod.rs` | 重新导出 skill tools |
| `nova-daemon/src/main.rs` | SharedSkillsLoader + 工具注册 |
| `nova-daemon/src/discord.rs` | skill injection 锁保护 |
| `nova-core/Cargo.toml` | 添加 strsim, yaml 依赖 |

## 已知问题

- `SkillViewTool.loader` 字段未使用（预留 hot-reload）
- `available` 变量未使用（调试用）
- 未接入 `FileReadTracker`（skill 文件变更不被追踪）

## Build 状态

- [x] `cargo build` - 通过
- [x] `cargo build --release` - 通过
