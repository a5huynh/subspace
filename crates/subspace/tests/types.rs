use subspace::types::*;

#[test]
fn test_message_user() {
    let msg = Message::user("hello");
    assert_eq!(msg.role, Role::User);
    assert_eq!(msg.text(), "hello");
    assert!(!msg.has_tool_calls());
    assert!(msg.tool_calls().is_empty());
}

#[test]
fn test_message_assistant_text() {
    let msg = Message::assistant(vec![ContentBlock::text("world")]);
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(msg.text(), "world");
}

#[test]
fn test_message_tool_calls() {
    let msg = Message::assistant(vec![
        ContentBlock::text("Let me check that."),
        ContentBlock::tool_use("call_1", "read", serde_json::json!({"path": "foo.rs"})),
        ContentBlock::tool_use("call_2", "bash", serde_json::json!({"command": "ls"})),
    ]);

    assert!(msg.has_tool_calls());
    let calls = msg.tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "read");
    assert_eq!(calls[1].name, "bash");
}

#[test]
fn test_content_block_text() {
    let block = ContentBlock::text("hi");
    assert_eq!(block.as_text(), Some("hi"));
}

#[test]
fn test_content_block_tool_result() {
    let block = ContentBlock::tool_result("call_1", "output here", false);
    match &block {
        ContentBlock::ToolResult {
            tool_use_id,
            is_error,
            content,
        } => {
            assert_eq!(tool_use_id, "call_1");
            assert!(!is_error);
            assert_eq!(content[0].as_text(), Some("output here"));
        }
        _ => panic!("expected ToolResult"),
    }
}

#[test]
fn test_usage_accumulate() {
    let mut total = Usage::default();
    let u1 = Usage {
        input_tokens: 100,
        output_tokens: 50,
        ..Default::default()
    };
    let u2 = Usage {
        input_tokens: 200,
        output_tokens: 100,
        cache_read_tokens: 50,
        cache_write_tokens: 10,
    };

    total.accumulate(&u1);
    total.accumulate(&u2);

    assert_eq!(total.input_tokens, 300);
    assert_eq!(total.output_tokens, 150);
    assert_eq!(total.cache_read_tokens, 50);
    assert_eq!(total.total(), 510);
}

#[test]
fn test_thinking_level_budget() {
    assert_eq!(ThinkingLevel::Off.budget_tokens(), None);
    assert!(ThinkingLevel::Low.budget_tokens().unwrap() > 0);
    assert!(
        ThinkingLevel::High.budget_tokens().unwrap() > ThinkingLevel::Low.budget_tokens().unwrap()
    );
}

#[test]
fn test_message_serialization() {
    let msg = Message::user("hello");
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.role, Role::User);
    assert_eq!(parsed.text(), "hello");
}
