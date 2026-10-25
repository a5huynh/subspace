# Subspace — Spec

A Rust agent harness and orchestration library. Think `pi` but as an embeddable Rust library with first-class support for multi-agent orchestration via a supervisor model.

---

## Goals

1. **Library-first**: `subspace` is a Rust crate you embed. The CLI/TUI is a thin consumer.
2. **Agent loop**: LLM ↔ tool execution loop with streaming events.
3. **Multi-agent orchestration**: A supervisor can spawn, coordinate, and manage multiple agents.
4. **Provider-agnostic**: Start with Anthropic, trait-based so others slot in.
5. **RPC mode**: Headless JSONL-over-stdio for cross-language integration.
6. **Minimal TUI**: Good enough for interactive testing, not a product goal.

## Non-Goals (for now)

- Extensions/plugins system (TypeScript or otherwise)
- Skills / prompt templates / themes
- Session branching/tree (flat log is fine initially)
- OAuth / subscription auth flows
- Context file discovery (AGENTS.md walking)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                     Consumers                        │
│  ┌─────────┐   ┌──────────┐   ┌──────────────────┐ │
│  │   CLI   │   │ RPC Mode │   │ Your Rust App    │ │
│  │  (TUI)  │   │ (stdio)  │   │ (SDK embedding)  │ │
│  └────┬────┘   └────┬─────┘   └────────┬─────────┘ │
│       │              │                   │           │
│       └──────────────┴───────────────────┘           │
│                      │                               │
├──────────────────────┼───────────────────────────────┤
│              ┌───────┴────────┐                      │
│              │   Supervisor   │  (optional layer)    │
│              └───────┬────────┘                      │
│          ┌───────────┼───────────┐                   │
│          ▼           ▼           ▼                   │
│     ┌─────────┐ ┌─────────┐ ┌─────────┐            │
│     │ Agent 1 │ │ Agent 2 │ │ Agent N │            │
│     └────┬────┘ └────┬────┘ └────┬────┘            │
│          │           │           │                   │
├──────────┼───────────┼───────────┼───────────────────┤
│          ▼           ▼           ▼                   │
│   ┌────────────────────────────────────┐            │
│   │           Tool Registry            │            │
│   │  read │ write │ edit │ bash │ ...  │            │
│   └────────────────────────────────────┘            │
│                                                      │
│   ┌────────────────────────────────────┐            │
│   │         Provider (LLM API)         │            │
│   │  Anthropic │ OpenAI │ Custom       │            │
│   └────────────────────────────────────┘            │
│                                                      │
│   ┌────────────────────────────────────┐            │
│   │         Session Storage            │            │
│   │  InMemory │ JSONL File             │            │
│   └────────────────────────────────────┘            │
└─────────────────────────────────────────────────────┘
```

---

## Crate Structure

Single crate with feature flags. Can be split later if needed.

```
subspace/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Public API re-exports
│   ├── types.rs            # Core types: Message, ContentBlock, Usage, etc.
│   │
│   ├── provider/
│   │   ├── mod.rs          # Provider trait + StreamEvent + CompletionRequest
│   │   └── anthropic.rs    # Anthropic Messages API implementation
│   │
│   ├── tools/
│   │   ├── mod.rs          # Tool trait + ToolRegistry
│   │   ├── read.rs         # Read file contents
│   │   ├── write.rs        # Write file contents
│   │   ├── edit.rs         # Find-and-replace edit
│   │   └── bash.rs         # Execute shell commands
│   │
│   ├── agent/
│   │   ├── mod.rs          # Agent struct — the core loop
│   │   └── events.rs       # AgentEvent enum (superset of StreamEvent)
│   │
│   ├── supervisor/
│   │   ├── mod.rs          # Supervisor struct — multi-agent orchestration
│   │   └── strategy.rs     # Orchestration strategies
│   │
│   ├── session/
│   │   ├── mod.rs          # Session trait
│   │   ├── memory.rs       # InMemory session
│   │   └── jsonl.rs        # JSONL file-backed session
│   │
│   ├── rpc/
│   │   ├── mod.rs          # RPC mode: JSONL over stdin/stdout
│   │   ├── commands.rs     # RPC command types
│   │   └── events.rs       # RPC event serialization
│   │
│   └── main.rs             # CLI entrypoint (TUI + print + rpc modes)
```

---

## Phase 1: Core Types & Provider

### `types.rs`

```rust
/// Unique ID (UUID v4 string)
type Id = String;

