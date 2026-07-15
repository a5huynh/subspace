use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use super::{OnUpdate, Tool, ToolDefinition, ToolResult};

const MAX_OUTPUT_LINES: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub struct BashTool {
    cwd: PathBuf,
}

impl BashTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
}

#[async_trait]
impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first).".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Bash command to execute"
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Timeout in seconds (default: 120)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        on_update: &OnUpdate,
    ) -> anyhow::Result<ToolResult> {
        let args: BashArgs = serde_json::from_value(arguments)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        let timeout_secs = args.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);

        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&args.command)
            .current_dir(&self.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn bash: {e}"))?;

        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();

        // Read stdout and stderr concurrently with timeout
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let (out_result, err_result) = tokio::join!(
                read_stream_with_updates(&mut stdout, &mut out_buf, on_update),
                async { stderr.read_to_end(&mut err_buf).await },
            );
            out_result?;
            err_result?;
            child.wait().await
        })
        .await;

        let exit_code = match result {
            Ok(Ok(status)) => status.code().unwrap_or(-1),
            Ok(Err(e)) => {
                return Ok(ToolResult::error(format!("Process error: {e}")));
            }
            Err(_) => {
                // Timeout — kill the process
                let _ = child.kill().await;
                return Ok(ToolResult::error(format!(
                    "Command timed out after {timeout_secs}s"
                )));
            }
        };

        // Combine stdout and stderr
        let stdout_str = String::from_utf8_lossy(&out_buf);
        let stderr_str = String::from_utf8_lossy(&err_buf);

        let mut output = String::new();
        if !stdout_str.is_empty() {
            output.push_str(&stdout_str);
        }
        if !stderr_str.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&stderr_str);
        }

        // Truncate if needed
        let truncated = truncate_output(&mut output);

        if truncated {
            output.push_str("\n\n[Output truncated. Full output saved to temp file.]");
        }

        if exit_code != 0 {
            output.push_str(&format!("\n\nExit code: {exit_code}"));
        }

        if exit_code != 0 {
            Ok(ToolResult::error(output))
        } else {
            Ok(ToolResult::success(output))
        }
    }
}

/// Read from a stream, calling on_update periodically.
async fn read_stream_with_updates(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    buf: &mut Vec<u8>,
    on_update: &OnUpdate,
) -> std::io::Result<()> {
    let mut chunk = [0u8; 4096];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);

        // Send partial output for streaming display
        let partial = String::from_utf8_lossy(&chunk[..n]).to_string();
        on_update(partial);
    }
    Ok(())
}

/// Truncate output to MAX_OUTPUT_LINES / MAX_OUTPUT_BYTES. Returns true if truncated.
fn truncate_output(output: &mut String) -> bool {
    let byte_len = output.len();
    let line_count = output.lines().count();

    if byte_len <= MAX_OUTPUT_BYTES && line_count <= MAX_OUTPUT_LINES {
        return false;
    }

    // Keep the last N lines / bytes
    let lines: Vec<&str> = output.lines().collect();
    let keep_lines = lines.len().min(MAX_OUTPUT_LINES);
    let start = lines.len() - keep_lines;

    let mut result = String::new();
    let mut bytes = 0;

    for line in lines[start..].iter().rev() {
        if bytes + line.len() + 1 > MAX_OUTPUT_BYTES {
            break;
        }
        bytes += line.len() + 1;
        if result.is_empty() {
            result = line.to_string();
        } else {
            result = format!("{line}\n{result}");
        }
    }

    *output = result;
    true
}
