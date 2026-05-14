# 任务列表：R4 自进化

## 任务

- [x] 1. AGENTS.md 行为规范更新
  - [x] 1.1 在"二、记忆系统"章节中添加"记忆内容策略"子节（应存储/不应存储/核心原则/工具分流指南）
  - [x] 1.2 新增"四、工具使用纪律"章节（核心规则 + 必须使用工具的场景）
  - [x] 1.3 调整后续章节编号（安全红线→五、SubAgent→六）
  - [x] 1.4 同步更新 nova-core/prompts/AGENTS.md（保持两份一致）

- [x] 2. nova-core 原子写入工具函数
  - [x] 2.1 创建 `nova-core/src/atomic_write.rs` 模块，实现 `atomic_write(path, content)` 同步版本
  - [x] 2.2 实现 `atomic_write_async(path, content)` 异步版本（使用 tokio::fs）
  - [x] 2.3 在 `nova-core/src/lib.rs` 中注册 `pub mod atomic_write`
  - [x] 2.4 编写属性测试 `nova-core/tests/atomic_write_props.rs`（Property 7: 内容保持性，≥100 iterations）
  - [x] 2.5 编写边界测试：rename 失败时清理 temp 文件、无效路径处理

- [x] 3. 原子写入集成（各组件替换直接写入）
  - [x] 3.1 MemoryKeeper: 将 `fs::write(&memory_path, updated)` 替换为 `atomic_write_async`
  - [x] 3.2 MemoryConsolidator: 将 `fs::write(&memory_path, response)` 替换为 `nova_core::atomic_write::atomic_write`
  - [x] 3.3 TaskManager: 将 `write_file()` 中的 `std::fs::write` 替换为 `nova_core::atomic_write::atomic_write`
  - [x] 3.4 SessionManager: 将 `save_meta()` 中的 `std::fs::write` 替换为 `nova_core::atomic_write::atomic_write`

- [x] 4. InjectStage Skill 渐进式披露改造
  - [x] 4.1 为 InjectStage 添加 `skills_loader: Option<SharedSkillsLoader>` 字段和构造函数参数
  - [x] 4.2 实现 `build_skills_summary()` 方法：从 loader 读取所有 Skill 元数据，生成摘要文本
  - [x] 4.3 实现 `get_auto_triggered_skills()` 方法：匹配 user_input 中的 auto_trigger keywords
  - [x] 4.4 在 `execute()` 中注入 `<available-skills>` 摘要标签（priority 适当）
  - [x] 4.5 在 `execute()` 中注入 auto_trigger 匹配的完整 Skill 内容
  - [x] 4.6 更新 InjectStage 的调用方（nova-agent 中构造 Pipeline 的代码）传入 skills_loader
  - [x] 4.7 编写属性测试 `nova-agent/tests/inject_skills_props.rs`（Property 5 + Property 6）

- [x] 5. Pipeline 单元测试
  - [x] 5.1 创建 mock PreFlightChecker（或直接手动设置 TurnContext 字段绕过 ClassifyStage）
  - [x] 5.2 编写 `nova-agent/tests/pipeline_high_complexity.rs`：高复杂度 → 仅委派工具 + terminate
  - [x] 5.3 编写 `nova-agent/tests/pipeline_low_topic_shift.rs`：低复杂度 + topic_shift → 全工具可见

- [x] 6. Dispatcher 集成测试
  - [x] 6.1 编写 `nova-daemon/tests/dispatcher_topic_archived.rs`：TopicArchived 路由到 MemoryKeeper
  - [x] 6.2 编写降级测试：无 MemoryKeeper 时不 panic

- [x] 7. 文档更新
  - [x] 7.1 更新 CLAUDE.md：8 crate 结构、TurnPipeline Stage 说明、LlmBackend/PlatformAdapter trait、依赖图
  - [x] 7.2 更新 docs/README.md：8 crate 依赖关系图、核心数据流、ShadowEvent 事件系统、记忆四层架构

- [x] 8. 构建验证
  - [x] 8.1 `cargo build --release` 编译通过
  - [x] 8.2 `cargo test` 全部测试通过（含新增测试）
  - [x] 8.3 `cargo clippy --all-targets` 无 warning