/// Content within a message
enum ContentBlock {
    Text { text: String },
    Image { media_type: String, data: String },
    Thinking { thinking: String },
    ToolUse { id: Id, name: String, arguments: serde_json::Value },
    ToolResult { tool_use_id: Id, content: Vec<ContentBlock>, is_error: bool },
}

enum Role { User, Assistant }

struct Message {
    id: Id,
    role: Role,
    content: Vec<ContentBlock>,
    timestamp: DateTime<Utc>,
}

struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
}

enum ThinkingLevel { Off, Low, Medium, High }

struct ModelInfo {
    id: String,
    name: String,
    provider: String,
    context_window: u64,
    max_output_tokens: u64,
    supports_thinking: bool,
}
```

Convenience constructors on `Message` and `ContentBlock` (e.g., `Message::user("hello")`, `ContentBlock::text("hi")`).

### `provider/mod.rs`

```rust
/// Streaming events emitted during completion
enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallStart { id: Id, name: String },
    ToolCallDelta { id: Id, delta: String },
    ToolCallEnd { id: Id, name: String, arguments: Value },
    MessageEnd { stop_reason: StopReason, usage: Usage },
    Error(String),
}

enum StopReason { Stop, ToolUse, MaxTokens, Error }

struct CompletionRequest {
    model: String,
    messages: Vec<Message>,
    system_prompt: Option<String>,
    tools: Vec<ToolDefinition>,
    max_tokens: u32,
    thinking: ThinkingLevel,
}

#[async_trait]
trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Stream a completion. Returns a receiver for streaming events
    /// and a join handle that resolves to the fully-assembled assistant Message.
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<(mpsc::Receiver<StreamEvent>, JoinHandle<Result<Message>>)>;
}
```

### `provider/anthropic.rs`

- Implements `Provider` for Anthropic Messages API (`/v1/messages`)
- SSE streaming via `reqwest` + `eventsource-stream`
- Maps Anthropic's SSE events to `StreamEvent`
- Handles tool definitions → Anthropic tool schema format
- API key from `ANTHROPIC_API_KEY` env var (or passed in at construction)

```rust
struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String, // default: https://api.anthropic.com
}

impl AnthropicProvider {
    fn new(api_key: String) -> Self;
    fn with_base_url(api_key: String, base_url: String) -> Self;
}
```

---

## Phase 2: Tools

### `tools/mod.rs`

```rust
/// Static definition of a tool (sent to the LLM)
struct ToolDefinition {
    name: String,
    description: String,
    parameters: Value, // JSON Schema object
}

/// Result of executing a tool
struct ToolResult {
    content: Vec<ContentBlock>,
    is_error: bool,
}

/// A tool that can be executed
#[async_trait]
trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    async fn execute(
        &self,
        arguments: Value,
        on_update: &(dyn Fn(String) + Send + Sync),  // streaming output
    ) -> Result<ToolResult>;
}

/// Registry of available tools
struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    fn new() -> Self;
    fn register(&mut self, tool: Arc<dyn Tool>);
    fn get(&self, name: &str) -> Option<&Arc<dyn Tool>>;
    fn definitions(&self) -> Vec<ToolDefinition>;

    /// Create registry with the default coding tools (read, write, edit, bash)
    fn coding_defaults(cwd: PathBuf) -> Self;
}
```

### Tool Implementations

All tools take a `cwd: PathBuf` at construction for path resolution.

**`tools/read.rs`** — Read file contents
- Params: `{ path: String, offset?: u64, limit?: u64 }`
- Supports text files and images (returns base64 for images)
- Truncation at 2000 lines or 50KB

**`tools/write.rs`** — Write/create files
- Params: `{ path: String, content: String }`
- Creates parent directories
- Returns confirmation

**`tools/edit.rs`** — Surgical find-and-replace
- Params: `{ path: String, old_text: String, new_text: String }`
- Exact match required
- Returns confirmation or error if not found

**`tools/bash.rs`** — Execute shell commands
- Params: `{ command: String, timeout?: u64 }`
- Streams stdout/stderr via `on_update`
- Truncation at 2000 lines or 50KB, saves full output to temp file
- Respects `cwd`
- Returns `{ output, exit_code, truncated }`

---

## Phase 3: Agent (Core Loop)

### `agent/events.rs`

```rust
/// Events emitted by the agent during a run
enum AgentEvent {
    /// Agent started processing a prompt
    AgentStart,

    /// New LLM turn starting
    TurnStart,

    /// Streaming text from LLM
    TextDelta(String),

