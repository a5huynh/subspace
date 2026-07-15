pub mod bash;
pub mod edit;
pub mod read;
pub mod write;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::ContentBlock;

// ---------------------------------------------------------------------------
// Tool definition (sent to the LLM as part of the schema)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Tool result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            is_error: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool trait
// ---------------------------------------------------------------------------

/// Callback for streaming partial output during tool execution.
pub type OnUpdate = dyn Fn(String) + Send + Sync;

#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool definition (name, description, JSON schema for parameters).
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given JSON arguments.
    /// `on_update` is called with partial output for streaming display.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        on_update: &OnUpdate,
    ) -> anyhow::Result<ToolResult>;
}

// ---------------------------------------------------------------------------
// Tool registry
// ---------------------------------------------------------------------------

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let def = tool.definition();
        self.tools.insert(def.name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Create a registry with the default coding tools: read, write, edit, bash.
    pub fn coding_defaults(cwd: PathBuf) -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(read::ReadTool::new(cwd.clone())));
        registry.register(Arc::new(write::WriteTool::new(cwd.clone())));
        registry.register(Arc::new(edit::EditTool::new(cwd.clone())));
        registry.register(Arc::new(bash::BashTool::new(cwd)));
        registry
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
