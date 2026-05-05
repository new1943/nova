// Feature: r4-self-evolution, Task 5.2: Pipeline 单元测试 — 高复杂度场景
//
// Strategy: Bypass ClassifyStage by directly setting TurnContext fields,
// then run GateStage + ExecuteConfigStage and verify the final state.

use nova_core::pipeline::{PipelineStage, TurnContext};
use nova_core::preflight_types::Complexity;
use nova_agent::stages::gate::GateStage;
use nova_agent::stages::execute_config::ExecuteConfigStage;

#[tokio::test]
async fn test_high_complexity_only_delegate_tools_and_terminate() {
    // Simulate ClassifyStage output: High complexity, no topic shift
    let mut ctx = TurnContext::new("帮我重构整个项目的架构".into(), vec![]);
    ctx.complexity = Complexity::High;
    ctx.topic_shift = false;

    // Run GateStage
    let gate = GateStage::new();
    gate.execute(&mut ctx).await.unwrap();

    // Run ExecuteConfigStage
    let exec_config = ExecuteConfigStage::new();
    exec_config.execute(&mut ctx).await.unwrap();

    // Assert: complexity remains High
    assert_eq!(ctx.complexity, Complexity::High);

    // Assert: only delegate tools are allowed
    let allowed = ctx.allowed_tools.as_ref().expect("allowed_tools should be Some for High complexity");
    assert_eq!(allowed, &vec![
        "delegate_complex_project".to_string(),
        "cancel_delegated_project".to_string(),
    ]);

    // Assert: should terminate after tool call (delegation mode)
    assert!(ctx.should_terminate_after_tool);
}

#[tokio::test]
async fn test_medium_complexity_delegate_task_and_terminate() {
    // Simulate ClassifyStage output: Medium complexity
    let mut ctx = TurnContext::new("帮我修复这个文件的bug".into(), vec![]);
    ctx.complexity = Complexity::Medium;
    ctx.topic_shift = false;

    // Run GateStage
    let gate = GateStage::new();
    gate.execute(&mut ctx).await.unwrap();

    // Run ExecuteConfigStage
    let exec_config = ExecuteConfigStage::new();
    exec_config.execute(&mut ctx).await.unwrap();

    // Assert: Medium uses delegate_task + cancel
    let allowed = ctx.allowed_tools.as_ref().expect("allowed_tools should be Some for Medium complexity");
    assert_eq!(allowed, &vec![
        "delegate_task".to_string(),
        "cancel_delegated_project".to_string(),
    ]);

    // Assert: should also terminate after tool call
    assert!(ctx.should_terminate_after_tool);
}

#[tokio::test]
async fn test_high_complexity_decision_log_recorded() {
    let mut ctx = TurnContext::new("重构整个项目".into(), vec![]);
    ctx.complexity = Complexity::High;

    let gate = GateStage::new();
    gate.execute(&mut ctx).await.unwrap();

    let exec_config = ExecuteConfigStage::new();
    exec_config.execute(&mut ctx).await.unwrap();

    // Verify decision log entries exist for both stages
    let gate_entries: Vec<_> = ctx.decision_log.iter()
        .filter(|e| e.stage == "gate")
        .collect();
    assert!(!gate_entries.is_empty(), "GateStage should log decisions");

    let exec_entries: Vec<_> = ctx.decision_log.iter()
        .filter(|e| e.stage == "execute_config")
        .collect();
    assert!(!exec_entries.is_empty(), "ExecuteConfigStage should log decisions");
}
