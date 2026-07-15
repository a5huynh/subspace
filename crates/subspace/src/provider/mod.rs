pub mod anthropic;
pub mod mock;

use crate::tools::ToolDefinition;
use crate::types::*;
use async_trait::async_trait;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Stream events from LLM provider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Incremental text output
    TextDelta(String),
    /// Incremental thinking/reasoning output
    ThinkingDelta(String),
    /// A tool call block has started
    ToolCallStart { id: Id, name: String },
    /// Incremental tool call argument JSON
    ToolCallDelta { id: Id, delta: String },
    /// Tool call fully parsed
    ToolCallEnd {
        id: Id,
        name: String,
        arguments: serde_json::Value,
    },
    /// Message generation complete
    MessageEnd {
        stop_reason: StopReason,
        usage: Usage,
    },
    /// Provider error
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Stop,
    ToolUse,
    MaxTokens,
    Error,
}

// ---------------------------------------------------------------------------
// Completion request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub system_prompt: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    pub thinking: ThinkingLevel,
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// Result of a completed LLM call: the assembled message and a stream of events.
pub struct CompletionStream {
    /// Receiver for streaming events as they arrive.
    pub events: mpsc::Receiver<StreamEvent>,
    /// Handle to the background task. Await this to get the final assembled Message.
    pub handle: tokio::task::JoinHandle<anyhow::Result<Message>>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider name (e.g. "anthropic")
    fn name(&self) -> &str;

    /// Start a streaming completion. Returns a CompletionStream with an event
    /// receiver and a join handle that resolves to the final assembled Message.
    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionStream>;
}
