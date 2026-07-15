use std::sync::Arc;

use subspace::agent::{Agent, AgentConfig, AgentEvent};
use subspace::provider::mock::{MockProvider, MockResponse};
use subspace::tools::ToolRegistry;
use subspace::types::*;

#[tokio::test]
async fn test_agent_simple_text_response() {
    let provider = Arc::new(MockProvider::with_text("Hello from the agent!"));
    let tools = ToolRegistry::new();
    let agent = Agent::new(AgentConfig::default(), provider, tools);

    let (mut rx, handle) = agent.prompt("Hi").await;

    let mut saw_start = false;
    let mut saw_text = false;
    let mut saw_end = false;
    let mut text_accum = String::new();

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::AgentStart => saw_start = true,
            AgentEvent::TextDelta(t) => {
                saw_text = true;
                text_accum.push_str(&t);
            }
            AgentEvent::AgentEnd { messages, .. } => {
                saw_end = true;
                // Should have 1 assistant message
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].role, Role::Assistant);
            }
            _ => {}
        }
    }

    handle.await.unwrap();

    assert!(saw_start);
    assert!(saw_text);
    assert!(saw_end);
    assert_eq!(text_accum, "Hello from the agent!");
}

#[tokio::test]
async fn test_agent_tool_call_loop() {
    // Mock: first response calls bash tool, second response is final text
    let provider = Arc::new(MockProvider::new(vec![
        MockResponse::tool_call(
            "call_1",
            "bash",
            serde_json::json!({"command": "echo hello"}),
        ),
        MockResponse::text("The command output was: hello"),
    ]));

    let dir = std::env::temp_dir().join("subspace_test_agent_tool");
    std::fs::create_dir_all(&dir).unwrap();
    let tools = ToolRegistry::coding_defaults(dir.clone());

    let agent = Agent::new(AgentConfig::default(), provider, tools);
    let (mut rx, handle) = agent.prompt("Run echo hello").await;

    let mut events: Vec<String> = Vec::new();
    let mut tool_exec_seen = false;
    let mut final_text = String::new();

    while let Some(event) = rx.recv().await {
        match &event {
            AgentEvent::AgentStart => events.push("agent_start".into()),
            AgentEvent::TurnStart { turn } => events.push(format!("turn_start:{turn}")),
            AgentEvent::ToolCallStart { name, .. } => events.push(format!("tool_call:{name}")),
            AgentEvent::ToolExecEnd { name, result, .. } => {
                tool_exec_seen = true;
                events.push(format!("tool_exec_end:{name}:err={}", result.is_error));
            }
            AgentEvent::TextDelta(t) => final_text.push_str(t),
            AgentEvent::TurnEnd { turn, .. } => events.push(format!("turn_end:{turn}")),
            AgentEvent::AgentEnd { messages, .. } => {
                events.push("agent_end".into());
                // Should have: assistant (tool_call) + tool_result + assistant (text) = 3
                assert_eq!(messages.len(), 3);
            }
            _ => {}
        }
    }

    handle.await.unwrap();

    assert!(tool_exec_seen);
    assert!(final_text.contains("hello"));
    assert!(events.contains(&"agent_start".to_string()));
    assert!(events.contains(&"turn_start:1".to_string()));
    assert!(events.contains(&"tool_call:bash".to_string()));
    assert!(events.contains(&"turn_start:2".to_string()));
    assert!(events.contains(&"agent_end".to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_agent_unknown_tool() {
    // Mock calls a tool that doesn't exist
    let provider = Arc::new(MockProvider::new(vec![
        MockResponse::tool_call("call_1", "nonexistent", serde_json::json!({})),
        MockResponse::text("OK, that tool doesn't exist."),
    ]));

    let tools = ToolRegistry::new(); // empty registry
    let agent = Agent::new(AgentConfig::default(), provider, tools);
    let (mut rx, handle) = agent.prompt("Call nonexistent tool").await;

    let mut saw_tool_error = false;
    let mut all_events = Vec::new();

    while let Some(event) = rx.recv().await {
        all_events.push(format!("{:?}", event));
        if let AgentEvent::ToolExecEnd { result, .. } = &event {
            if result.is_error {
                let text = result.content[0].as_text().unwrap();
                assert!(text.contains("Unknown tool"));
                saw_tool_error = true;
            }
        }
    }

    handle.await.unwrap();
    assert!(
        saw_tool_error,
        "Expected tool error, got events: {:#?}",
        all_events
    );
}

#[tokio::test]
async fn test_agent_max_turns() {
    // Provider always returns tool calls — should hit max_turns
    let responses: Vec<MockResponse> = (0..10)
        .map(|i| {
            MockResponse::tool_call(
                format!("call_{i}"),
                "bash",
                serde_json::json!({"command": "echo hi"}),
            )
        })
        .collect();

    let provider = Arc::new(MockProvider::new(responses));
    let dir = std::env::temp_dir().join("subspace_test_max_turns");
    std::fs::create_dir_all(&dir).unwrap();
    let tools = ToolRegistry::coding_defaults(dir.clone());

    let config = AgentConfig {
        max_turns: 3,
        ..Default::default()
    };

    let agent = Agent::new(config, provider, tools);
    let (mut rx, handle) = agent.prompt("Loop forever").await;

    let mut saw_warning = false;
    let mut turn_count = 0;

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::TurnStart { .. } => turn_count += 1,
            AgentEvent::Warning(w) if w.contains("Max turns") => saw_warning = true,
            _ => {}
        }
    }

    handle.await.unwrap();
    assert!(saw_warning);
    assert_eq!(turn_count, 3);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_agent_messages_persisted() {
    let provider = Arc::new(MockProvider::with_text("Response"));
    let tools = ToolRegistry::new();
    let agent = Agent::new(AgentConfig::default(), provider, tools);

    let (mut rx, handle) = agent.prompt("Hello").await;
    while rx.recv().await.is_some() {}
    handle.await.unwrap();

    let messages = agent.messages().await;
    assert_eq!(messages.len(), 2); // user + assistant
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[1].role, Role::Assistant);
}
