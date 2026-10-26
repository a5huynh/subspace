use std::collections::HashMap;
use std::sync::Mutex;

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
    /// Thinking request format per model, learned from the API (see
    /// [`ThinkingStyle`]); models not listed use adaptive thinking.
    thinking_styles: Mutex<HashMap<String, ThinkingStyle>>,
}

/// How a model wants thinking requested. Current models only accept
/// adaptive thinking with an effort level; older ones only accept a token
/// budget. The API says which with a 400, so the provider learns per model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkingStyle {
    /// `thinking: {type: "adaptive"}` + `output_config: {effort}`.
    Adaptive,
    /// `thinking: {type: "enabled", budget_tokens}`.
    Budget,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            thinking_styles: Mutex::new(HashMap::new()),
        }
    }

    fn thinking_style(&self, model: &str) -> ThinkingStyle {
        self.thinking_styles
            .lock()
            .unwrap()
            .get(model)
            .copied()
            .unwrap_or(ThinkingStyle::Adaptive)
    }

    async fn send(
        &self,
        request: &CompletionRequest,
        style: ThinkingStyle,
    ) -> anyhow::Result<reqwest::Response> {
        let body = build_request_body(request, style)?;
        self.client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Anthropic")
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
        let style = self.thinking_style(&request.model);
        let mut response = self.send(&request, style).await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            // Wrong thinking format for this model: remember the other one
            // and retry once.
            let other = (status == reqwest::StatusCode::BAD_REQUEST
                && request.thinking != ThinkingLevel::Off)
                .then(|| other_thinking_style(&body, style))
                .flatten();
            let Some(other) = other else {
                bail!("Anthropic API error {status}: {body}");
            };
            self.thinking_styles
                .lock()
                .unwrap()
                .insert(request.model.clone(), other);
            response = self.send(&request, other).await?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                bail!("Anthropic API error {status}: {body}");
            }
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
    thinking: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<serde_json::Value>,
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

/// The style a 400 error body asks for instead of `current`, if it is a
/// thinking-format complaint.
fn other_thinking_style(error_body: &str, current: ThinkingStyle) -> Option<ThinkingStyle> {
    match current {
        ThinkingStyle::Adaptive if error_body.contains("adaptive thinking is not supported") => {
            Some(ThinkingStyle::Budget)
        }
        ThinkingStyle::Budget
            if error_body.contains("thinking.type.enabled")
                && error_body.contains("not supported") =>
        {
            Some(ThinkingStyle::Adaptive)
        }
        _ => None,
    }
}

