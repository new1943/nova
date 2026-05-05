// Feature: r4-self-evolution, Task 5.3: Pipeline 单元测试 — 低复杂度 + 话题切换
//
// Strategy: Bypass ClassifyStage by directly setting TurnContext fields,
// then run GateStage + ExecuteConfigStage and verify the final state.

use nova_core::pipeline::{PipelineStage, TurnContext};
use nova_core::preflight_types::Complexity;
use nova_core::message::Message;
use nova_agent::stages::gate::GateStage;
use nova_agent::stages::execute_config::ExecuteConfigStage;

#[tokio::test]
async fn test_low_complexity_topic_shift_all_tools_visible() {
    // Simulate ClassifyStage output: Low complexity + topic shift detected
    let recent = vec![
        Message::user("帮我看看这个Rust代码的性能问题"),
        Message::assistant(Some("好的，我来分析一下...".into()), None),
    ];
    let mut ctx = TurnContext::new("对了，你几岁了？".into(), recent);
    ctx.complexity = Complexity::Low;
    ctx.topic_shift = true;

    // Run GateStage
    let gate = GateStage::new();
    gate.execute(&mut ctx).await.unwrap();

    // Run ExecuteConfigStage
    let exec_config = ExecuteConfigStage::new();
    exec_config.execute(&mut ctx).await.unwrap();

    // Assert: complexity remains Low
    assert_eq!(ctx.complexity, Complexity::Low);

    // Assert: topic_shift is true
    assert!(ctx.topic_shift);

    // Assert: all tools visible (no restriction)
    assert!(
        ctx.allowed_tools.is_none(),
        "Low complexity should not restrict tools, got: {:?}",
        ctx.allowed_tools
    );

    // Assert: should NOT terminate after tool call (normal interactive mode)
    assert!(!ctx.should_terminate_after_tool);
}

#[tokio::test]
async fn test_low_complexity_no_topic_shift_all_tools_visible() {
    // Low complexity without topic shift — same tool behavior
    let mut ctx = TurnContext::new("你好".into(), vec![]);
    ctx.complexity = Complexity::Low;
    ctx.topic_shift = false;

    let gate = GateStage::new();
    gate.execute(&mut ctx).await.unwrap();

    let exec_config = ExecuteConfigStage::new();
    exec_config.execute(&mut ctx).await.unwrap();

    // Assert: all tools visible
    assert!(ctx.allowed_tools.is_none());

    // Assert: no termination
    assert!(!ctx.should_terminate_after_tool);
}

#[tokio::test]
async fn test_low_complexity_topic_shift_decision_log() {
    let mut ctx = TurnContext::new("换个话题吧".into(), vec![]);
    ctx.complexity = Complexity::Low;
    ctx.topic_shift = true;

    let gate = GateStage::new();
    gate.execute(&mut ctx).await.unwrap();

    let exec_config = ExecuteConfigStage::new();
    exec_config.execute(&mut ctx).await.unwrap();

    // Verify gate logged "all tools visible"
    let gate_entry = ctx.decision_log.iter()
        .find(|e| e.stage == "gate")
        .expect("GateStage should log a decision");
    assert!(
        gate_entry.decision.contains("all"),
        "Gate decision should mention 'all' tools, got: {}",
        gate_entry.decision
    );

    // Verify execute_config logged no termination
    let exec_entry = ctx.decision_log.iter()
        .find(|e| e.stage == "execute_config")
        .expect("ExecuteConfigStage should log a decision");
    assert!(
        exec_entry.decision.contains("false"),
        "ExecuteConfig should log terminate=false, got: {}",
        exec_entry.decision
    );
}
