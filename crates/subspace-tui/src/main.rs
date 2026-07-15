mod tui;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use dotenv::dotenv;

use subspace::agent::{Agent, AgentConfig, AgentEvent};
use subspace::provider::anthropic::AnthropicProvider;
use subspace::tools::ToolRegistry;
use subspace::types::ThinkingLevel;

#[derive(Parser)]
#[command(name = "subspace", version, about = "A Rust agent harness")]
struct Cli {
    /// Initial message to send
    #[arg(trailing_var_arg = true)]
    message: Vec<String>,

    /// Print mode: send prompt, print response, exit
    #[arg(short, long)]
    print: bool,

    /// Model ID
    #[arg(long, default_value = "claude-opus-4-8")]
    model: String,

    /// API key (overrides ANTHROPIC_API_KEY env var)
    #[arg(long, env)]
    anthropic_api_key: String,

    /// Thinking level: off, low, medium, high
    #[arg(long, default_value = "off")]
    thinking: String,

    /// System prompt override
    #[arg(long)]
    system_prompt: Option<String>,

    /// Max agent turns
    #[arg(long, default_value = "50")]
    max_turns: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    let cli = Cli::parse();

    // Build provider
    let provider: Arc<dyn subspace::provider::Provider> =
        Arc::new(AnthropicProvider::new(cli.anthropic_api_key));

    let thinking = match cli.thinking.as_str() {
        "off" => ThinkingLevel::Off,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        other => {
            eprintln!("Unknown thinking level: {other}, using 'off'");
            ThinkingLevel::Off
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut config = AgentConfig {
        model: cli.model,
        thinking,
        max_turns: cli.max_turns,
        ..Default::default()
    };
    if let Some(prompt) = cli.system_prompt {
        config.system_prompt = prompt;
    }

    // Load context files (AGENTS.md / CLAUDE.md) and append to system prompt
    let context_files = subspace::context::load_context_files(&cwd);
    if !context_files.is_empty() {
        eprintln!(
            "Loaded context from: {}",
            context_files
                .iter()
                .map(|f| f.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(ctx) = subspace::context::format_context(&context_files) {
            config.system_prompt.push_str("\n\n");
            config.system_prompt.push_str(&ctx);
        }
    }
    let tools = ToolRegistry::coding_defaults(cwd);
    let agent = Arc::new(Agent::new(config, provider, tools));

    if cli.print {
        // Print mode
        let message = cli.message.join(" ");
        if message.is_empty() {
            eprintln!("Print mode requires a message. Use: subspace -p \"your prompt\"");
            std::process::exit(1);
        }
        run_print_mode(&agent, &message).await
    } else {
        // Interactive TUI mode
        let initial_message = if cli.message.is_empty() {
            None
        } else {
            Some(cli.message.join(" "))
        };
        run_tui_mode(agent, initial_message).await
    }
}

async fn run_print_mode(agent: &Agent, message: &str) -> anyhow::Result<()> {
    let (mut rx, handle) = agent.prompt(message).await;

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::TextDelta(text) => print!("{text}"),
            AgentEvent::ToolCallStart {
                name, arguments, ..
            } => {
                eprintln!("\n[tool: {name}({arguments})]");
            }
            AgentEvent::ToolExecUpdate { partial, .. } => eprint!("{partial}"),
            AgentEvent::ToolExecEnd { name, result, .. } => {
                let status = if result.is_error { "ERROR" } else { "ok" };
                eprintln!("[/{name}: {status}]");
            }
            AgentEvent::TurnEnd { turn, usage, .. } => {
                eprintln!("\n[turn {turn} | tokens: {}]", usage.total());
            }
            AgentEvent::AgentEnd { total_usage, .. } => {
                eprintln!("\n[done | total tokens: {}]", total_usage.total());
            }
            AgentEvent::Warning(w) => eprintln!("[warning: {w}]"),
            AgentEvent::Error(e) => eprintln!("[error: {e}]"),
            _ => {}
        }
    }

    let _ = handle.await;
    println!();
    Ok(())
}

async fn run_tui_mode(agent: Arc<Agent>, initial_message: Option<String>) -> anyhow::Result<()> {
    // If there's an initial message, we'll let the TUI handle it
    // For now, just launch the TUI
    let _ = initial_message; // TODO: send as first prompt in TUI
    crate::tui::run(agent).await
}
