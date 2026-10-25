use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{OnUpdate, Tool, ToolDefinition, ToolResult};

const MAX_LINES: usize = 2_000;
const MAX_BYTES: usize = 50 * 1024;

pub struct ReadTool {
    cwd: PathBuf,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() { p } else { self.cwd.join(p) }
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".to_string(),
            description: "Read the contents of a file. Supports text files and images (jpg, png, gif, webp). Output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative or absolute)"
                    },
                    "offset": {
                        "type": "number",
                        "description": "Line number to start reading from (1-indexed)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to read"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _on_update: &OnUpdate,
    ) -> anyhow::Result<ToolResult> {
        let args: ReadArgs = serde_json::from_value(arguments)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let path = self.resolve_path(&args.path);

        if !path.exists() {
            return Ok(ToolResult::error(format!("File not found: {}", args.path)));
        }

        // Check if image
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") {
            let data = tokio::fs::read(&path).await?;
            let b64 = base64_encode(&data);
            let media_type = match ext.as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "application/octet-stream",
            };
            return Ok(ToolResult {
                content: vec![crate::types::ContentBlock::Image {
                    media_type: media_type.to_string(),
                    data: b64,
                }],
                is_error: false,
            });
        }

        // Text file
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", args.path))?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        // Apply offset (1-indexed)
        let start = args.offset.unwrap_or(1).saturating_sub(1);
        let limit = args.limit.unwrap_or(MAX_LINES);

        let mut output = String::new();
        let mut line_count = 0;
        let mut byte_count = 0;
        let mut truncated = false;

        for (i, line) in lines.iter().enumerate().skip(start) {
            if line_count >= limit || line_count >= MAX_LINES {
                truncated = true;
                break;
            }
            if byte_count + line.len() + 1 > MAX_BYTES {
                truncated = true;
                break;
            }

            if !output.is_empty() {
                output.push('\n');
                byte_count += 1;
            }

            // Prepend line number for context
            let numbered = format!("{:>4} | {}", i + 1, line);
            byte_count += numbered.len();
            output.push_str(&numbered);
            line_count += 1;
        }

        if truncated {
            output.push_str(&format!(
                "\n\n[Truncated. File has {total_lines} lines. Use offset/limit to read more.]"
            ));
        }

        Ok(ToolResult::success(output))
    }
}

fn base64_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        let _ = write!(
            result,
            "{}",
            CHARS[((triple >> 18) & 0x3F) as usize] as char
        );
        let _ = write!(
            result,
            "{}",
            CHARS[((triple >> 12) & 0x3F) as usize] as char
        );
        if chunk.len() > 1 {
            let _ = write!(result, "{}", CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            let _ = write!(result, "{}", CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
