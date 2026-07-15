pub mod events;

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::provider::{CompletionRequest, Provider, StreamEvent};
use crate::tools::{ToolRegistry, ToolResult};
use crate::types::*;

pub use events::AgentEvent;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are an expert coding assistant. You help users by reading files, executing commands, editing code, and writing new files.

Available tools:
- read: Read file contents
- write: Create or overwrite files
- edit: Make surgical edits to files (find exact text and replace)
- bash: Execute bash commands

Guidelines:
- Use bash for file operations like ls, grep, find
- Use read to examine files before editing
- Use edit for precise changes (old text must match exactly)
- Use write only for new files or complete rewrites
- Be concise in your responses"#;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub system_prompt: String,
    pub model: String,
    pub thinking: ThinkingLevel,
    pub max_tokens: u32,
    pub max_turns: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            thinking: ThinkingLevel::Low,
            max_tokens: 16_384,
            max_turns: 50,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

const EVENT_CHANNEL_SIZE: usize = 512;

/// Shared mutable state for the agent, accessible from spawned tasks.
struct AgentState {
    messages: Vec<Message>,
    cancel: CancellationToken,
}

pub struct Agent {
    config: AgentConfig,
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    state: Arc<Mutex<AgentState>>,
}

impl Agent {
    pub fn new(config: AgentConfig, provider: Arc<dyn Provider>, tools: ToolRegistry) -> Self {
        Self {
            config,
            provider,
            tools: Arc::new(tools),
            state: Arc::new(Mutex::new(AgentState {
                messages: Vec::new(),
                cancel: CancellationToken::new(),
            })),
        }
    }

    /// Send a user prompt and run the agent loop.
    ///
    /// Returns a receiver for streaming `AgentEvent`s and a `JoinHandle`.
    /// Events stream in real-time. The handle resolves when the loop finishes.
    pub async fn prompt(
        &self,
        text: &str,
    ) -> (mpsc::Receiver<AgentEvent>, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_SIZE);

        // Add user message and create fresh cancel token
        let cancel = {
            let mut state = self.state.lock().await;
            state.messages.push(Message::user(text));
            state.cancel = CancellationToken::new();
            state.cancel.clone()
        };

        let config = self.config.clone();
        let provider = self.provider.clone();
        let tools = self.tools.clone();
        let state = self.state.clone();

        let handle = tokio::spawn(async move {
            run_loop(config, provider, tools, state, cancel, tx).await;
        });

