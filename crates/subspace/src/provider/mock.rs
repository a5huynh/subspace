//! Mock provider for testing. Returns pre-configured responses.

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::{CompletionRequest, CompletionStream, Provider, StopReason, StreamEvent};
use crate::types::*;

/// A pre-configured response the mock provider will return.
#[derive(Clone)]
pub struct MockResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
}

impl MockResponse {
    /// Simple text response (no tool calls).
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(s)],
            stop_reason: StopReason::Stop,
        }
    }

    /// Response with a single tool call.
    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            content: vec![ContentBlock::tool_use(id, name, arguments)],
            stop_reason: StopReason::ToolUse,
        }
    }

    /// Response with text + tool call.
    pub fn text_and_tool(
        text: impl Into<String>,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            content: vec![
                ContentBlock::text(text),
                ContentBlock::tool_use(id, name, arguments),
            ],
            stop_reason: StopReason::ToolUse,
        }
    }
}

/// Mock provider that returns responses from a queue.
/// Each call to `complete()` pops the next response. If the queue is empty, returns a default text response.
pub struct MockProvider {
    responses: std::sync::Mutex<Vec<MockResponse>>,
}

impl MockProvider {
    pub fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }

    /// Single text response.
    pub fn with_text(s: impl Into<String>) -> Self {
        Self::new(vec![MockResponse::text(s)])
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn complete(&self, _request: CompletionRequest) -> anyhow::Result<CompletionStream> {
        let response = {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                MockResponse::text("(no more mock responses)")
            } else {
                responses.remove(0)
            }
        };

        let (tx, rx) = mpsc::channel(64);
        let content = response.content.clone();
        let stop_reason = response.stop_reason.clone();

        let handle = tokio::spawn(async move {
            // Simulate streaming: emit text deltas and tool call events
            for block in &content {
                match block {
                    ContentBlock::Text { text } => {
                        let _ = tx.send(StreamEvent::TextDelta(text.clone())).await;
                    }
                    ContentBlock::ToolUse {
                        id,
                        name,
                        arguments,
                    } => {
                        let _ = tx
                            .send(StreamEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                            })
                            .await;
                        let _ = tx
                            .send(StreamEvent::ToolCallEnd {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            })
                            .await;
                    }
                    _ => {}
                }
            }

            let _ = tx
                .send(StreamEvent::MessageEnd {
                    stop_reason,
                    usage: Usage::default(),
                })
                .await;

            Ok(Message::assistant(content))
        });

        Ok(CompletionStream { events: rx, handle })
    }
}