    /// Streaming thinking from LLM
    ThinkingDelta(String),

    /// Tool call identified
    ToolCallStart { id: Id, name: String, arguments: Value },

    /// Streaming tool execution output
    ToolExecUpdate { id: Id, name: String, partial: String },

    /// Tool execution complete
    ToolExecEnd { id: Id, name: String, result: ToolResult, is_error: bool },

    /// LLM turn complete
    TurnEnd { message: Message, tool_results: Vec<Message> },

    /// Agent finished (no more tool calls)
    AgentEnd { messages: Vec<Message> },

    /// Error (non-fatal, e.g. tool failure fed back to LLM)
    Warning(String),

    /// Fatal error
    Error(String),
}
```

### `agent/mod.rs`

```rust
struct AgentConfig {
    /// System prompt
    system_prompt: String,
    /// Model ID to use
    model: String,
    /// Thinking level
    thinking: ThinkingLevel,
    /// Max output tokens per turn
    max_tokens: u32,
    /// Max consecutive tool-use turns before forcing a stop
    max_turns: u32,
}

impl Default for AgentConfig {
    // system_prompt: pi-style default prompt
    // model: "claude-opus-4-8"
    // thinking: ThinkingLevel::Low
    // max_tokens: 16384
    // max_turns: 50
}

struct Agent {
    config: AgentConfig,
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    messages: Vec<Message>,
}

impl Agent {
    fn new(
        config: AgentConfig,
        provider: Arc<dyn Provider>,
        tools: ToolRegistry,
    ) -> Self;

    /// Send a user prompt and run the agent loop until the LLM stops.
    /// Returns a receiver that streams AgentEvents.
    /// The agent loop runs in a spawned task.
    async fn prompt(
        &mut self,
        text: &str,
    ) -> Result<mpsc::Receiver<AgentEvent>>;

    /// Abort the current run (cancellation token)
    fn abort(&self);

    /// Access conversation history
    fn messages(&self) -> &[Message];

    /// Replace messages (for branching/compaction)
    fn replace_messages(&mut self, messages: Vec<Message>);

    /// Current model
    fn model(&self) -> &str;

    /// Change model
    fn set_model(&mut self, model: String);
}
```

**Agent loop logic** (runs in a spawned task, sends events to the returned `mpsc::Receiver`):

```
1. Append user Message to self.messages
2. Loop:
   a. Send CompletionRequest to provider (messages + tools + system prompt)
   b. Forward StreamEvents from provider as AgentEvents (TextDelta, ThinkingDelta, etc.)
   c. Await assembled assistant Message from provider's JoinHandle
   d. Append assistant Message to self.messages
   e. Extract tool calls from assistant message
   f. If no tool calls → send AgentEnd, break
   g. For each tool call:
      - Send ToolCallStart
      - Look up tool in registry
      - Execute tool, forwarding updates as ToolExecUpdate
      - Send ToolExecEnd
      - Build ToolResult content block
   h. Append tool results as a user message to self.messages
   i. Send TurnEnd
   j. If turn count >= max_turns → break with warning
   k. Continue loop (back to step a)
```

---

## Phase 4: Supervisor (Multi-Agent Orchestration)

The supervisor manages multiple agents and routes messages between them.

### `supervisor/strategy.rs`

```rust
/// Defines how agents are orchestrated
#[async_trait]
trait Strategy: Send + Sync {
    /// Called when the supervisor receives a prompt.
    /// Returns a plan: which agents to invoke, in what order, with what input.
    async fn plan(
        &self,
        prompt: &str,
        agents: &[AgentHandle],
        context: &SupervisorContext,
    ) -> Result<Vec<Step>>;

    /// Called after a step completes. Decides what to do next.
    async fn next(
        &self,
        completed: &StepResult,
        remaining: &[Step],
        context: &SupervisorContext,
    ) -> Result<Decision>;
}

enum Step {
    /// Send prompt to a specific agent
    Prompt { agent_id: String, message: String },
    /// Send prompts to multiple agents in parallel
    Parallel(Vec<Step>),
}

enum Decision {
    /// Continue with remaining steps
    Continue,
    /// Replace remaining steps with new ones
    Redirect(Vec<Step>),
    /// Done
    Done,
}

