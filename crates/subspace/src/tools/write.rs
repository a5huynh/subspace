use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{OnUpdate, Tool, ToolDefinition, ToolResult};

pub struct WriteTool {
    cwd: PathBuf,
}

impl WriteTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() { p } else { self.cwd.join(p) }
    }
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".to_string(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _on_update: &OnUpdate,
    ) -> anyhow::Result<ToolResult> {
        let args: WriteArgs = serde_json::from_value(arguments)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let path = self.resolve_path(&args.path);

        // Create parent directories
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                anyhow::anyhow!("Failed to create directories for {}: {e}", args.path)
            })?;
        }

        let bytes = args.content.len();
        tokio::fs::write(&path, &args.content)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {e}", args.path))?;

        Ok(ToolResult::success(format!(
            "Successfully wrote {bytes} bytes to {}",
            args.path
        )))
    }
}
