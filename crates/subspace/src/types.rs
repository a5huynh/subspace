use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

pub type Id = String;

pub fn new_id() -> Id {
    Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Content blocks (mirrors Anthropic's content block model)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "image")]
    Image { media_type: String, data: String },

    #[serde(rename = "thinking")]
    Thinking { thinking: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: Id,
        name: String,
        #[serde(rename = "input")]
        arguments: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: Id,
        content: Vec<ContentBlock>,
        #[serde(default)]
        is_error: bool,
    },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    pub fn tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    pub fn tool_result(
        tool_use_id: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: vec![Self::text(text)],
            is_error,
        }
    }

    /// Extract text content, if this is a Text block.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Id,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            role: Role::User,
            content: vec![ContentBlock::text(text)],
            timestamp: Utc::now(),
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            id: new_id(),
            role: Role::Assistant,
            content,
            timestamp: Utc::now(),
        }
    }

    /// Collect all text content from this message.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract tool use blocks as ToolCalls.
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::ToolUse {
                    id,
                    name,
                    arguments,
                } => Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// True if this message contains any tool use blocks.
    pub fn has_tool_calls(&self) -> bool {
        self.content
            .iter()
            .any(|c| matches!(c, ContentBlock::ToolUse { .. }))
    }
}

// ---------------------------------------------------------------------------
// Tool call (convenience struct extracted from ContentBlock::ToolUse)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: Id,
    pub name: String,
    pub arguments: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Token usage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    pub fn accumulate(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }
}

// ---------------------------------------------------------------------------
// Model info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub supports_thinking: bool,
}

// ---------------------------------------------------------------------------
// Thinking level
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    #[default]
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    /// Map to Anthropic's budget_tokens hint.
    /// Returns None for Off (thinking disabled).
    pub fn budget_tokens(&self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Low => Some(2_000),
            Self::Medium => Some(8_000),
            Self::High => Some(32_000),
        }
    }
}
