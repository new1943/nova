use std::fmt;

use crate::agent::preflight::{Complexity, PreFlightCheckResult};
use crate::message::Message;

/// TurnContext — 单次对话 turn 的共享数据载体。
///
/// 所有 PipelineStage 通过读写 TurnContext 字段来协作，
/// 而不是直接操作 QueryLoop 内部状态。
///
/// 规则：
/// - 每个 Stage "写自己负责的字段，读别人的字段"
/// - Stage 之间不直接通信，只通过 TurnContext 间接交互
/// - 下游 Stage 可以读取上游 Stage 写入的结果
#[derive(Debug)]
pub struct TurnContext {
    // ── 输入（由调用者设置，Stage 只读） ────────────────────────
    /// 用户本轮输入的原始文本
    pub user_input: String,
    /// 最近的历史消息（用于上下文推断）
    pub recent_messages: Vec<Message>,

    // ── ClassifyStage 输出 ──────────────────────────────────────
    /// Preflight 分类结果（由 ClassifyStage 写入）
    pub preflight_result: Option<PreFlightCheckResult>,
    /// 分类出的复杂度（ClassifyStage 写入，GateStage / ExecuteConfigStage 读取）
    pub complexity: Complexity,

    // ── TrackStage 输出 ─────────────────────────────────────────
    /// 是否检测到话题切换（TrackStage 写入，InjectStage 读取）
    pub topic_shift: bool,
    /// 当前张力值（TrackStage 写入）
    pub tension: u8,

    // ── GateStage 输出 ──────────────────────────────────────────
    /// 允许的工具名称列表（GateStage 写入，QueryLoop 读取）
    /// None = 所有工具可见，Some = 白名单过滤
    pub allowed_tools: Option<Vec<String>>,

    // ── InjectStage 输出 ────────────────────────────────────────
    /// 需要追加到 system prompt 的上下文片段（InjectStage 写入）
    pub prompt_injections: Vec<PromptInjection>,

    // ── ExecuteConfigStage 输出 ─────────────────────────────────
    /// 是否应在工具调用后立即终止循环（委派场景）
    pub should_terminate_after_tool: bool,

    // ── 决策日志（所有 Stage 可追加） ───────────────────────────
    pub decision_log: Vec<DecisionEntry>,
}

/// 注入到 system prompt 的上下文片段
#[derive(Debug, Clone)]
pub struct PromptInjection {
    /// 标签名称（用于生成 XML 包裹）
    pub tag: String,
    /// 注入内容
    pub content: String,
    /// 优先级（数字越小越靠前）
    pub priority: u8,
}

/// 决策日志条目 — 用于可观测性和调试
#[derive(Debug, Clone)]
pub struct DecisionEntry {
    pub stage: String,
    pub decision: String,
    pub reason: String,
}

impl TurnContext {
    /// 创建新的 TurnContext
    pub fn new(user_input: String, recent_messages: Vec<Message>) -> Self {
        Self {
            user_input,
            recent_messages,
            preflight_result: None,
            complexity: Complexity::Low,
            topic_shift: false,
            tension: 0,
            allowed_tools: None,
            prompt_injections: Vec::new(),
            should_terminate_after_tool: false,
            decision_log: Vec::new(),
        }
    }

    /// 记录一条决策日志
    pub fn log_decision(&mut self, stage: &str, decision: &str, reason: &str) {
        self.decision_log.push(DecisionEntry {
            stage: stage.to_string(),
            decision: decision.to_string(),
            reason: reason.to_string(),
        });
    }

    /// 将所有 prompt_injections 按优先级排序后拼装为注入字符串
    pub fn build_injection_string(&self) -> String {
        let mut sorted = self.prompt_injections.clone();
        sorted.sort_by_key(|p| p.priority);
        sorted.iter()
            .map(|p| format!("\n\n<{}>\n{}\n</{}>", p.tag, p.content, p.tag))
            .collect::<Vec<_>>()
            .join("")
    }
}

impl fmt::Display for TurnContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TurnContext(complexity={:?}, topic_shift={}, tools={}, injections={}, terminate={})",
            self.complexity,
            self.topic_shift,
            self.allowed_tools.as_ref().map(|t| t.len().to_string()).unwrap_or("all".into()),
            self.prompt_injections.len(),
            self.should_terminate_after_tool,
        )
    }
}

/// PipelineStage — TurnPipeline 中的一个处理阶段。
///
/// 每个 Stage 负责读取 TurnContext 中前置 Stage 的输出，
/// 并写入自己负责的字段。
#[async_trait::async_trait]
pub trait PipelineStage: Send + Sync {
    /// Stage 名称（用于日志和决策记录）
    fn name(&self) -> &str;

    /// 执行此 Stage 的逻辑，修改 TurnContext
    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()>;
}

/// TurnPipeline — 按顺序执行一系列 PipelineStage。
///
/// ```text
/// Classify → Track → Gate → Inject → ExecuteConfig
///    │          │       │       │          │
///    └──────────┴───────┴───────┴──────────┘
///               共享 TurnContext
/// ```
pub struct TurnPipeline {
    stages: Vec<Box<dyn PipelineStage>>,
}

impl TurnPipeline {
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// 添加一个 Stage 到 Pipeline 末尾
    pub fn add_stage(&mut self, stage: Box<dyn PipelineStage>) {
        self.stages.push(stage);
    }

    /// 按顺序执行所有 Stage
    pub async fn run(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        for stage in &self.stages {
            tracing::info!("Pipeline: running stage '{}'", stage.name());
            stage.execute(ctx).await?;
            tracing::debug!("Pipeline: after '{}' → {}", stage.name(), ctx);
        }
        tracing::info!("Pipeline complete: {}", ctx);
        // 输出决策日志
        for entry in &ctx.decision_log {
            tracing::info!("  [{}] {} — {}", entry.stage, entry.decision, entry.reason);
        }
        Ok(())
    }
}

impl Default for TurnPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyStage {
        name: String,
        complexity: Complexity,
    }

    #[async_trait::async_trait]
    impl PipelineStage for DummyStage {
        fn name(&self) -> &str { &self.name }
        async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
            ctx.complexity = self.complexity;
            ctx.log_decision(&self.name, "set_complexity", &format!("{:?}", self.complexity));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_pipeline_runs_stages_in_order() {
        let mut pipeline = TurnPipeline::new();
        pipeline.add_stage(Box::new(DummyStage {
            name: "classify".into(),
            complexity: Complexity::High,
        }));

        let mut ctx = TurnContext::new("test input".into(), vec![]);
        assert_eq!(ctx.complexity, Complexity::Low); // default

        pipeline.run(&mut ctx).await.unwrap();
        assert_eq!(ctx.complexity, Complexity::High);
        assert_eq!(ctx.decision_log.len(), 1);
        assert_eq!(ctx.decision_log[0].stage, "classify");
    }

    #[tokio::test]
    async fn test_injection_string_sorted_by_priority() {
        let mut ctx = TurnContext::new("test".into(), vec![]);
        ctx.prompt_injections.push(PromptInjection {
            tag: "tasks".into(),
            content: "task list".into(),
            priority: 10,
        });
        ctx.prompt_injections.push(PromptInjection {
            tag: "preflight".into(),
            content: "complexity: High".into(),
            priority: 1,
        });

        let injection = ctx.build_injection_string();
        assert!(injection.find("<preflight>").unwrap() < injection.find("<tasks>").unwrap());
    }
}