fn build_request_body(
    request: &CompletionRequest,
    style: ThinkingStyle,
) -> anyhow::Result<RequestBody> {
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

    let (thinking, output_config) = match (style, request.thinking) {
        (_, ThinkingLevel::Off) => (None, None),
        (ThinkingStyle::Adaptive, level) => (
            Some(serde_json::json!({"type": "adaptive"})),
            level.effort().map(|e| serde_json::json!({"effort": e})),
        ),
        (ThinkingStyle::Budget, level) => (
            // The budget must be below max_tokens (and at least 1024).
            level.budget_tokens().map(|budget| {
                let budget = budget.min(request.max_tokens.saturating_sub(1)).max(1024);
                serde_json::json!({"type": "enabled", "budget_tokens": budget})
            }),
            None,
        ),
    };

    Ok(RequestBody {
        model: request.model.clone(),
        messages,
        max_tokens: request.max_tokens,
        stream: true,
        system: request.system_prompt.clone(),
        tools,
        thinking,
        output_config,
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
        .filter_map(|block| {
            Some(match block {
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
                // The API rejects thinking without its signature; older recorded
                // blocks don't have one, so they are left out.
                ContentBlock::Thinking {
                    signature: None, ..
                } => return None,
                ContentBlock::Thinking {
                    thinking,
                    signature: Some(signature),
                } => serde_json::json!({
                    "type": "thinking",
                    "thinking": thinking,
                    "signature": signature,
                }),
                ContentBlock::RedactedThinking { data } => serde_json::json!({
                    "type": "redacted_thinking",
                    "data": data,
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
    Thinking {
        text: String,
        signature: String,
    },
    RedactedThinking(String),
    /// A block type this provider doesn't use; tracked so indexes stay aligned.
    Other,
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
            assembler.block_states.push(BlockState::Thinking {
                text: String::new(),
                signature: String::new(),
            });
        }
        "redacted_thinking" => {
            let data = block.get("data").and_then(|v| v.as_str()).unwrap_or("");
            assembler
                .block_states
                .push(BlockState::RedactedThinking(data.to_string()));
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
        _ => assembler.block_states.push(BlockState::Other),
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
            if let BlockState::Thinking { ref mut text, .. } = assembler.block_states[idx] {
                text.push_str(thinking);
            }
            let _ = tx
                .send(StreamEvent::ThinkingDelta(thinking.to_string()))
                .await;
        }
        "signature_delta" => {
            let piece = delta
                .get("signature")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            if let BlockState::Thinking {
                ref mut signature, ..
            } = assembler.block_states[idx]
            {
                signature.push_str(piece);
            }
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
        BlockState::Thinking { text, signature } => ContentBlock::Thinking {
            thinking: text.clone(),
            signature: (!signature.is_empty()).then(|| signature.clone()),
        },
        BlockState::RedactedThinking(data) => ContentBlock::RedactedThinking { data: data.clone() },
        BlockState::Other => return,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn request(model: &str, thinking: ThinkingLevel, messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            model: model.into(),
            messages,
            system_prompt: None,
            tools: Vec::new(),
            max_tokens: 16_384,
            thinking,
        }
    }

    fn body_json(request: &CompletionRequest, style: ThinkingStyle) -> serde_json::Value {
        serde_json::to_value(build_request_body(request, style).unwrap()).unwrap()
    }

    #[test]
    fn thinking_is_requested_in_the_models_style() {
        let medium = request("m", ThinkingLevel::Medium, vec![Message::user("hi")]);
        let adaptive = body_json(&medium, ThinkingStyle::Adaptive);
        assert_eq!(
            adaptive["thinking"],
            serde_json::json!({"type": "adaptive"})
        );
        assert_eq!(
            adaptive["output_config"],
            serde_json::json!({"effort": "medium"})
        );

        let budget = body_json(&medium, ThinkingStyle::Budget);
        assert_eq!(
            budget["thinking"],
            serde_json::json!({"type": "enabled", "budget_tokens": 8000})
        );
        assert!(budget.get("output_config").is_none());

        // A budget at or beyond max_tokens is clamped below it.
        let mut high = request("m", ThinkingLevel::High, vec![Message::user("hi")]);
        high.max_tokens = 4096;
        let clamped = body_json(&high, ThinkingStyle::Budget);
        assert_eq!(clamped["thinking"]["budget_tokens"], 4095);

        let off = body_json(
            &request("m", ThinkingLevel::Off, vec![Message::user("hi")]),
            ThinkingStyle::Adaptive,
        );
        assert!(off.get("thinking").is_none() && off.get("output_config").is_none());
    }

    #[test]
    fn thinking_format_errors_name_the_other_style() {
        let adaptive_err = r#"{"error":{"message":"\"thinking.type.enabled\" is not supported for this model. Use \"thinking.type.adaptive\""}}"#;
        let budget_err =
            r#"{"error":{"message":"adaptive thinking is not supported on this model"}}"#;
        assert_eq!(
            other_thinking_style(adaptive_err, ThinkingStyle::Budget),
            Some(ThinkingStyle::Adaptive)
        );
        assert_eq!(
            other_thinking_style(budget_err, ThinkingStyle::Adaptive),
            Some(ThinkingStyle::Budget)
        );
        assert_eq!(
            other_thinking_style(adaptive_err, ThinkingStyle::Adaptive),
            None
        );
        assert_eq!(
            other_thinking_style("max_tokens: too large", ThinkingStyle::Adaptive),
            None
        );
    }

    #[test]
    fn signed_thinking_is_sent_back_and_unsigned_thinking_is_dropped() {
        let msg = Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "old".into(),
                signature: None,
            },
            ContentBlock::Thinking {
                thinking: "new".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::RedactedThinking { data: "enc".into() },
            ContentBlock::text("answer"),
        ]);
        assert_eq!(
            convert_message(&msg).content,
            serde_json::json!([
                {"type": "thinking", "thinking": "new", "signature": "sig"},
                {"type": "redacted_thinking", "data": "enc"},
                {"type": "text", "text": "answer"},
            ])
        );
    }

    /// SSE for one assistant message: signed thinking, an unknown block,
    /// redacted thinking, then text.
    const STREAM: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"SIG\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"something_new\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"ENC\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":3}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    /// A one-model fake API: rejects the thinking style it doesn't accept
    /// with the real API's error, otherwise streams [`STREAM`]. Returns the
    /// base URL and the request bodies it received.
    async fn fake_api(
        accepts: ThinkingStyle,
    ) -> (String, std::sync::Arc<Mutex<Vec<serde_json::Value>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let body = loop {
                    let mut chunk = [0u8; 4096];
                    let n = sock.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len: usize = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= len {
                            break body.to_string();
                        }
                    }
                };
                let json: serde_json::Value = serde_json::from_str(&body).unwrap();
                let style = match json["thinking"]["type"].as_str() {
                    Some("adaptive") => Some(ThinkingStyle::Adaptive),
                    Some("enabled") => Some(ThinkingStyle::Budget),
                    _ => None,
                };
                log.lock().unwrap().push(json);
                let response = match style {
                    Some(s) if s != accepts => {
                        let msg = if s == ThinkingStyle::Budget {
                            r#"{"type":"error","error":{"type":"invalid_request_error","message":"\"thinking.type.enabled\" is not supported for this model. Use \"thinking.type.adaptive\" and \"output_config.effort\" to control thinking behavior."}}"#
                        } else {
                            r#"{"type":"error","error":{"type":"invalid_request_error","message":"adaptive thinking is not supported on this model"}}"#
                        };
                        format!(
                            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{msg}",
                            msg.len()
                        )
                    }
                    _ => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{STREAM}",
                        STREAM.len()
                    ),
                };
                sock.write_all(response.as_bytes()).await.unwrap();
                let _ = sock.shutdown().await;
            }
        });
        (base, seen)
    }

    async fn complete(provider: &AnthropicProvider, req: CompletionRequest) -> Message {
        let mut stream = provider.complete(req).await.unwrap();
        while stream.events.recv().await.is_some() {}
        stream.handle.await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn learns_a_models_thinking_style_from_the_api_and_keeps_signatures() {
        let (base, seen) = fake_api(ThinkingStyle::Budget).await;
        let provider = AnthropicProvider::with_base_url("key", base);
        let req = || request("legacy", ThinkingLevel::Low, vec![Message::user("hi")]);

        // Adaptive first, rejected, then the budget style succeeds.
        let reply = complete(&provider, req()).await;
        let styles = |seen: &Mutex<Vec<serde_json::Value>>| -> Vec<String> {
            seen.lock()
                .unwrap()
                .iter()
                .map(|b| b["thinking"]["type"].as_str().unwrap_or("none").to_string())
                .collect()
        };
        assert_eq!(styles(&seen), ["adaptive", "enabled"]);

        // The reply keeps the signature and the redacted block; the unknown
        // block doesn't shift the text into the wrong slot.
        assert_eq!(
            serde_json::to_value(&reply.content).unwrap(),
            serde_json::to_value(vec![
                ContentBlock::Thinking {
                    thinking: "hmm".into(),
                    signature: Some("SIG".into())
                },
                ContentBlock::RedactedThinking { data: "ENC".into() },
                ContentBlock::text("ok"),
            ])
            .unwrap()
        );

        // The style is remembered for this model: no second rejection.
        complete(&provider, req()).await;
        assert_eq!(styles(&seen), ["adaptive", "enabled", "enabled"]);
    }

    #[tokio::test]
    async fn current_models_get_adaptive_thinking_on_the_first_try() {
        let (base, seen) = fake_api(ThinkingStyle::Adaptive).await;
        let provider = AnthropicProvider::with_base_url("key", base);
        complete(
            &provider,
            request("new", ThinkingLevel::High, vec![Message::user("hi")]),
        )
        .await;
        let bodies = seen.lock().unwrap().clone();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["output_config"]["effort"], "high");
    }
}
