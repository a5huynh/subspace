//! Live checks against the real Anthropic API. Ignored by default; run with
//! `cargo test -p subspace --test live_anthropic -- --ignored` and
//! `ANTHROPIC_API_KEY` set (a `.env` at the repo root also works).

use std::sync::Arc;

use subspace::agent::{Agent, AgentConfig, AgentEvent};
use subspace::provider::anthropic::AnthropicProvider;
use subspace::tools::ToolRegistry;
use subspace::types::{ContentBlock, ThinkingLevel};

fn provider() -> Arc<AnthropicProvider> {
    let _ = dotenv_from_repo_root();
    Arc::new(AnthropicProvider::from_env().expect("ANTHROPIC_API_KEY"))
}

/// Minimal `.env` reader so the test needs no extra dependency.
fn dotenv_from_repo_root() -> Option<()> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    for line in std::fs::read_to_string(path).ok()?.lines() {
        if let Some((k, v)) = line.split_once('=')
            && std::env::var(k.trim()).is_err()
        {
            // SAFETY: tests in this file set env vars before any threads read them.
            unsafe { std::env::set_var(k.trim(), v.trim().trim_matches('"')) };
        }
    }
    Some(())
}

/// Run one prompt; fail on any error event. Returns the run's final text and
/// whether it called a tool.
async fn turn(agent: &Agent, text: &str) -> (String, bool) {
    let (mut rx, handle) = agent.prompt(text).await;
    let mut used_tool = false;
    let mut reply = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::Error(e) => panic!("{text:?} failed: {e}"),
            AgentEvent::ToolCallStart { .. } => used_tool = true,
            AgentEvent::AgentEnd { messages, .. } => {
                reply = messages.last().map(|m| m.text()).unwrap_or_default();
            }
            _ => {}
        }
    }
    handle.await.unwrap();
    (reply, used_tool)
}

/// Thinking + a tool call, then a second turn: the second request must send
/// the first run's thinking back with its signature.
async fn multi_turn_with_thinking(model: &str, thinking: ThinkingLevel) {
    let dir = std::env::temp_dir().join(format!("subspace_live_{}", subspace::types::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("secret.txt"), "the code word is aubergine").unwrap();

    let config = AgentConfig {
        model: model.into(),
        thinking,
        max_tokens: 4096,
        ..AgentConfig::default()
    };
    let agent = Agent::new(
        config,
        provider(),
        ToolRegistry::coding_defaults(dir.clone()),
    );

    let (first, used_tool) = turn(&agent, "Read secret.txt and tell me the code word.").await;
    assert!(used_tool, "{model}: expected a tool call");
    assert!(
        first.to_lowercase().contains("aubergine"),
        "{model}: {first}"
    );

    let (second, _) = turn(&agent, "Now spell that word backwards, nothing else.").await;
    assert!(
        second.to_lowercase().contains("enigrebua"),
        "{model}: {second}"
    );

    let thought = agent.messages().await.iter().any(|m| {
        m.content.iter().any(|b| {
            matches!(
                b,
                ContentBlock::Thinking {
                    signature: Some(_),
                    ..
                }
            )
        })
    });
    println!("{model}: signed thinking recorded: {thought}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "calls the real Anthropic API"]
async fn adaptive_thinking_model() {
    multi_turn_with_thinking("claude-opus-5-5", ThinkingLevel::High).await;
}

#[tokio::test]
#[ignore = "calls the real Anthropic API"]
async fn budget_thinking_model() {
    multi_turn_with_thinking("claude-sonnet-4-5-20250929", ThinkingLevel::High).await;
}
