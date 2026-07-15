use anyhow::{Context as _, bail};
use eventsource_stream::Eventsource as _;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::{CompletionRequest, CompletionStream, Provider, StopReason, StreamEvent};
use crate::types::*;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const EVENT_CHANNEL_SIZE: usize = 256;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY environment variable not set")?;
        Ok(Self::new(key))
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionStream> {
        let body = build_request_body(&request)?;
        let url = format!("{}/v1/messages", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Anthropic")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Anthropic API error {status}: {body}");
        }

        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_SIZE);

        let byte_stream = response.bytes_stream();
        let sse_stream = byte_stream.eventsource();

        let handle = tokio::spawn(async move { process_sse_stream(sse_stream, tx).await });

        Ok(CompletionStream { events: rx, handle })
    }
}

// ---------------------------------------------------------------------------
// Request body construction
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct RequestBody {
    model: String,
    messages: Vec<ApiMessage>,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ApiThinking>,
}

#[derive(Serialize)]
struct ApiThinking {
    #[serde(rename = "type")]
    type_: String,
    budget_tokens: u32,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Serialize)]
struct ApiTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

fn build_request_body(request: &CompletionRequest) -> anyhow::Result<RequestBody> {
    let messages = request
        .messages
        .iter()
        .map(|m| convert_message(m))
        .collect();

    let tools: Vec<ApiTool> = request
        .tools
        .iter()
        .map(|t| ApiTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        })
        .collect();

    let thinking = request.thinking.budget_tokens().map(|budget| ApiThinking {
        type_: "enabled".to_string(),
        budget_tokens: budget,
    });

    Ok(RequestBody {
        model: request.model.clone(),
        messages,
        max_tokens: request.max_tokens,
        stream: true,
        system: request.system_prompt.clone(),
        tools,
        thinking,
    })
}

fn convert_message(msg: &Message) -> ApiMessage {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };

    let content: Vec<serde_json::Value> = msg
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => serde_json::json!({
                "type": "text",
                "text": text,
            }),
            ContentBlock::Image { media_type, data } => serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data,
                }
            }),
            ContentBlock::Thinking { thinking } => serde_json::json!({
                "type": "thinking",
                "thinking": thinking,
            }),
            ContentBlock::ToolUse {
                id,
                name,
                arguments,
            } => serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": arguments,
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let result_content: Vec<serde_json::Value> = content
                    .iter()
                    .map(|c| match c {
                        ContentBlock::Text { text } => serde_json::json!({
                            "type": "text",
                            "text": text,
                        }),
                        _ => serde_json::json!({"type": "text", "text": ""}),
                    })
                    .collect();
                serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": result_content,
                    "is_error": is_error,
                })
            }
        })
        .collect();

    ApiMessage {
        role: role.to_string(),
        content: serde_json::Value::Array(content),
    }
}

// ---------------------------------------------------------------------------
// SSE stream processing
// ---------------------------------------------------------------------------

/// Tracks state while assembling an assistant message from SSE events.
struct MessageAssembler {
    content_blocks: Vec<ContentBlock>,
    /// Per-block accumulators for partial data
    block_states: Vec<BlockState>,
    stop_reason: StopReason,
    usage: Usage,
}

enum BlockState {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        json_accum: String,
    },
}

impl MessageAssembler {
    fn new() -> Self {
        Self {
            content_blocks: Vec::new(),
            block_states: Vec::new(),
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
        }
    }

    fn finalize(self) -> Message {
        Message::assistant(self.content_blocks)
    }
}

