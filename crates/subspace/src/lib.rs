pub mod agent;
pub mod context;
pub mod provider;
pub mod tools;
pub mod types;

pub mod prelude {
    pub use crate::agent::{Agent, AgentConfig, AgentEvent};
    pub use crate::provider::anthropic::AnthropicProvider;
    pub use crate::provider::{CompletionRequest, Provider, StopReason, StreamEvent};
    pub use crate::tools::{Tool, ToolDefinition, ToolRegistry, ToolResult};
    pub use crate::types::*;
}
