# 🛰️ Subspace

A Rust agent harness and orchestration library. Think [pi](https://github.com/mariozechner/pi-coding-agent) but as an embeddable Rust library with first-class support for multi-agent orchestration via a supervisor model.

> ⚠️ **Early stage / experimental** — APIs will change.

## Goals

- **Library-first** — `subspace` is a Rust crate you embed. The CLI/TUI is a thin consumer.
- **Agent loop** — LLM ↔ tool execution loop with streaming events.
- **Multi-agent orchestration** — A supervisor can spawn, coordinate, and manage multiple agents.
- **Provider-agnostic** — Trait-based provider system (Anthropic implemented, others can slot in).

## Architecture

```
┌──────────────────────────────────────────────────┐
│                   Consumers                       │
│   CLI (TUI)  │  RPC Mode (stdio)  │  Your App   │
├──────────────────────────────────────────────────┤
│              ┌──────────────┐                     │
│              │  Supervisor  │  (optional)          │
│              └──────┬───────┘                     │
│          ┌──────────┼──────────┐                  │
│       Agent 1    Agent 2    Agent N               │
├──────────────────────────────────────────────────┤
│   Tool Registry  │  Provider (LLM)  │  Session   │
└──────────────────────────────────────────────────┘
```

## Crate Structure

This is a Cargo workspace with two crates:

| Crate | Description |
|-------|-------------|
| [`subspace`](crates/subspace/) | Core library — types, provider trait, tools, agent loop |
| [`subspace-tui`](crates/subspace-tui/) | CLI/TUI binary — interactive terminal interface |

## Getting Started

### Prerequisites

- Rust (edition 2024)
- An Anthropic API key

### Build & Run

```bash
# Clone the repo
git clone https://github.com/a5huynh/subspace.git
cd subspace

# Set your API key
export ANTHROPIC_API_KEY="sk-ant-..."
# or create a .env file
echo 'ANTHROPIC_API_KEY=sk-ant-...' > .env

# Build
cargo build --release

# Run interactive TUI
cargo run -p subspace-tui

# Single-shot print mode
cargo run -p subspace-tui -- -p "Explain this project"
```

### CLI Options

```
subspace [OPTIONS] [MESSAGE...]

Arguments:
  [MESSAGE...]  Initial message to send

Options:
  -p, --print              Print mode: send prompt, print response, exit
      --model <MODEL>      Model ID [default: claude-sonnet-4-6]
      --thinking <LEVEL>   Thinking level: off, low, medium, high
  -h, --help               Print help
  -V, --version            Print version
```

## Library Usage

The core library can be embedded in any Rust application:

```rust
use subspace::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = Arc::new(AnthropicProvider::from_env()?);
    let tools = ToolRegistry::coding_defaults(".".into());

    let mut agent = Agent::new(
        AgentConfig::default(),
        provider,
        tools,
    );

    let mut rx = agent.prompt("Hello, what files are in this directory?").await?;

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::TextDelta(text) => print!("{text}"),
            AgentEvent::AgentEnd { .. } => break,
            _ => {}
        }
    }

    Ok(())
}
```

## Built-in Tools

| Tool | Description |
|------|-------------|
| **read** | Read file contents (text + images), with offset/limit support |
| **write** | Create or overwrite files, auto-creates parent directories |
| **edit** | Surgical find-and-replace text editing |
| **bash** | Execute shell commands with streaming output and timeout |

## Roadmap

- [x] Core types & Anthropic provider
- [x] Tool system (read, write, edit, bash)
- [x] Agent loop with streaming events
- [x] Minimal TUI for interactive testing
- [ ] Supervisor / multi-agent orchestration
- [ ] RPC mode (JSONL over stdio)
- [ ] Session persistence (JSONL file-backed)
- [ ] Additional providers (OpenAI, etc.)

## Design Decisions

- **Event channels, not callbacks** — The agent returns a `tokio::sync::mpsc::Receiver<AgentEvent>`. Idiomatic async Rust, natural backpressure, and easy to compose in multi-agent select loops.
- **Tool cwd isolation** — Each tool instance is bound to a working directory. In multi-agent setups, different agents can have different working directories.
- **Abort via CancellationToken** — Uses `tokio_util::CancellationToken` for clean cancellation of LLM streams and running tools.
- **Errors are non-fatal when possible** — Tool execution errors are fed back to the LLM as error results. Provider errors (network, auth) bubble up as `Result::Err`.

## License

MIT
