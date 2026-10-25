# Subspace — Spec v2

A self-hostable **agent workforce platform**. Subspace runs a fleet of long-lived,
independent agents ("Workers") on a server, connects them to external services via
Integrations, gives them a tiered memory (private + a shared company wiki), and exposes
a web console to create, configure, steer, and observe them.

> This supersedes `SPEC.md`, which described a library-first, embeddable harness. That
> crate is **not discarded** — it becomes the *engine* that the server embeds. `SPEC.md`
> is retained for reference until v2 stabilizes.

---

## Table of Contents

- [Core Premise](#core-premise)
- [Key Concepts](#key-concepts)
- [Non-Goals (for now)](#non-goals-for-now)
- [Architecture Overview](#architecture-overview)
- [Relationship to the Engine](#relationship-to-the-engine)
- [Crate & Package Structure](#crate--package-structure)
- [Data Model & Core Entities](#data-model--core-entities)
- [Server Subsystems](#server-subsystems)
- [Event Model](#event-model)
- [API Surface](#api-surface)
- [Guardrails, Policies & Approvals](#guardrails-policies--approvals)
- [Web Console](#web-console)
- [Persistence Schema](#persistence-schema)
- [Phase Plan](#phase-plan)
- [Key Design Decisions](#key-design-decisions)
- [Open Questions / Deferred](#open-questions--deferred)
- [Dependencies (Proposed)](#dependencies-proposed)

---

## Core Premise

Three layers:

1. **Engine** — the existing `subspace` crate (types, provider trait, tools, agent loop).
   Runs a single LLM ↔ tool loop with streaming events. Reused unchanged in spirit.
2. **Server** (`subspace-server`) — a long-running daemon that operates many **Workers**,
   holds their state, connects them to **Integrations**, routes external events to them,
   manages memory and credentials, and exposes an API.
3. **Client** — a web **Console** to manage workers, configure integrations, curate the
   shared wiki, chat with a worker, watch live activity, and approve sensitive actions.

The product is a "team of agents you manage," not a library you embed.

---

## Key Concepts

- **Worker** — a persistent, named, independently-operating agent (think: an employee).
  It has a role (system prompt), bound integrations, private memory/history, and a
  lifecycle that outlives any single task. Workers do not share conversation state.
- **Worker Home** — a per-worker directory holding everything *authored* about the worker:
  config (`worker.toml`), persona (`ROLE.md`), skills, and private memory notes. Copying a
  home clones the agent. Runtime state (history, events, approvals, credentials) lives in
  SQLite, never in the home. See [Worker Home](#worker-home-on-disk).
- **Skill** — a directory containing a `SKILL.md` (frontmatter `name` + `description`, then
  instructions) plus optional supporting files. Only the name/description are injected;
  the worker reads the body on demand (progressive disclosure). Skills are resolved by
  layered lookup: worker → global → built-in.
- **Mailbox** — a per-worker input queue. Both user prompts and routed integration events
  land here as **Stimuli**. The worker's agent loop consumes the mailbox; it does not care
  whether an item came from a human or a webhook. This is the primitive that unifies
  interactive and event-driven triggers.
- **Integration** — a connector to an external service. Contributes three things:
  **actions** (tools the worker can call), **event sources** (inbound triggers), and a
  **credential requirement** (per-worker secrets).
- **Trigger / Subscription** — a worker's declared interest in a class of events
  ("secretary listens to `email.received` on account X"). The Trigger Router matches
  inbound events to subscriptions and delivers them to mailboxes.
- **Memory (tiered):**
  - **Working** — the current task's message list (engine already handles this).
  - **Private** — per-worker long-term memory; freely writable by the worker via tools.
    Stored as markdown files in the worker home, indexed into SQLite FTS5.
  - **Shared** — a company wiki: readable by all workers, but **writes are proposals**
    that a human (or a designated worker) approves — the git PR model, minus git.
  - Access is **tools-first** with **progressive disclosure** (search/read on demand),
    mirroring how coding agents like `pi` use skills + context files.

---

## Non-Goals (for now)

- Multi-tenant SaaS / org & user management (assume single-tenant, self-hosted).
- Horizontal scale-out across multiple server instances (single node to start).
- Semantic/vector memory search (start with full-text search).
- A plugin marketplace or third-party integration distribution.
- Fine-grained RBAC. The console operator is trusted.
- Mobile clients.

---

## Architecture Overview

```
┌────────────────────────────────────────────────────────────────┐
│                       Web Console (client)                       │
│  manage workers · configure integrations · curate/approve wiki · │
│  chat with a worker · watch live activity · approve actions      │
└─────────────────────────────┬────────────────────────────────────┘
                   REST (mgmt) │ WebSocket/SSE (live events)
┌─────────────────────────────┴────────────────────────────────────┐
│                          subspace-server                          │
│                                                                   │
│  ┌──────────────┐         ┌───────────────────────────────────┐  │
│  │  API Layer   │         │  Event Ingress (webhooks/pollers) │  │
│  │ REST + WS    │         │            → Trigger Router       │  │
│  │ + webhooks   │         └──────────────────┬────────────────┘  │
│  └──────┬───────┘  user prompts        events│  (match subs)      │
│         └───────────────┐          ┌─────────┘                    │
│                         ▼          ▼                              │
│                    ┌─────────────────────────┐                   │
│                    │      Worker Manager      │ lifecycle:        │
│                    │  spawn · run · persist   │ park idle workers │
│                    └────────────┬─────────────┘                   │
│              ┌──────────────────┼──────────────────┐             │
│              ▼                  ▼                  ▼             │
│         ┌─────────┐        ┌─────────┐        ┌─────────┐        │
│         │ Worker  │        │ Worker  │  ...   │ Worker  │        │
│         │ Mailbox │◀─ prompts + events           │        │        │
│         │   ▼     │        └─────────┘        └─────────┘        │
│         │ Agent   │ ← engine (subspace crate: loop/provider/tools)│
│         │ loop    │                                              │
│         └────┬────┘                                              │
│    uses      │                                                   │
│  ┌───────────┼─────────────┬───────────────┬──────────────┐     │
│  ▼           ▼             ▼               ▼              ▼     │
│ ┌─────┐ ┌─────────┐ ┌────────────┐ ┌────────────┐ ┌──────────┐ │
│ │core │ │integr.  │ │  memory    │ │ shared wiki│ │ secret / │ │
│ │tools│ │actions  │ │ (private)  │ │ (fts+revs) │ │ cred store│ │
│ └─────┘ └─────────┘ └────────────┘ └────────────┘ └──────────┘ │
│                                                                  │
│  ┌────────────────────────── SQLite ─────────────────────────┐  │
│  │ workers · subscriptions · history · memory · wiki+revs ·   │  │
│  │ encrypted credentials · action log                         │  │
│  └────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

---

## Relationship to the Engine

The `subspace` crate (Phase 0, already built) is embedded unchanged in spirit:

| Engine piece | Role in the server |
|--------------|--------------------|
| `Agent` + `AgentConfig` | the per-worker loop; one `Agent` per `Worker` |
| `AgentEvent` | streamed out per worker, wrapped with a `worker_id` |
| `Provider` / `AnthropicProvider` | shared across workers (workers may pin different models) |
| `Tool` / `ToolRegistry` | integration **actions** and **memory** are just `Tool`s |
| `ToolResult` | unchanged; approval gating wraps execution, not the trait |
| `context.rs` | seed for the **shared wiki** injection / progressive disclosure |

Key consequence: **integrations and memory reuse the `Tool` trait.** A worker's tool
registry = core tools (read/write/edit/bash, optional & sandboxed) + integration actions +
memory tools. Nothing new at the trait level.

---

## Crate & Package Structure

```
subspace/                      # workspace root
├── crates/
│   ├── subspace/              # ENGINE (existing): types, provider, tools, agent, context
│   ├── subspace-server/       # NEW: the daemon (bulk of new work)
│   │   ├── src/
│   │   │   ├── main.rs        # bootstrap: config, db, http server, worker manager
│   │   │   ├── worker/        # Worker, Mailbox, Stimulus, runtime loop
│   │   │   ├── manager.rs     # Worker Manager (lifecycle)
│   │   │   ├── home.rs        # Worker Home: layout, worker.toml, ROLE.md, scoped file tools
│   │   │   ├── skills.rs      # skill discovery, frontmatter, layered lookup
│   │   │   ├── integration/   # Integration trait + built-ins (email, ...)
│   │   │   ├── trigger/       # event ingress, router, subscriptions
│   │   │   ├── memory/        # private store + shared wiki + memory tools
│   │   │   ├── secrets.rs     # encrypted credential store
│   │   │   ├── policy.rs      # action policies / approval evaluation
│   │   │   ├── store/         # SQLite persistence
│   │   │   └── api/           # REST + WS + webhook ingress
│   └── subspace-tui/          # existing CLI/TUI — demoted to a thin API client
└── console/                   # NEW: React (Vite + TS) web client
```

---

## Data Model & Core Entities

Illustrative Rust. IDs are UUID-v4 strings (`Id` from the engine's `types.rs`).

### Worker

```rust
type WorkerId = Id;

/// Loaded from `workers/<id>/worker.toml` + `ROLE.md` (identity), joined with wiring
/// from SQLite. The home directory is the source of truth for identity fields.
struct WorkerConfig {
    // identity — from the worker home
    id: WorkerId,
    name: String,                 // "Secretary" (display only; dirs are keyed by id)
    description: String,          // one line: what this worker is good at (used for routing)
    role: String,                 // system prompt / persona — contents of ROLE.md
    model: String,                // defaults to engine default (claude-opus-4-8)
    thinking: ThinkingLevel,      // engine type
    policies: Vec<PolicyRule>,    // action approval rules
    skills: SkillSelection,       // which global/built-in skills are enabled (default: all)
    enable_core_tools: bool,      // read/write/edit/bash (sandboxed) — off by default
    working_dir: Option<PathBuf>, // cwd for core tools; separate from the home dir

    // wiring — from SQLite (never copied on clone)
    integrations: Vec<IntegrationBinding>,  // which integrations + which credential
    subscriptions: Vec<Subscription>,       // which events wake this worker
}

enum WorkerStatus { Idle, Running, AwaitingApproval, Paused, Error(String) }

struct Worker {
    config: WorkerConfig,
    agent: Agent,                 // engine
    mailbox: MailboxSender,
    status: WorkerStatus,
}
```

### Worker Home (on-disk)

```
<data_dir>/
├── subspace.db                  # operational state (see Persistence Schema)
├── skills/<skill>/SKILL.md      # global skills, available to all workers
├── wiki/                        # shared wiki docs (storage TBD — see Open Questions)
└── workers/<worker_id>/         # one Worker Home per worker
    ├── worker.toml              # identity config: name, description, model, thinking,
    │                            #   policies, skill selection, core-tool settings
    ├── ROLE.md                  # persona / standing instructions → system prompt
    ├── skills/<skill>/SKILL.md  # worker-specific skills (override same-named global ones)
    └── memory/*.md              # private memory notes
```

**Identity vs. wiring vs. state.** Every piece of worker data falls in exactly one bucket,
which defines where it lives and what a clone/export carries:

| Bucket | Examples | Lives in | Cloned / exported? |
|--------|----------|----------|--------------------|
| Identity | config (incl. approval policies), persona, skills, curated memory | Worker Home | yes \* |
| Wiring | integration bindings, credentials, subscriptions | SQLite | **no** — rebound per worker |
| State | history, events, approvals, action log, status | SQLite | no (full backup only) |

\* Policies travel with a clone on purpose: a copied worker starts with the same approval
rules as its source (conservative by default) and is loosened explicitly if needed.

Wiring is excluded because some credentials are exclusive (see
[exclusive sources](#integration-registry--event-ingress)) and single-use OAuth refresh
tokens cannot be safely duplicated.

**Rules:**

- **Keyed by id, not name.** Renames touch only `worker.toml`.
- **One source of truth.** The home is authoritative for identity; the `workers` table is
  an index rebuilt from homes on `boot()`. Console `PATCH /workers/{id}` writes
  `worker.toml` / `ROLE.md`, then refreshes the index.
- **Single writer.** The server is the only process that writes a home (the Worker Manager
  owns one loop per worker). Hand edits are allowed while stopped, or picked up at the
  next session start.
- **Read at session start.** Identity files are (re)read when a worker boots or starts a
  fresh context. No file watching initially.
- **Delete order.** Remove SQLite rows first, then the directory; an orphaned home dir is
  harmless and reported on boot.

**Home-scoped file tools.** Workers get `home_read` / `home_write` / `home_edit`,
restricted to their own home. These work even when `enable_core_tools` is off, so skills
and memory don't depend on unrestricted filesystem access. Write access is limited to
`skills/` and `memory/`; `worker.toml` and `ROLE.md` are read-only to the worker (changed
only via the console), so a worker cannot loosen its own policies or rewrite its persona.

### Skills

```
skills/<name>/
├── SKILL.md        # ---\nname: ...\ndescription: ...\n---\n<instructions>
└── ...             # optional scripts, references, templates
```

- **Discovery:** worker `skills/` → global `<data_dir>/skills/` → built-ins compiled into
  the server. First match by name wins, so a worker can override a shared skill.
- **No copying.** Global skills are looked up, not copied into each home, so updates
  reach every worker and a worker's overrides are never clobbered.
- **Injection:** only `name` + `description` for enabled skills go into the system prompt,
  as a compact list with each skill's path. The worker reads the body with `home_read`
  (or `read` for global skills, via a read-only scoped path) when relevant.
- **Authoring:** a worker may create/edit skills in its own `skills/` with `home_write` /
  `home_edit`. Global skills are edited by humans (console or filesystem).
- **Frontmatter:** only `name` and `description` are required; a minimal hand-rolled
  `key: value` parser covers this without a YAML dependency.
- **Compatibility:** the format matches pi / Claude-style `SKILL.md` directories, so
  existing skill packs can be dropped in.

### Mailbox & Stimulus

```rust
/// Anything that can wake a worker. Prompts and events are the same to the loop.
enum Stimulus {
    /// From a human via the console.
    UserPrompt { text: String, images: Vec<Image>, reply_to: ClientRef },
    /// From an integration event source, matched by the trigger router.
    Event { integration: String, kind: String, payload: serde_json::Value },
    /// Human response to an approval request.
    ApprovalResult { action_id: Id, approved: bool, note: Option<String> },
    /// Lifecycle / config control (pause, set_model, new_task, ...).
    Control(ControlMsg),
}

type MailboxSender = mpsc::Sender<Stimulus>;
```

Events are rendered into a user-role `Message` for the agent (e.g. a compact
`"[event: email.received] from=... subject=... body=..."`), so the worker's role prompt
governs *how* it reacts. The raw payload is also retained for tool calls.

### Integration

```rust
#[async_trait]
trait Integration: Send + Sync {
    fn manifest(&self) -> IntegrationManifest;

    /// Tools this integration contributes, bound to a specific worker's credentials.
    fn actions(&self, creds: &Credentials) -> Vec<Arc<dyn Tool>>;

    /// Begin producing events for a subscription; push normalized events to `sink`.
    async fn start_source(
        &self,
        sub: &Subscription,
        creds: &Credentials,
        sink: EventSink,
    ) -> Result<SourceHandle>;
}

struct IntegrationManifest {
    id: String,                     // "email", "github", "slack"
    name: String,
    actions: Vec<ToolDefinition>,   // engine type
    event_kinds: Vec<String>,       // e.g. ["email.received"]
    credential_schema: serde_json::Value,  // JSON Schema for required creds
}
```

### Subscription & Trigger

```rust
struct Subscription {
    id: Id,
    worker_id: WorkerId,
    integration: String,
    event_kind: String,             // "email.received"
    filter: Option<serde_json::Value>,  // e.g. { "from": "*@important.com" }
}
```

### Memory

```rust
/// Private, per-worker. Freely writable by the worker.
/// Backed by `workers/<id>/memory/*.md`; `memory_notes` in SQLite is an FTS5 index
/// rebuilt from those files on boot and updated on every write.
#[async_trait]
trait PrivateMemory: Send + Sync {
    async fn save(&self, worker: &WorkerId, note: MemoryNote) -> Result<Id>;
    async fn recall(&self, worker: &WorkerId, query: &str, k: usize) -> Result<Vec<MemoryNote>>;
    async fn history(&self, worker: &WorkerId, page: Page) -> Result<Vec<Message>>;
}

/// Shared company wiki. Read is free; writes are proposals pending approval.
#[async_trait]
trait SharedKnowledge: Send + Sync {
    async fn search(&self, query: &str, k: usize) -> Result<Vec<WikiHit>>;   // FTS
    async fn read(&self, path: &str) -> Result<WikiDoc>;
    async fn propose_edit(&self, worker: &WorkerId, edit: WikiEdit) -> Result<ProposalId>;
    async fn approve(&self, proposal: ProposalId, by: Approver) -> Result<Revision>;
    async fn reject(&self, proposal: ProposalId, by: Approver) -> Result<()>;
}
```

Exposed to the worker as tools: `memory_save`, `memory_recall`, `wiki_search`,
`wiki_read`, `wiki_propose_edit`. Progressive disclosure = the worker searches/reads
only the wiki slices it needs, rather than injecting the whole thing each turn. A small
always-injected "core" block (persona + a few pinned facts) is allowed.

### Credentials / Secrets

```rust
#[async_trait]
trait SecretStore: Send + Sync {
    async fn put(&self, worker: &WorkerId, integration: &str, creds: Credentials) -> Result<()>;
    async fn get(&self, worker: &WorkerId, integration: &str) -> Result<Option<Credentials>>;
    async fn delete(&self, worker: &WorkerId, integration: &str) -> Result<()>;
}
```

Encrypted at rest with a server master key (AEAD). Never returned to the console in
plaintext; only "configured / not configured" status is exposed.

---

## Server Subsystems

### Worker Manager

Replaces the old `Supervisor` / `Strategy` machinery. Pure lifecycle:

```rust
struct WorkerManager {
    workers: DashMap<WorkerId, RunningWorker>,   // handle + mailbox sender
    store: Arc<dyn Store>,
    provider: Arc<dyn Provider>,
    integrations: Arc<IntegrationRegistry>,
    memory: MemoryHandles,
    events: EventBus,
}

impl WorkerManager {
    async fn boot(&self) -> Result<()>;                  // restore all workers from store
    async fn spawn(&self, cfg: WorkerConfig) -> Result<WorkerId>;
    async fn deliver(&self, id: &WorkerId, stimulus: Stimulus) -> Result<()>;
    async fn pause(&self, id: &WorkerId) -> Result<()>;
    async fn remove(&self, id: &WorkerId) -> Result<()>;
}
```

Idle workers are parked awaiting `mailbox.recv()` — near-zero cost, so many can coexist.

### Worker Runtime Loop

```
loop {
    let stimulus = mailbox.recv().await          // park while idle → Idle
    match stimulus {
        UserPrompt | Event => {
            status = Running
            let msg = render_to_message(stimulus)     // event → user message
            let mut rx = agent.prompt(msg).await?      // engine loop
            while let Some(ev) = rx.recv().await {
                // approval gate: intercept ToolCallStart
                if policy.evaluate(worker, tool, args) == RequireApproval {
                    emit ApprovalRequired; status = AwaitingApproval
                    // park loop until ApprovalResult stimulus arrives
                }
                events.publish(worker_id, ev)          // → WS console clients
            }
            persist(history, memory)
        }
        ApprovalResult { .. } => resume_pending_action(),
        Control(msg)          => apply(msg),           // pause, set_model, new_task
    }
}
```

The approval gate wraps tool execution; the engine's `Tool`/`Agent` contracts are
unchanged. (Implementation detail: gating likely lives in a wrapper `Tool` or an agent
hook — TBD in Phase D.)

### Integration Registry & Event Ingress

- **Registry** — all available `Integration`s, keyed by `manifest().id`. Built-ins are
  compiled in initially. (MCP as the general interface is a candidate — see Deferred.)
- **Event ingress** — two producers feed the Trigger Router:
  - **Webhooks**: `POST /ingress/{integration}` endpoints; the integration normalizes the
    raw payload into an internal `Event`.
  - **Pollers**: per-source async tasks (e.g. IMAP poll) that emit `Event`s on a schedule.
- **Trigger Router** — indexes subscriptions; for each incoming `Event`, finds matching
  subscriptions (by `integration` + `event_kind` + `filter`) and delivers a
  `Stimulus::Event` to each subscriber's mailbox.
- **Exclusive sources.** Fan-out applies to *events*, not *connections*. Sources that hold a
  credential-bound session (IMAP IDLE, bot long-polling) must have exactly one owner: the
  registry takes a lock per `(integration, credential fingerprint)` and refuses to start a
  second source on the same credential, naming the worker that holds it.

### History & Compaction

A worker's conversation is continuous: history is reloaded on boot and appended forever.
To keep context bounded, `ControlMsg::Compact` (also triggered automatically past a token
threshold) summarizes older turns into a compaction record and starts a fresh working
context. The full transcript stays in `messages`; only the agent's working context is
replaced. A "new task" is a compaction, never a fork into a separate conversation.

---

## Event Model

Every engine `AgentEvent` from a worker is wrapped for attribution and streamed to
subscribed console clients:

```rust
struct ServerEvent {
    worker_id: WorkerId,
    seq: u64,                 // monotonic per worker; lets clients resume
    ts: DateTime<Utc>,
    inner: WorkerEvent,
}

enum WorkerEvent {
    StimulusReceived(StimulusSummary),           // "got prompt" / "got email.received"
    Agent(AgentEvent),                           // engine events (text, tool calls, ...)
    ApprovalRequired { action_id: Id, tool: String, args: serde_json::Value },
    ActionResolved { action_id: Id, approved: bool },
    StatusChanged(WorkerStatus),
}
```

- Cross-worker isolation: no shared event stream between workers; the console subscribes
  per worker (or to a filtered global feed).
- `seq` enables reconnect/replay so the console can catch up after a dropped socket.
- This is also the on-the-wire payload for WS, and the serialization target that the old
  "RPC mode" was reaching for — now with worker addressing.

---

## API Surface

### REST (management)

| Method & Path | Purpose |
|---------------|---------|
| `GET /workers` | list workers + status |
| `POST /workers` | create a worker |
| `GET /workers/{id}` | worker detail/config |
| `PATCH /workers/{id}` | update config (role, model, thinking, ...) |
| `DELETE /workers/{id}` | remove a worker |
| `POST /workers/{id}/prompt` | send a user prompt (→ mailbox) |
| `POST /workers/{id}/pause` · `/resume` | lifecycle control |
| `GET /workers/{id}/history` | conversation/message history |
| `GET /workers/{id}/memory` | browse private memory |
| `GET /integrations` | available integrations + manifests |
| `POST /workers/{id}/integrations` | bind an integration + store credentials |
| `DELETE /workers/{id}/integrations/{integration}` | unbind |
| `GET/POST/DELETE /workers/{id}/subscriptions` | manage triggers |
| `GET /wiki` · `GET /wiki/{path}` | browse shared knowledge |
| `PUT /wiki/{path}` | human edit (direct commit) |
| `GET /wiki/proposals` | pending worker-proposed edits |
| `POST /wiki/proposals/{id}/approve` · `/reject` | curate proposals |
| `GET /approvals` | pending action approvals |
| `POST /approvals/{id}` `{approve\|reject, note?}` | resolve an approval |

### Streaming

- `GET /workers/{id}/stream` — WebSocket (or SSE) of `ServerEvent`s for one worker.
- `GET /stream` — filtered global feed across workers.
- Clients send `?since={seq}` to resume.

### Webhook Ingress

- `POST /ingress/{integration}` — raw inbound events from external services; normalized by
  the integration, then handed to the Trigger Router.

The `subspace-tui` CLI becomes a thin client of this API (interactive = open a stream +
POST prompts; single-shot = POST prompt, read stream to completion).

---

## Guardrails, Policies & Approvals

Autonomy is a core goal, so action safety is first-class from Phase D:

```rust
enum ActionDecision { Allow, RequireApproval, Deny }

struct PolicyRule {
    // e.g. tool = "email.send", condition = recipient not in known_contacts → RequireApproval
    tool: String,
    when: Option<serde_json::Value>,
    decision: ActionDecision,
}
```

- Every tool call is evaluated against the worker's policies **before execution**.
- `RequireApproval` → emit `ApprovalRequired`, set `AwaitingApproval`, park until the human
  resolves it in the console (delivered back as `Stimulus::ApprovalResult`).
- `Deny` → tool result is a synthetic error fed back to the LLM (non-fatal, engine-style).
- Every executed action is written to an **action log** (audit trail) with the decision and
  approver.
- Sensible defaults: read-like actions `Allow`; send/write/spend/delete-like actions
  `RequireApproval` until an operator loosens them. The secretary *drafts* freely but
  *sending* is gated unless a policy says otherwise.

---

## Web Console

Responsibilities (React + Vite + TypeScript):

- **Workers**: list/create/configure; per-worker view with live activity stream.
- **Chat**: talk to a worker; see thinking, tool calls, and results in real time.
- **Integrations**: browse available integrations, bind them to a worker, enter
  credentials (write-only), manage subscriptions/triggers.
- **Wiki**: browse/edit shared knowledge; review & approve/reject worker-proposed edits;
  view revision history.
- **Approvals**: an inbox of pending actions; approve/reject with a note.
- **Audit**: action log per worker.

Talks to the server via REST + WebSocket. No business logic beyond presentation and the
approval/curation workflows.

---

## Persistence Schema

SQLite (single-node, self-hosted). Illustrative tables:

Authored content (config, persona, skills, memory notes, possibly wiki docs) lives in the
[Worker Home](#worker-home-on-disk) and is indexed here; SQLite is authoritative only
for wiring and state.

| Table | Key columns |
|-------|-------------|
| `workers` | id, name, description, home_path, created_at — **index** rebuilt from homes (status is runtime-only) |
| `legacy_workers` | A3 rows not yet moved into homes; drained on `boot()` (home created first, then index + delete in one transaction). Unconvertible rows are logged and left in place. |
| `worker_integrations` | worker_id, integration, credential_ref |
| `subscriptions` | id, worker_id, integration, event_kind, filter(json) |
| `messages` | id, worker_id, role, content(json), ts — conversation history |
| `memory_notes` | id, worker_id, path, text, tags(json), ts — FTS5 **index** over `memory/*.md` |
| `compactions` | id, worker_id, summary, through_seq, ts |
| `wiki_docs` | path, title, current_rev |
| `wiki_revisions` | id, path, content, author (worker/human), status(proposed/approved), ts |
| `credentials` | worker_id, integration, ciphertext, nonce |
| `pending_approvals` | action_id, worker_id, tool, args(json), ts |
| `action_log` | id, worker_id, tool, args(json), decision, approved_by, ts |

Full-text search via SQLite **FTS5** for `memory_notes` and `wiki_docs`. No vector store
initially (see Deferred).

---

## Phase Plan

| Phase | Deliverable | Proves |
|-------|-------------|--------|
| **0 (done)** | Engine: types, Anthropic provider, tools (read/write/edit/bash), agent loop, context loading, mock provider, minimal TUI | the loop works |
| **A** | Persistence (SQLite) + `Worker` + `WorkerManager`; one worker, interactive via a minimal API | long-lived, stateful loop |
| **A4** | Worker Home: `worker.toml` + `ROLE.md` as source of truth, `workers` table as index, migrate A3 rows to homes; skill discovery + injection; home-scoped file tools | identity is portable files |
| **B** | Server API (REST + WS) + minimal web console: create worker, chat, watch live events | usable interactively |
| **C** | Integration framework (actions + credential/secret store) + **email** integration end-to-end (actions only) | worker acts on a real service |
| **D** | Trigger Router (webhook ingress + pollers + subscriptions) + action approval checkpoint | **secretary reacts to incoming email** |
| **E** | Memory: private memory tools + shared wiki (FTS + versioned propose/approve) + console curation | knowledge substrate |
| **F** | Multi-worker coordination: ephemeral delegation (agents-as-tools) + durable handoffs (task table / mailbox messages), routed by worker `description` | the "workforce" |
| **Later** | more integrations, MCP integration interface, semantic search, tool sandboxing, additional providers | scale/hardening |

`A → B → C → D` is the fastest path to the compelling demo: a secretary that reacts to
incoming email *and* answers questions about it. `E` and `F` enrich from there.

---

## Key Design Decisions

- **One loop, two input sources.** The mailbox merges prompts and events into a single
  stimulus stream, so interactive and autonomous behavior share one code path.
- **Manager, not Supervisor.** The old `Strategy`/`Step`/`Decision` machinery is dropped.
  Lifecycle lives in the Worker Manager; *in-worker* delegation is agents-as-tools;
  *cross-worker* coordination is mailbox messages (Phase F).
- **Files for authored content, SQLite for operational state.** Persona, config, skills,
  and memory notes are files in a per-worker home (editable, diffable, cloneable); history,
  events, approvals, credentials, and subscriptions are rows. Each datum has one source of
  truth; SQLite indexes the files rather than duplicating them.
- **Integrations & memory are just Tools.** Reuses the engine's `Tool`/`ToolRegistry`;
  no parallel abstraction. Actions, `wiki_search`, `memory_save` are all tools.
- **Shared knowledge = versioned proposals.** Private memory is free-write; the shared
  wiki takes worker writes as proposals that a human approves — auditable autonomy without
  a bespoke approval bureaucracy (the coding-agent + git pattern, generalized).
- **Progressive disclosure over injection.** Workers pull memory/wiki slices via tools
  instead of stuffing everything into context, mirroring `pi`-style skills/context files.
- **Safety before autonomy ships.** The approval checkpoint lands in the same phase (D)
  that introduces autonomous triggers; no autonomous send/write without a policy or a tap.
- **Single node, SQLite, event channels.** Keep the runtime simple; `AgentEvent` channels
  from the engine extend naturally to `ServerEvent` over WebSocket with `worker_id` + `seq`.

---

## Open Questions / Deferred

- **MCP as the integration interface.** Modeling each integration as an MCP server (client
  side in subspace) would let us ride an ecosystem instead of hand-writing connectors.
  Attractive, but a new dependency/direction — decide before Phase C solidifies.
- **Tool sandboxing.** `bash` and file tools inside a shared, possibly-autonomous server
  are a real risk. Off by default per worker (`enable_core_tools`); proper sandboxing
  (container/namespace) is a hardening item.
- **Semantic memory.** FTS first; vector embeddings + a store is a later, dependency-heavy
  upgrade.
- **Cross-worker protocol (Phase F).** Leaning toward *both*: agents-as-tools for short,
  ephemeral delegation whose result returns to the caller's context, plus a durable task
  table (or mailbox messages) for handoffs that must survive crashes, retry, and be
  visible/editable by humans. Open: whether the durable path is a task board or plain
  mailbox messages with persistence.
- **Wiki storage.** Keep revisions in SQLite (current plan) or store wiki docs as files
  under `<data_dir>/wiki/` and use real git for propose/approve (shell out to `git` or a
  `git2`-style crate — new dependency).
- **Live reload of homes.** Session-start reads are the baseline; file watching for hand
  edits while running is deferred.
- **Home export format.** Plain tarball of the home vs. a git repo per worker (enables
  sharing/updating worker templates in place).
- **Multi-provider per worker.** The engine supports it via the `Provider` trait; confirm
  whether worker config should reference a `(provider, model)` pair rather than a bare
  model string.
- **Approval UX for latency-sensitive events.** Some events want fast autonomous action;
  define escalation/timeout behavior for `AwaitingApproval`.

---

## Dependencies (Proposed)

> Per project policy, **no new dependencies are added without approval.** These are
> proposals for the server/console, listed for discussion — nothing here is committed yet.

| Crate / Package | Purpose | Status |
|-----------------|---------|--------|
| `axum` (+ `tower`, `tower-http`) | HTTP + WebSocket server | pending approval |
| `sqlx` (sqlite) *or* `rusqlite` | SQLite persistence + FTS5 | pending approval — pick one |
| AEAD crate (e.g. `chacha20poly1305`) | encrypt credentials at rest | pending approval |
| `tokio-cron-scheduler` or similar | pollers / scheduled triggers | pending approval |
| React + Vite + TypeScript | web console | pending approval (separate `console/` package) |
| `toml` | parse/write `worker.toml` | **approved** |

Already in the workspace and reused: `tokio`, `reqwest`, `serde`/`serde_json`, `clap`,
`anyhow`/`thiserror`, `async-trait`, `chrono`, `uuid`, `tracing`, `dirs`, `tokio-util`,
`ratatui`/`crossterm` (TUI client).