struct StepResult {
    agent_id: String,
    messages: Vec<Message>,  // Messages produced during this step
}
```

### Built-in Strategies

**`LlmStrategy`** — The supervisor itself is backed by an LLM that decides how to delegate. It receives the user prompt + descriptions of available agents and produces a plan. After each step it can re-evaluate.

**`PipelineStrategy`** — Simple sequential: Agent A → Agent B → Agent C. Output of each is input to the next.

**`MapReduceStrategy`** — Fan out the same prompt to N agents, then fan in results to a reducer agent.

### `supervisor/mod.rs`

```rust
struct AgentHandle {
    id: String,
    description: String,
    agent: Agent,
}

struct SupervisorConfig {
    strategy: Arc<dyn Strategy>,
    /// Max total steps across all agents
    max_steps: u32,
}

struct Supervisor {
    config: SupervisorConfig,
    agents: Vec<AgentHandle>,
}

/// Events emitted by the supervisor
enum SupervisorEvent {
    /// Supervisor started
    Started,
    /// A step is being dispatched
    StepStart { agent_id: String, message: String },
    /// Forwarded agent event (wrapped with agent_id)
    AgentEvent { agent_id: String, event: AgentEvent },
    /// A step completed
    StepEnd { result: StepResult },
    /// Supervisor finished
    Done { results: Vec<StepResult> },
    /// Error
    Error(String),
}

impl Supervisor {
    fn new(config: SupervisorConfig) -> Self;

    fn add_agent(&mut self, handle: AgentHandle);

    /// Run the supervisor with a user prompt.
    /// Returns a receiver that streams SupervisorEvents.
    async fn run(
        &mut self,
        prompt: &str,
    ) -> Result<mpsc::Receiver<SupervisorEvent>>;

    fn abort(&self);
}
```

### SDK Usage Example (Conceptual)

```rust
use subspace::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let provider = Arc::new(AnthropicProvider::from_env()?);
    let tools = ToolRegistry::coding_defaults(".".into());

    // Create specialized agents
    let planner = Agent::new(
        AgentConfig {
            system_prompt: "You are a planning agent. Break tasks into steps.".into(),
            ..Default::default()
        },
        provider.clone(),
        ToolRegistry::new(), // planner has no tools, just reasons
    );

    let coder = Agent::new(
        AgentConfig {
            system_prompt: "You are a coding agent. Implement changes.".into(),
            ..Default::default()
        },
        provider.clone(),
        tools,
    );

    let reviewer = Agent::new(
        AgentConfig {
            system_prompt: "You review code for bugs and issues.".into(),
            ..Default::default()
        },
        provider.clone(),
        ToolRegistry::coding_defaults(".".into()),
    );

    // Build supervisor
    let mut supervisor = Supervisor::new(SupervisorConfig {
        strategy: Arc::new(PipelineStrategy),
        max_steps: 20,
    });

    supervisor.add_agent(AgentHandle {
        id: "planner".into(),
        description: "Breaks tasks into implementation steps".into(),
        agent: planner,
    });
    supervisor.add_agent(AgentHandle {
        id: "coder".into(),
        description: "Implements code changes".into(),
        agent: coder,
    });
    supervisor.add_agent(AgentHandle {
        id: "reviewer".into(),
        description: "Reviews code for correctness".into(),
        agent: reviewer,
    });

    let mut rx = supervisor.run("Add pagination to the user list API").await?;

    while let Some(event) = rx.recv().await {
        match event {
            SupervisorEvent::AgentEvent { agent_id, event: AgentEvent::TextDelta(t) } => {
                eprint!("[{agent_id}] {t}");
            }
            SupervisorEvent::StepEnd { result } => {
                eprintln!("\n--- {}: done ---", result.agent_id);
            }
            _ => {}
        }
    }

    Ok(())
}
```

---

## Phase 5: RPC Mode

JSONL over stdin/stdout, matching pi's protocol closely so existing pi RPC clients can work with minimal changes.

### Commands (stdin → subspace)

| Command | Fields | Description |
|---------|--------|-------------|
| `prompt` | `message`, `images?`, `streamingBehavior?` | Send a prompt |
| `steer` | `message` | Interrupt with new instruction |
| `follow_up` | `message` | Queue for after agent finishes |
| `abort` | — | Cancel current run |
| `get_state` | — | Current model, streaming status, etc. |
| `get_messages` | — | Full conversation history |
| `set_model` | `provider`, `model_id` | Switch model |
| `set_thinking_level` | `level` | Change thinking level |
| `compact` | `custom_instructions?` | Compact context |
| `new_session` | — | Start fresh |
| `bash` | `command` | Run shell command outside agent |

All commands accept an optional `id` field for correlation.

### Events (subspace → stdout)

Same event types as `AgentEvent`, serialized as JSON with a `type` field:

```json
{"type": "text_delta", "delta": "Hello"}
{"type": "tool_execution_start", "tool_call_id": "abc", "tool_name": "bash", "args": {...}}
{"type": "agent_end", "messages": [...]}
```

Responses to commands:
```json
{"type": "response", "id": "req-1", "command": "prompt", "success": true}
```

### Framing

- Strict LF (`\n`) delimited JSONL
- One JSON object per line
- No Unicode line separator splitting (same caveat as pi)

---

## Phase 6: CLI & TUI

Thin wrapper around the library. Three modes via `clap`:

```
subspace [options] [message...]

