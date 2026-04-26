//! Pre-flight Check — lightweight intent classification before main LLM call.
//!
//! Design:
//! - Runs in PARALLEL with the main LLM call (does NOT block the main loop)
//! - Uses JSON-Mode for fast, structured output
//! - Timeout: 3 seconds, then degrade to safe defaults
//! - Output drives:
//!   - Tool interception (if complexity=High, only delegate_complex_project is available)
//!   - TopicShift detection → ShadowEvent::TopicArchived routing
//!
//! ## Output schema
//! ```json
//! { "topic_shift": false, "complexity": "Low", "reason": "simple question" }
//! ```

use crate::message::Message;
use nova_api::client::ApiClient;
use nova_api::types::{ApiMessage, ApiRequest, Content, ContentBlock};
use tracing::{debug, warn};
use std::time::Duration;

/// Pre-flight check result — returned from the lightweight classification LLM call.
#[derive(Debug, Clone, Default)]
pub struct PreFlightCheckResult {
    /// Chain-of-thought reasoning from the classifier
    pub thinking: String,
    /// True if the user's message indicates a topic shift (new topic, context switch)
    pub topic_shift: bool,
    /// Task complexity level — drives tool interception
    pub complexity: Complexity,
    /// Brief reason for the classification
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    Low,
    Medium,
    High,
}

impl Default for Complexity {
    fn default() -> Self {
        Complexity::Low
    }
}

impl Complexity {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "high" => Complexity::High,
            "medium" => Complexity::Medium,
            _ => Complexity::Low,
        }
    }
}

/// System prompt for the pre-flight classifier.
/// Uses JSON-Mode for deterministic, fast output.
const PREFLIGHT_SYSTEM: &str = r#"你是一个对话状态分类器。请先逐步推理，再输出结论。

## 推理步骤（先思考）
1. 分析上下文：当前在讨论什么主题/任务？
2. 分析最新输入：用户想做什么？预估需要几轮工具调用？
3. 话题判断：是否有上下文跳跃或跨领域切换？
4. 复杂度判定：根据工具轮数判断 High/Medium/Low

## 输出格式（严格JSON）
{
  "topic_shift": true或false,
  "complexity": "High"或"Medium"或"Low",
  "reason": "简短原因"
}

## 判断标准

### topic_shift (话题转移)
返回 true：明显的上下文跳跃（"对了"、"换个话题"）、跨领域切换
返回 false：追问、补充、纠错、情绪发泄但针对当前任务

### complexity (执行复杂度)
根据预估的工具调用轮数判断：

**High (≥3轮工具调用，必须派发)**:
- 多步骤任务（跨文件重构、批量修改）
- 浏览器操作（搜索网页、浏览多页、采集数据、填表等多轮交互）
- 搭建服务、完整功能实现
- 需要团队协作的项目

**Medium (1-2轮工具调用，单兵可做)**:
- 单文件修改
- 调试报错
- 解释代码
- **网页查询仅1轮**：直接给出答案的简单查询（如"什么是RAG"）

**Low (无需工具或仅1轮)**:
- 问候、闲聊
- 常识问答
- 理论解释

## Few-Shot 示例

[上下文] User问Rust TCP并发，Assistant回答tokio spawn
[新输入] "tokio spawn报生命周期错误怎么改"
→ complexity: Medium，单点调试

[上下文] User讨论JWT鉴权改Session
[新输入] "把整个鉴权模块从JWT换成Session，前后端都改，加TDD测试"
→ complexity: High，跨前后端+测试，≥3轮

[上下文] User问Rust代码
[新输入] "对了，我鱼缸200L换水问题"
→ topic_shift: true，话题跳跃

[上下文] User问Rust代码
[新输入] "帮我用browser看今天github trending"
→ complexity: High，浏览器多轮交互

[上下文] 讨论GraphRAG
[新输入] "网上有生产落地的案例么"
→ complexity: High，搜索+浏览多页面属于多轮交互

只输出JSON，不要其他文字。"#;

/// Pre-flight classifier that runs in parallel with the main LLM call.
#[derive(Clone)]
pub struct PreFlightChecker {
    api_key: String,
    api_base_url: String,
    model: String,
}

impl PreFlightChecker {
    pub fn new(api_key: String, api_base_url: String, model: String) -> Self {
        Self { api_key, api_base_url, model }
    }

