# 🛰️ Subspace

A Rust agent harness and orchestration library. Think [pi](https://github.com/mariozechner/pi-coding-agent) but as an embeddable Rust library with first-class support for multi-agent orchestration via a supervisor model.

> ⚠️ **Early stage / experimental** — APIs will change.

## Goals

- **Library-first** — `subspace` is a Rust crate you embed. The server and web console are consumers.
- **Agent loop** — LLM ↔ tool execution loop with streaming events.
- **Multi-agent orchestration** — A supervisor can spawn, coordinate, and manage multiple agents.
- **Provider-agnostic** — Trait-based provider system (Anthropic implemented, others can slot in).

## Architecture

```
┌──────────────────────────────────────────────────┐
│        Web console (React)  │  Your App           │
├──────────────────────────────────────────────────┤
│  subspace-server: REST + WebSocket API            │
│  Worker Manager → Worker 1 … Worker N             │
│  (worker homes, skills, SQLite history)           │
├──────────────────────────────────────────────────┤
│  subspace: agent loop │ tools │ provider (LLM)    │
└──────────────────────────────────────────────────┘
```

## Crate Structure

This is a Cargo workspace with two crates, plus a web console:

| Crate | Description |
|-------|-------------|
| [`subspace`](crates/subspace/) | Core library — types, provider trait, tools, agent loop |
| [`subspace-server`](crates/subspace-server/) | Server: long-lived workers, on-disk homes + skills, REST/WebSocket API (see [SPEC-v2](docs/SPEC-v2.md)) |
| [`console/`](console/) | Web console (React + Vite + TypeScript) for the server |

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
```

### Run the server

```bash
# Serves the API on http://127.0.0.1:7700; data lives in ~/.subspace
cargo run -p subspace-server

# Elsewhere / another port
cargo run -p subspace-server -- --data-dir ./data --bind 127.0.0.1:8080

# Model that routes workspace posts that mention nobody (default: claude-haiku-5-5)
cargo run -p subspace-server -- --router-model claude-sonnet-5-5
```

Then start the web console (proxies `/api` to the server):

```bash
cd console && npm install && npm run dev   # http://localhost:5173
```

The API has no authentication, so keep it on loopback. Each worker's identity lives in
`<data-dir>/workers/<id>/` (`worker.toml`, `ROLE.md`, `skills/`, `memory/`); global skills
go in `<data-dir>/skills/`.

The console's TypeScript API types are generated from the Rust wire types with
[ts-rs](https://github.com/Aleph-Alpha/ts-rs). After changing them, regenerate:

```bash
UPDATE_TS_BINDINGS=1 cargo test -p subspace-server --test ts_bindings
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
- [x] Server: persistent workers, worker homes, skills, home-scoped tools (SPEC-v2 Phase A)
- [x] Server API + web console (SPEC-v2 Phase B)
- [ ] Integrations, triggers, approvals, memory, multi-worker coordination (SPEC-v2 Phases C to F)
- [ ] Additional providers (OpenAI, etc.)

## Design Decisions

- **Event channels, not callbacks** — The agent returns a `tokio::sync::mpsc::Receiver<AgentEvent>`. Idiomatic async Rust, natural backpressure, and easy to compose in multi-agent select loops.
- **Tool cwd isolation** — Each tool instance is bound to a working directory. In multi-agent setups, different agents can have different working directories.
- **Abort via CancellationToken** — Uses `tokio_util::CancellationToken` for clean cancellation of LLM streams and running tools.
- **Errors are non-fatal when possible** — Tool execution errors are fed back to the LLM as error results. Provider errors (network, auth) bubble up as `Result::Err`.

## License

MIT
