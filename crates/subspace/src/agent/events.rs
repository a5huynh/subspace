use crate::tools::ToolResult;
use crate::types::*;

/// Events emitted by the agent during a run.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Agent started processing a prompt.
    AgentStart,

    /// New LLM turn starting (one turn = LLM response + tool executions).
    TurnStart { turn: u32 },

    /// Streaming text from LLM.
    TextDelta(String),

    /// Streaming thinking/reasoning from LLM.
    ThinkingDelta(String),

    /// LLM requested a tool call.
    ToolCallStart {
        id: Id,
        name: String,
        arguments: serde_json::Value,
    },

    /// Streaming partial output from tool execution.
    ToolExecUpdate {
        id: Id,
        name: String,
        partial: String,
    },

    /// Tool execution completed.
    ToolExecEnd {
        id: Id,
        name: String,
        result: ToolResult,
    },

    /// LLM turn complete.
    TurnEnd {
        turn: u32,
        message: Message,
        usage: Usage,
    },

    /// Agent finished — no more tool calls.
    AgentEnd {
        /// All messages produced during this run (assistant + tool results).
        messages: Vec<Message>,
        /// Accumulated usage across all turns.
        total_usage: Usage,
    },

    /// Non-fatal warning (e.g. max turns reached).
    Warning(String),

    /// Fatal error.
    Error(String),
}