        (rx, handle)
    }

    /// Abort the current run.
    pub async fn abort(&self) {
        self.state.lock().await.cancel.cancel();
    }

    /// Access conversation history (takes lock briefly).
    pub async fn messages(&self) -> Vec<Message> {
        self.state.lock().await.messages.clone()
    }

    /// Replace conversation history.
    pub async fn replace_messages(&self, messages: Vec<Message>) {
        self.state.lock().await.messages = messages;
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Core agent loop (free function, runs inside spawned task)
// ---------------------------------------------------------------------------

async fn run_loop(
    config: AgentConfig,
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    state: Arc<Mutex<AgentState>>,
    cancel: CancellationToken,
    tx: mpsc::Sender<AgentEvent>,
) {
    let _ = tx.send(AgentEvent::AgentStart).await;

    let mut turn: u32 = 0;
    let mut total_usage = Usage::default();
    let mut produced_messages: Vec<Message> = Vec::new();

    loop {
        if cancel.is_cancelled() {
            let _ = tx.send(AgentEvent::Warning("Aborted".into())).await;
            break;
        }

        turn += 1;
        if turn > config.max_turns {
            let _ = tx
                .send(AgentEvent::Warning(format!(
                    "Max turns ({}) reached",
                    config.max_turns
                )))
                .await;
            break;
        }

        let _ = tx.send(AgentEvent::TurnStart { turn }).await;

        // Snapshot messages for the request
        let messages = {
            let state = state.lock().await;
            state.messages.clone()
        };

        let request = CompletionRequest {
            model: config.model.clone(),
            messages,
            system_prompt: Some(config.system_prompt.clone()),
            tools: tools.definitions(),
            max_tokens: config.max_tokens,
            thinking: config.thinking,
        };

        // Call LLM
        let stream = match provider.complete(request).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                break;
            }
        };

        // Forward streaming events in real-time
        let mut events_rx = stream.events;
        let mut turn_usage = Usage::default();

        while let Some(event) = events_rx.recv().await {
            if cancel.is_cancelled() {
                break;
            }
            match &event {
                StreamEvent::TextDelta(d) => {
                    let _ = tx.send(AgentEvent::TextDelta(d.clone())).await;
                }
                StreamEvent::ThinkingDelta(d) => {
                    let _ = tx.send(AgentEvent::ThinkingDelta(d.clone())).await;
                }
                StreamEvent::ToolCallEnd {
                    id,
                    name,
                    arguments,
                } => {
                    let _ = tx
                        .send(AgentEvent::ToolCallStart {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        })
                        .await;
                }
                StreamEvent::MessageEnd { usage, .. } => {
                    turn_usage = usage.clone();
                }
                StreamEvent::Error(e) => {
                    let _ = tx.send(AgentEvent::Error(e.clone())).await;
                }
                _ => {}
            }
        }

        // Get assembled message
        let assistant_msg = match stream.handle.await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                let _ = tx.send(AgentEvent::Error(e.to_string())).await;
                break;
            }
            Err(e) => {
                let _ = tx
                    .send(AgentEvent::Error(format!("Provider task panicked: {e}")))
                    .await;
                break;
            }
        };

        total_usage.accumulate(&turn_usage);

        // Store assistant message
        {
            let mut s = state.lock().await;
            s.messages.push(assistant_msg.clone());
        }
        produced_messages.push(assistant_msg.clone());

        let tool_calls = assistant_msg.tool_calls();

        if tool_calls.is_empty() {
            let _ = tx
                .send(AgentEvent::TurnEnd {
                    turn,
                    message: assistant_msg,
                    usage: turn_usage,
                })
                .await;
            break;
        }

        // Execute tools
        let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();

        for tc in &tool_calls {
            if cancel.is_cancelled() {
                break;
            }

            let result = execute_tool(&tools, tc, &tx).await;
            tool_result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tc.id.clone(),
                content: result.content.clone(),
                is_error: result.is_error,
            });
        }

        // Add tool results as user message
        let tool_msg = Message {
            id: new_id(),
            role: Role::User,
            content: tool_result_blocks,
            timestamp: chrono::Utc::now(),
        };

        {
            let mut s = state.lock().await;
            s.messages.push(tool_msg.clone());
        }
        produced_messages.push(tool_msg);

        let _ = tx
            .send(AgentEvent::TurnEnd {
                turn,
                message: assistant_msg,
                usage: turn_usage,
            })
            .await;
    }

    let _ = tx
        .send(AgentEvent::AgentEnd {
            messages: produced_messages,
            total_usage,
        })
        .await;
}

async fn execute_tool(
    tools: &ToolRegistry,
    tc: &ToolCall,
    tx: &mpsc::Sender<AgentEvent>,
) -> ToolResult {
    let tool = match tools.get(&tc.name) {
        Some(t) => t,
        None => {
            let result = ToolResult::error(format!("Unknown tool: {}", tc.name));
            let _ = tx
                .send(AgentEvent::ToolExecEnd {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    result: result.clone(),
                })
                .await;
            return result;
        }
    };

    let tool_id = tc.id.clone();
    let tool_name = tc.name.clone();
    let tx_clone = tx.clone();

    let on_update = move |partial: String| {
        let _ = tx_clone.try_send(AgentEvent::ToolExecUpdate {
            id: tool_id.clone(),
            name: tool_name.clone(),
            partial,
        });
    };

    match tool.execute(tc.arguments.clone(), &on_update).await {
        Ok(result) => {
            let _ = tx
                .send(AgentEvent::ToolExecEnd {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    result: result.clone(),
                })
                .await;
            result
        }
        Err(e) => {
            let result = ToolResult::error(format!("Tool error: {e}"));
            let _ = tx
                .send(AgentEvent::ToolExecEnd {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    result: result.clone(),
                })
                .await;
            result
        }
    }
}
