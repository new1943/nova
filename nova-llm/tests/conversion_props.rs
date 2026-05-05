// Feature: r3-open-connectivity, Property 1: CompletionRequest ↔ ApiRequest 转换往返保真
// **Validates: Requirements 2.1, 2.2, 2.3**
//
// For any valid CompletionRequest (random model, messages, tools),
// converting to ApiRequest preserves all field values.

use nova_core::llm_backend::{
    CompletionContent, CompletionMessage, CompletionRequest, ContentBlock as CoreContentBlock,
    ToolSchema as CoreToolSchema,
};
use nova_llm::client::ApiClient;
use nova_llm::types::{ApiMessage, Content, ContentBlock};
use proptest::prelude::*;

// --- Generators ---

fn arb_content_block() -> impl Strategy<Value = CoreContentBlock> {
    prop_oneof![
        any::<String>().prop_map(|text| CoreContentBlock::Text { text }),
        (any::<String>(), proptest::option::of(any::<String>())).prop_map(
            |(thinking, signature)| CoreContentBlock::Thinking {
                thinking,
                signature,
            }
        ),
        (any::<String>(), any::<String>()).prop_map(|(id, name)| CoreContentBlock::ToolUse {
            id,
            name,
            input: serde_json::json!({"key": "value"}),
        }),
        (any::<String>(), any::<String>()).prop_map(|(tool_use_id, content)| {
            CoreContentBlock::ToolResult {
                tool_use_id,
                content,
            }
        }),
    ]
}

fn arb_completion_content() -> impl Strategy<Value = CompletionContent> {
    prop_oneof![
        any::<String>().prop_map(CompletionContent::Text),
        proptest::collection::vec(arb_content_block(), 1..4).prop_map(CompletionContent::Blocks),
    ]
}

fn arb_completion_message() -> impl Strategy<Value = CompletionMessage> {
    prop_oneof![
        arb_completion_content().prop_map(|content| CompletionMessage::User { content }),
        arb_completion_content().prop_map(|content| CompletionMessage::Assistant { content }),
    ]
}

fn arb_tool_schema() -> impl Strategy<Value = CoreToolSchema> {
    (any::<String>(), any::<String>()).prop_map(|(name, description)| CoreToolSchema {
        name,
        description,
        input_schema: serde_json::json!({"type": "object", "properties": {}}),
    })
}

fn arb_completion_request() -> impl Strategy<Value = CompletionRequest> {
    (
        any::<String>(),                                    // model
        1u32..100000u32,                                    // max_tokens
        any::<String>(),                                    // system
        proptest::collection::vec(arb_completion_message(), 0..5), // messages
        proptest::collection::vec(arb_tool_schema(), 0..4), // tools
        any::<bool>(),                                      // stream
    )
        .prop_map(
            |(model, max_tokens, system, messages, tools, stream)| CompletionRequest {
                model,
                max_tokens,
                system,
                messages,
                tools,
                stream,
            },
        )
}

// --- Property Tests ---

proptest! {
    #[test]
    fn prop_completion_request_to_api_request_preserves_fields(req in arb_completion_request()) {
        let client = ApiClient::new("test-key".into(), "http://localhost".into());
        let api_req = client.to_api_request(&req);

        // Verify top-level fields
        prop_assert_eq!(&api_req.model, &req.model);
        prop_assert_eq!(api_req.max_tokens, req.max_tokens);
        prop_assert_eq!(&api_req.system, &req.system);
        prop_assert_eq!(api_req.stream, req.stream);
        prop_assert_eq!(api_req.messages.len(), req.messages.len());
        prop_assert_eq!(api_req.tools.len(), req.tools.len());

        // Verify messages
        for (api_msg, core_msg) in api_req.messages.iter().zip(req.messages.iter()) {
            match (api_msg, core_msg) {
                (ApiMessage::User { content: api_content }, CompletionMessage::User { content: core_content }) => {
                    assert_content_matches(api_content, core_content);
                }
                (ApiMessage::Assistant { content: api_content }, CompletionMessage::Assistant { content: core_content }) => {
                    assert_content_matches(api_content, core_content);
                }
                _ => prop_assert!(false, "Message role mismatch"),
            }
        }

        // Verify tools
        for (api_tool, core_tool) in api_req.tools.iter().zip(req.tools.iter()) {
            prop_assert_eq!(&api_tool.name, &core_tool.name);
            prop_assert_eq!(&api_tool.description, &core_tool.description);
            prop_assert_eq!(&api_tool.input_schema, &core_tool.input_schema);
        }
    }
}

// --- Helper functions ---

fn assert_content_matches(api_content: &Content, core_content: &CompletionContent) {
    match (api_content, core_content) {
        (Content::Text(api_text), CompletionContent::Text(core_text)) => {
            assert_eq!(api_text, core_text);
        }
        (Content::Blocks(api_blocks), CompletionContent::Blocks(core_blocks)) => {
            assert_eq!(api_blocks.len(), core_blocks.len());
            for (api_block, core_block) in api_blocks.iter().zip(core_blocks.iter()) {
                assert_block_matches(api_block, core_block);
            }
        }
        _ => panic!("Content type mismatch"),
    }
}

fn assert_block_matches(api_block: &ContentBlock, core_block: &CoreContentBlock) {
    match (api_block, core_block) {
        (ContentBlock::Text { text: api_text }, CoreContentBlock::Text { text: core_text }) => {
            assert_eq!(api_text, core_text);
        }
        (
            ContentBlock::Thinking {
                thinking: api_thinking,
                signature: api_sig,
            },
            CoreContentBlock::Thinking {
                thinking: core_thinking,
                signature: core_sig,
            },
        ) => {
            assert_eq!(api_thinking, core_thinking);
            assert_eq!(api_sig, core_sig);
        }
        (
            ContentBlock::ToolUse {
                id: api_id,
                name: api_name,
                input: api_input,
            },
            CoreContentBlock::ToolUse {
                id: core_id,
                name: core_name,
                input: core_input,
            },
        ) => {
            assert_eq!(api_id, core_id);
            assert_eq!(api_name, core_name);
            assert_eq!(api_input, core_input);
        }
        (
            ContentBlock::ToolResult {
                tool_use_id: api_id,
                content: api_content,
            },
            CoreContentBlock::ToolResult {
                tool_use_id: core_id,
                content: core_content,
            },
        ) => {
            assert_eq!(api_id, core_id);
            assert_eq!(api_content, core_content);
        }
        _ => panic!("ContentBlock variant mismatch"),
    }
}