Modes:
  (default)         Interactive TUI
  -p, --print       Single-shot: send prompt, print response, exit
  --mode rpc        RPC mode (JSONL over stdio)

Options:
  --provider <name>       LLM provider (default: anthropic)
  --model <id>            Model ID
  --api-key <key>         API key (overrides env)
  --thinking <level>      off|low|medium|high
  --system-prompt <text>  Override system prompt
  --max-turns <n>         Max agent turns (default: 50)
  --no-session            Don't persist session
  -c, --continue          Continue last session
  -v, --version           Version
  -h, --help              Help
```

### TUI (minimal, for testing)

Using `ratatui` + `crossterm`:

- **Top pane**: Scrollable message history (user messages, assistant text, tool calls/results)
- **Bottom pane**: Input editor (single line is fine initially, multi-line stretch goal)
- **Footer**: Model name, token count, status
- **Keybindings**: Enter to send, Ctrl+C to abort/quit, Escape to cancel

No fancy features — this is a testing harness, not a product UI.

---

## Phase Plan

| Phase | What | Depends On |
|-------|------|------------|
| **1** | `types.rs`, `provider/` trait + Anthropic impl | — |
| **2** | `tools/` trait + read, write, edit, bash | — |
| **3** | `agent/` core loop + events | Phase 1, 2 |
| **4** | `supervisor/` + strategies | Phase 3 |
| **5** | `rpc/` mode | Phase 3 |
| **6** | CLI + minimal TUI | Phase 3, 5 |
| **7** | `session/` persistence (JSONL) | Phase 3 |
| **8** | Additional providers (OpenAI, etc.) | Phase 1 |

Phases 1 and 2 can be done in parallel. Phase 4 and 5 can be done in parallel after 3.

---

## Key Design Decisions

### Event channels, not callbacks

The agent returns a `tokio::sync::mpsc::Receiver<AgentEvent>` rather than taking a callback. Rationale:
- Idiomatic async Rust — consumers `recv()` in a select loop
- Decouples producer and consumer across tasks naturally
- The supervisor needs to `tokio::select!` across multiple agents running in parallel, which is awkward with callbacks
- Backpressure is built in
- Every real consumer (TUI, RPC, supervisor) would immediately create a channel inside a callback anyway
- `broadcast` channel available if multiple consumers need the same stream

### Single crate vs workspace

Start as a single crate. If it grows, split into:
- `subspace-core` (types, provider trait, tool trait)
- `subspace-anthropic` (provider impl)
- `subspace-agent` (agent loop)
- `subspace-supervisor` (orchestration)
- `subspace` (CLI, re-exports everything)

### Tool cwd isolation

Each tool instance is bound to a `cwd: PathBuf`. In multi-agent setups, different agents can have different working directories (e.g., one agent works in `frontend/`, another in `backend/`).

### Abort via CancellationToken

Use `tokio_util::CancellationToken` internally. `agent.abort()` triggers the token, which cancels the current LLM stream and any running tool (bash process gets killed).

### Error philosophy

- Tool execution errors are **not fatal** — they're fed back to the LLM as error tool results
- Provider errors (network, auth) **are fatal** — bubble up as `Result::Err`
- The agent distinguishes between retryable (429, 529, 5xx) and non-retryable errors

---

## Dependencies (Planned)

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime |
| `reqwest` | HTTP client (LLM APIs) |
| `serde` / `serde_json` | Serialization |
| `clap` | CLI arg parsing |
| `eventsource-stream` | SSE parsing for streaming |
| `futures` / `tokio-stream` | Stream combinators |
| `uuid` | ID generation |
| `chrono` | Timestamps |
| `crossterm` + `ratatui` | Terminal UI |
| `anyhow` / `thiserror` | Error handling |
| `async-trait` | Async trait support |
| `tracing` | Structured logging |
| `dirs` | Home directory resolution |
| `tokio-util` | CancellationToken |