#[derive(Deserialize)]
struct SseEvent {
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    delta: Option<serde_json::Value>,
    #[serde(default)]
    content_block: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<serde_json::Value>,
    #[serde(default)]
    usage: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

async fn process_sse_stream<S>(
    mut stream: S,
    tx: mpsc::Sender<StreamEvent>,
) -> anyhow::Result<Message>
where
    S: futures::Stream<
            Item = Result<
                eventsource_stream::Event,
                eventsource_stream::EventStreamError<reqwest::Error>,
            >,
        > + Unpin,
{
    let mut assembler = MessageAssembler::new();

    while let Some(event_result) = stream.next().await {
        let event = match event_result {
            Ok(e) => e,
            Err(err) => {
                let _ = tx
                    .send(StreamEvent::Error(format!("SSE error: {err}")))
                    .await;
                bail!("SSE stream error: {err}");
            }
        };

        // SSE data field
        let data = event.data.trim().to_string();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        let sse: SseEvent = match serde_json::from_str(&data) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match sse.type_.as_str() {
            "content_block_start" => {
                if let Some(block) = &sse.content_block {
                    handle_block_start(&mut assembler, block, &tx).await;
                }
            }
            "content_block_delta" => {
                if let (Some(idx), Some(delta)) = (sse.index, &sse.delta) {
                    handle_block_delta(&mut assembler, idx, delta, &tx).await;
                }
            }
            "content_block_stop" => {
                if let Some(idx) = sse.index {
                    handle_block_stop(&mut assembler, idx, &tx).await;
                }
            }
            "message_delta" => {
                if let Some(delta) = &sse.delta {
                    if let Some(reason) = delta.get("stop_reason").and_then(|r| r.as_str()) {
                        assembler.stop_reason = match reason {
                            "tool_use" => StopReason::ToolUse,
                            "max_tokens" => StopReason::MaxTokens,
                            _ => StopReason::Stop,
                        };
                    }
                }
                if let Some(usage) = &sse.usage {
                    if let Some(out) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                        assembler.usage.output_tokens = out;
                    }
                }
            }
            "message_start" => {
                if let Some(msg) = &sse.message {
                    if let Some(usage) = msg.get("usage") {
                        if let Some(inp) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                            assembler.usage.input_tokens = inp;
                        }
                        if let Some(cr) = usage
                            .get("cache_read_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            assembler.usage.cache_read_tokens = cr;
                        }
                        if let Some(cw) = usage
                            .get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            assembler.usage.cache_write_tokens = cw;
                        }
                    }
                }
            }
            "message_stop" => {
                let _ = tx
                    .send(StreamEvent::MessageEnd {
                        stop_reason: assembler.stop_reason.clone(),
                        usage: assembler.usage.clone(),
                    })
                    .await;
                break;
            }
            "error" => {
                let err_msg = sse
                    .error
                    .as_ref()
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string();
                let _ = tx.send(StreamEvent::Error(err_msg.clone())).await;
                bail!("Anthropic error: {err_msg}");
            }
            _ => {}
        }
    }

    Ok(assembler.finalize())
}

async fn handle_block_start(
    assembler: &mut MessageAssembler,
    block: &serde_json::Value,
    tx: &mpsc::Sender<StreamEvent>,
) {
    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match block_type {
        "text" => {
            assembler.block_states.push(BlockState::Text(String::new()));
        }
        "thinking" => {
            assembler
                .block_states
                .push(BlockState::Thinking(String::new()));
        }
        "tool_use" => {
            let id = block
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = block
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let _ = tx
                .send(StreamEvent::ToolCallStart {
                    id: id.clone(),
                    name: name.clone(),
                })
                .await;
            assembler.block_states.push(BlockState::ToolUse {
                id,
                name,
                json_accum: String::new(),
            });
        }
        _ => {}
    }
}

async fn handle_block_delta(
    assembler: &mut MessageAssembler,
    idx: usize,
    delta: &serde_json::Value,
    tx: &mpsc::Sender<StreamEvent>,
) {
    if idx >= assembler.block_states.len() {
        return;
    }

    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match delta_type {
        "text_delta" => {
            let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if let BlockState::Text(ref mut accum) = assembler.block_states[idx] {
                accum.push_str(text);
            }
            let _ = tx.send(StreamEvent::TextDelta(text.to_string())).await;
        }
        "thinking_delta" => {
            let thinking = delta.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
            if let BlockState::Thinking(ref mut accum) = assembler.block_states[idx] {
                accum.push_str(thinking);
            }
            let _ = tx
                .send(StreamEvent::ThinkingDelta(thinking.to_string()))
                .await;
        }
        "input_json_delta" => {
            let partial = delta
                .get("partial_json")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            if let BlockState::ToolUse {
                ref id,
                ref mut json_accum,
                ..
            } = assembler.block_states[idx]
            {
                json_accum.push_str(partial);
                let _ = tx
                    .send(StreamEvent::ToolCallDelta {
                        id: id.clone(),
                        delta: partial.to_string(),
                    })
                    .await;
            }
        }
        _ => {}
    }
}

async fn handle_block_stop(
    assembler: &mut MessageAssembler,
    idx: usize,
    tx: &mpsc::Sender<StreamEvent>,
) {
    if idx >= assembler.block_states.len() {
        return;
    }

    let state = &assembler.block_states[idx];
    let content_block = match state {
        BlockState::Text(text) => ContentBlock::Text { text: text.clone() },
        BlockState::Thinking(thinking) => ContentBlock::Thinking {
            thinking: thinking.clone(),
        },
        BlockState::ToolUse {
            id,
            name,
            json_accum,
        } => {
            let arguments: serde_json::Value = serde_json::from_str(json_accum)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            let _ = tx
                .send(StreamEvent::ToolCallEnd {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                })
                .await;
            ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                arguments,
            }
        }
    };

    assembler.content_blocks.push(content_block);
}