    /// Run the pre-flight check asynchronously.
    /// This should be called BEFORE the main LLM call, not awaited — the main
    /// LLM call starts immediately while this runs in the background.
    ///
    /// Timeout: 3 seconds. On timeout or error, returns safe defaults:
    /// `topic_shift: false, complexity: Low`
    pub async fn check(&self, user_input: &str, recent_messages: &[Message]) -> PreFlightCheckResult {
        // Build a compact context: last 10 messages + user input
        let context = Self::build_sliding_window(recent_messages, user_input, 10);

        let req = ApiRequest {
            model: self.model.clone(),
            max_tokens: 4096,
            system: PREFLIGHT_SYSTEM.to_string(),
            messages: vec![ApiMessage::User {
                content: Content::Text(context),
            }],
            tools: vec![],
            stream: false,
        };

        let api = ApiClient::new(self.api_key.clone(), self.api_base_url.clone());

        // Race: API call vs 30-second timeout
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            api.complete(&req),
        ).await;

        match result {
            Ok(Ok(resp)) => {
                Self::parse_response(&resp)
            }
            Ok(Err(e)) => {
                warn!("Pre-flight check API error: {}, degrading to safe defaults", e);
                PreFlightCheckResult::default()
            }
            Err(_) => {
                debug!("Pre-flight check timed out after 30s, degrading to safe defaults");
                PreFlightCheckResult::default()
            }
        }
    }

    /// Build a compact sliding window of recent messages plus the new user input.
    fn build_sliding_window(messages: &[Message], user_input: &str, max_msgs: usize) -> String {
        let recent: Vec<String> = messages
            .iter()
            .rev()
            .take(max_msgs)
            .filter_map(|m| {
                let role = match m.role {
                    crate::message::Role::User => "User",
                    crate::message::Role::Assistant => "Assistant",
                    crate::message::Role::Tool => "Tool",
                    crate::message::Role::System => "System",
                };
                m.content.as_ref().map(|c| format!("{}: {}", role, c))
            })
            .collect();

        let mut ctx = recent.into_iter().rev().collect::<Vec<_>>().join("\n");
        ctx.push_str(&format!("\nUser最新: {}", user_input));
        ctx
    }

    /// Parse the JSON response from the classifier.
    fn parse_response(resp: &nova_api::types::ApiResponse) -> PreFlightCheckResult {
        // Extract native thinking block if any
        let mut thinking = resp.content.iter().find_map(|block| {
            if let ContentBlock::Thinking { thinking, .. } = block {
                Some(thinking.clone())
            } else {
                None
            }
        }).unwrap_or_default();

        // Extract text block
        let text = resp.content.iter().find_map(|block| {
            if let ContentBlock::Text { text } = block {
                Some(text.clone())
            } else {
                None
            }
        });

        let text = match text {
            Some(t) => t,
            None => {
                warn!("Pre-flight: no text in response, using defaults");
                return PreFlightCheckResult::default();
            }
        };

        // If no native thinking block, try to extract from <think> tags in text
        if thinking.is_empty() {
            if let Some(start) = text.find("<think>") {
                if let Some(end) = text.find("</think>") {
                    thinking = text[start + 7..end].trim().to_string();
                }
            }
        }

        // Clean text to extract JSON (find first { and last })
        let cleaned = if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                &text[start..=end]
            } else {
                text.trim()
            }
        } else {
            text.trim()
        };

        let parsed: Result<serde_json::Value, _> = serde_json::from_str(cleaned);

        match parsed {
            Ok(v) => {
                // Also check if thinking was inside the JSON as a fallback
                if thinking.is_empty() {
                    thinking = v.get("thinking")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                }
                
                let topic_shift = v.get("topic_shift")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let complexity_str = v.get("complexity")
                    .and_then(|x| x.as_str())
                    .unwrap_or("Low");
                let reason = v.get("reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();

                PreFlightCheckResult {
                    thinking,
                    topic_shift,
                    complexity: Complexity::from_str(complexity_str),
                    reason,
                }
            }
            Err(e) => {
                warn!("Pre-flight JSON parse error: {} (raw text: {:?}), using defaults", e, text);
                PreFlightCheckResult::default()
            }
        }
    }
}
