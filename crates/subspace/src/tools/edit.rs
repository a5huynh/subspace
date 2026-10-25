use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{OnUpdate, Tool, ToolDefinition, ToolResult};

pub struct EditTool {
    cwd: PathBuf,
}

impl EditTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() { p } else { self.cwd.join(p) }
    }
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    #[serde(rename = "oldText")]
    old_text: String,
    #[serde(rename = "newText")]
    new_text: String,
}

#[async_trait]
impl Tool for EditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit".to_string(),
            description: "Edit a file by replacing exact text. The oldText must match exactly (including whitespace). Use this for precise, surgical edits.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative or absolute)"
                    },
                    "oldText": {
                        "type": "string",
                        "description": "Exact text to find and replace (must match exactly)"
                    },
                    "newText": {
                        "type": "string",
                        "description": "New text to replace the old text with"
                    }
                },
                "required": ["path", "oldText", "newText"]
            }),
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _on_update: &OnUpdate,
    ) -> anyhow::Result<ToolResult> {
        let args: EditArgs = serde_json::from_value(arguments)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let path = self.resolve_path(&args.path);

        if !path.exists() {
            return Ok(ToolResult::error(format!("File not found: {}", args.path)));
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", args.path))?;

        // Find exact match
        let match_count = content.matches(&args.old_text).count();

        if match_count == 0 {
            return Ok(ToolResult::error(format!(
                "oldText not found in {}. Make sure it matches exactly (including whitespace).",
                args.path
            )));
        }

        if match_count > 1 {
            return Ok(ToolResult::error(format!(
                "oldText found {match_count} times in {}. Please provide more context to make the match unique.",
                args.path
            )));
        }

        let new_content = content.replacen(&args.old_text, &args.new_text, 1);
        tokio::fs::write(&path, &new_content)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {e}", args.path))?;

        Ok(ToolResult::success(format!(
            "Successfully edited {}",
            args.path
        )))
    }
}
