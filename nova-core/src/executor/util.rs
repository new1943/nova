//! Executor 共享工具函数 — 消除 5 个 executor 文件的代码重复。

use crate::llm_backend::{CompletionContent, CompletionMessage, CompletionResponse, ContentBlock};
use crate::message::{Message, Role, ToolCall};

/// 将 Message 列表转换为 CompletionMessage 列表。
///
/// 正确处理 tool_calls 和 tool_result 的结构保留：
/// - Role::User → CompletionMessage::User { Text }
/// - Role::Assistant → CompletionMessage::Assistant { Blocks([Text + ToolUse...]) }
/// - Role::Tool → CompletionMessage::User { Blocks([ToolResult]) }
/// - Role::System → 跳过
pub fn to_completion_messages(messages: &[Message]) -> Vec<CompletionMessage> {
    let mut result = Vec::new();
    for msg in messages {
        match msg.role {
            Role::System => {}
            Role::User => {
                if let Some(ref content) = msg.content {
                    if !content.is_empty() {
                        result.push(CompletionMessage::User {
                            content: CompletionContent::Text(content.clone()),
                        });
                    }
                }
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                if let Some(ref text) = msg.content {
                    if !text.is_empty() {
                        blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                }
                if let Some(ref tcs) = msg.tool_calls {
                    for tc in tcs {
                        let input = if tc.arguments.is_null() {
                            serde_json::json!({})
                        } else {
                            tc.arguments.clone()
                        };
                        blocks.push(ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input,
                        });
                    }
                }
                if !blocks.is_empty() {
                    result.push(CompletionMessage::Assistant {
                        content: CompletionContent::Blocks(blocks),
                    });
                }
            }
            Role::Tool => {
                if let (Some(ref id), Some(ref content)) = (&msg.tool_call_id, &msg.content) {
                    result.push(CompletionMessage::User {
                        content: CompletionContent::Blocks(vec![ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: content.clone(),
                        }]),
                    });
                }
            }
        }
    }
    result
}

/// 从 CompletionResponse 中提取文本和工具调用
pub fn extract_response(response: &CompletionResponse) -> (String, Vec<ToolCall>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    for block in &response.content {
        match block {
            ContentBlock::Text { text: t } => {
                text.push_str(t);
            }
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: input.clone(),
                });
            }
            _ => {}
        }
    }

    (text, tool_calls)
}
