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
- [Workspaces, Channels & DMs](#workspaces-channels--dms)
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
  lifecycle that outlives any single task. Each worker keeps its own model context; what
  workers share is a channel's public transcript, never each other's sessions.
- **Worker Home** — a per-worker directory holding everything *authored* about the worker:
  config (`worker.toml`), persona (`ROLE.md`), skills, and private memory notes. Copying a
  home clones the agent. Runtime state (history, events, approvals, credentials) lives in
  SQLite, never in the home. See [Worker Home](#worker-home-on-disk).
- **Skill** — a directory containing a `SKILL.md` (frontmatter `name` + `description`, then
  instructions) plus optional supporting files. Only the name/description are injected;
  the worker reads the body on demand (progressive disclosure). Skills are resolved by
  layered lookup: worker → global → built-in.
- **Workspace** — a top-level container (like a Slack workspace) holding channels and the
  workers that may join them. A worker can belong to any number of workspaces. One default
  workspace, **General**, exists to start with.
- **Channel** — collaborative context for a piece of work: a named, shared transcript
  inside a workspace whose members are the human operator plus some workers. Workers reply
  when @mentioned, including by each other, which is how agents talk to one another; an
  operator post that mentions nobody goes to the best-suited member.
- **DM** — the private 1:1 conversation between the operator and one worker (the original
  "chat with a worker"). Every worker has exactly one.
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
│   └── subspace-server/       # NEW: the daemon (bulk of new work)
│       ├── src/
│       │   ├── main.rs        # bootstrap: config, db, http server, worker manager
│       │   ├── worker/        # Worker, Mailbox, Stimulus, runtime loop
│       │   ├── manager.rs     # Worker Manager (lifecycle)
│       │   ├── home.rs        # Worker Home: layout, worker.toml, ROLE.md, scoped file tools
│       │   ├── skills.rs      # skill discovery, frontmatter, layered lookup
│       │   ├── integration/   # Integration trait + built-ins (email, ...)
│       │   ├── trigger/       # event ingress, router, subscriptions
│       │   ├── memory/        # private store + shared wiki + memory tools
│       │   ├── secrets.rs     # encrypted credential store
│       │   ├── policy.rs      # action policies / approval evaluation
│       │   ├── store/         # SQLite persistence
│       │   └── api/           # REST + WS + webhook ingress
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
    model: String,                // defaults to engine default (claude-opus-5-5)
    thinking: ThinkingLevel,      // engine type
    policies: Vec<PolicyRule>,    // action approval rules
    skills: SkillSelection,       // [skills] disabled = [...]; default: all enabled
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

**Home-scoped file tools.** Workers get `home_read` / `home_write` / `home_edit`: the
engine's read/write/edit tools behind a path guard (`HomeScope`). These work even when
`enable_core_tools` is off, so skills and memory don't depend on unrestricted filesystem
access.

- **Read:** the worker's home, the global skills dir, and the real directories of the
  skills offered to this worker (so `<skills>` locations resolve even when symlinked).
- **Write:** only inside the home's `skills/` and `memory/`; `worker.toml` and `ROLE.md`
  are read-only to the worker (changed only via the console), so a worker cannot loosen
  its own policies or rewrite its persona.
- **Resolution:** paths are relative to the home (or absolute); `..` is rejected; the
  deepest existing ancestor is canonicalized (following symlinks) and compared against
  canonicalized roots, so symlinks cannot escape the scope.
- **Denials** are returned as tool errors, not run failures, so the model can recover.
- **Limits:** the check-then-act gap (TOCTOU) is acceptable while workers have no tool
  that can create symlinks; revisit when core tools are sandboxed. Indexing `memory/`
  writes into FTS5 arrives with Phase E.

### Skills

```
skills/<name>/
├── SKILL.md        # ---\nname: ...\ndescription: ...\n---\n<instructions>
└── ...             # optional scripts, references, templates
```

- **Discovery:** worker `skills/` → global `<data_dir>/skills/` → built-ins compiled into
  the server (none yet). First match by name wins, so a worker can override a shared
  skill. Each `<name>/SKILL.md` one level down is a skill; symlinked skill dirs are
  followed. Broken skills are logged and skipped. Re-read whenever a worker starts.
- **Selection:** all discovered skills are enabled unless listed in `worker.toml`:
  `[skills]` / `disabled = ["name", ...]`.
- **No copying.** Global skills are looked up, not copied into each home, so updates
  reach every worker and a worker's overrides are never clobbered.
- **Injection:** a `<skills>` block appended after `ROLE.md`, one
  `<skill><name/><description/><location/></skill>` per enabled skill (XML-escaped;
  `location` is the absolute `SKILL.md` path). The worker reads the body on demand via
  the home-scoped tools, which allow reads within its home and the global skills dir.
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

A worker has one *session* (model context) per conversation: its DM, plus one per channel
it takes part in (see [Workspaces, Channels & DMs](#workspaces-channels--dms)). Each
session is continuous: history is reloaded on demand and appended forever.
To keep context bounded, `ControlMsg::Compact` (also triggered automatically past a token
threshold) summarizes older turns into a compaction record and starts a fresh working
context. The full transcript stays in `messages`; only the agent's working context is
replaced. A "new task" in a DM is a compaction; separate pieces of work get a channel.

---

## Workspaces, Channels & DMs

Slack-shaped collaboration. A **workspace** contains **channels**; a channel is the shared
context for one piece of work, and its members (the operator plus some workers) talk in
one transcript. The operator's private 1:1 chat with a worker is its **DM**.

### Membership

- Workspaces ↔ workers is many-to-many. A new worker joins **General** (the default,
  seeded on first boot); existing workers with no workspace are added to it on boot.
- A channel belongs to one workspace; its worker members must be members of that
  workspace. Removing a worker from a workspace removes it from that workspace's channels.
- The operator is an implicit member of every channel (single-tenant, no auth).
- Each member has a **handle** used for mentions: the slugified worker name (`Research
  Bot` → `@research-bot`). Handles must be unique within a channel; adding a worker whose
  handle collides is rejected. The operator's handle is `@you`.

### Public transcript vs. private sessions

Two records, kept separate:

1. **Posts**: the channel's public transcript (`channel_posts`): author (operator or
   worker, with the author's name captured at post time), text, mentions, and routing data.
   Everyone sees these.
2. **Sessions**: each worker's private model context *for that channel* (rows in
   `messages` with `conversation = channel:<id>`), including its tool calls and results.
   Other members only see the final reply.

A worker's DM is the conversation `dm`. The engine is unchanged: one `Agent` per worker,
and the worker swaps the agent's messages when it switches conversations
(`Agent::replace_messages`). The engine still sees only user/assistant turns; speaker
attribution lives in the text.

### A channel turn

When a worker is woken in a channel (`Stimulus::ChannelTurn`), it:

1. Collects the posts since its last turn (`channel_members.last_seen_seq`), excluding
   its own. A first turn (or a long gap) is capped at the most recent 50 posts, with a
   note that earlier posts were omitted. If there is nothing new, the turn is skipped,
   which de-duplicates repeated wakes.
2. Prompts its channel session with one user message:
   ```
   <channel name="launch-plan" workspace="General">
   Members: @you (human operator), @researcher (Researcher: finds facts), @writer (Writer: drafts copy)
   You are @writer.
   New posts:
   [@you] @researcher what changed in v2?
   [@researcher] Three things: ...  @writer can you draft the announcement?
   </channel>
   Reply with your next post in this channel. Mention @handle to ask a member to respond.
   ```
3. Posts the final assistant text of the run as its reply. A run that fails produces a
   channel notice instead of a post.

Live activity (text deltas, tool calls) streams on the channel's event log while the
worker runs, so viewers watch it work; afterwards the post holds only the text.

### Routing and loop safety

- **Mentions are explicit routing.** A post wakes the members it @mentions, except its
  author.
- **Unaddressed operator posts are auto-routed.** When the operator mentions nobody, the
  hub picks one running member to answer:
  - none running → nobody; one → that worker (no model call);
  - two or more → one quick call to the **router model** (`--router-model`, default
    `claude-haiku-5-5`). It sees each member's handle, name, description, and the start
    of its role (600 chars), plus the last 10 posts, so follow-ups go to whoever is already
    on it. It answers `{"handle": ..., "reason": ...}`, or `handle: null` when the post needs
    no reply (thanks, FYIs).

  The decision is stored on the post (`routing`: `direct` / `pending` / `chosen` /
  `nobody` / `failed`) and published as a `Routed` event; the console shows "→ Writer ·
  reason". Routing runs in the background, so posting stays instant. A failed or unusable
  answer wakes nobody and leaves a notice asking for an @mention.
- **Worker replies are never auto-routed**: a reply that mentions nobody ends the chain.
- **Hop limit.** Each operator post starts a chain (`root_id`, `hops = 0`). A worker's
  reply carries `hops = trigger.hops + 1`. When a reply at `hops ≥ HOP_LIMIT` (default
  **5**) mentions workers, they are not woken; a notice says the chain stopped and the
  operator can continue it by posting.
- **One thing at a time.** A worker's mailbox is still serial: a mention while it is busy
  in its DM (or another channel) waits in line. Delivery between workers never blocks a
  worker's loop (a full mailbox drops the wake with a notice).

### Deferred (wanted)

- **Channel files**: a shared folder per channel that members can read and write (like
  Slack canvases). Planned; likely lands with Phase E's memory/wiki work.
- `@channel` mentions; a tool-call summary attached to posts; multiple workspaces in the
  console (the data model and API already allow them).

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
    UserMessage(Message),                        // persisted prompt, with its stable id
    Agent(AgentEvent),                           // engine events (text, tool calls, ...)
    Notice(Notice),                              // persisted run error/warning, with its id
    ApprovalRequired { action_id: Id, tool: String, args: serde_json::Value },
    ActionResolved { action_id: Id, approved: bool },
    StatusChanged(WorkerStatus),
}
```

- A worker's event log carries its DM activity plus status changes. Activity from a
  channel run goes to that **channel's** event log instead, wrapped with the worker id:

```rust
struct ChannelEvent {
    channel_id: ChannelId,
    seq: u64,                 // monotonic per channel
    ts: DateTime<Utc>,
    inner: ChannelEventKind,
}

enum ChannelEventKind {
    Post(ChannelPost),                            // persisted, stable id
    Routed { post_id: Id, routing: Routing },     // who answers an unaddressed post
    Notice(Notice),                               // run failure, hop limit, ...
    Worker { worker_id: WorkerId, event: WorkerEvent },  // live run activity
    Changed,                                      // name/topic/members changed: refetch
    Deleted,
}
```

- Clients resume either log with `since` + `epoch`. A channel's resume cursor points before
  the earliest run still in progress, so a reconnect replays every active run in full.
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
| `GET /workspaces` | list workspaces |
| `GET /workspaces/{id}` | workspace detail: members + channels |
| `POST /workspaces/{id}/members` · `DELETE …/members/{worker}` | add / remove a worker |
| `POST /workspaces/{id}/channels` | create a channel (name, topic, initial members) |
| `GET /channels/{id}` | channel detail, members, posts, notices, stream cursor |
| `PATCH /channels/{id}` · `DELETE /channels/{id}` | rename / set topic · delete |
| `POST /channels/{id}/members` · `DELETE …/members/{worker}` | channel membership |
| `POST /channels/{id}/posts` | post as the operator (routes mentions) |
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
- `GET /channels/{id}/stream` — WebSocket of `ChannelEvent`s for one channel.
- `GET /stream` — filtered global feed across workers.
- Clients send `?since={seq}` to resume.

### Webhook Ingress

- `POST /ingress/{integration}` — raw inbound events from external services; normalized by
  the integration, then handed to the Trigger Router.

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

- **Layout** (Slack-shaped): a sidebar with the workspace, its **Channels**, and **Direct
  messages** (one per worker).
- **Workers**: list/create/configure; the DM view shows live activity.
- **Chat**: DM a worker; see thinking, tool calls, and results in real time.
- **Channels**: create, set topic, manage members; a shared transcript with author names,
  @mention autocomplete, and each responding worker's live activity.
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
| `messages` | id, worker_id, conversation (`dm` or `channel:<id>`), role, content(json), ts — each worker's private sessions |
| `workspaces` | id, name, created_at; `General` seeded on first boot |
| `workspace_members` | workspace_id, worker_id, joined_at |
| `channels` | id, workspace_id, name (unique per workspace), topic, created_at |
| `channel_members` | channel_id, worker_id, last_seen_seq, joined_at |
| `channel_posts` | seq, id, channel_id, author_kind, author_id, author_name, text, mentions(json), root_id, hops, routing(json), created_at |
| `memory_notes` | id, worker_id, path, text, tags(json), ts — FTS5 **index** over `memory/*.md` |
| `compactions` | id, worker_id, summary, through_seq, ts |
| `notices` | id, worker_id, conversation, level (warning/error), text, created_at; run errors shown to humans, never sent to the model |
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
| **A4 (done)** | Worker Home: `worker.toml` + `ROLE.md` as source of truth, `workers` table as index, migrate A3 rows to homes; skill discovery + injection; home-scoped file tools | identity is portable files |
| **B (done)** | Server API (REST + WS) + minimal web console: create worker, chat, watch live events | usable interactively |
| **B2** | Workspaces & channels: default workspace, channels with shared transcripts, mention routing with a hop limit, DMs as today's chat; Slack-style console | agents collaborate in shared context |
| **C** | Integration framework (actions + credential/secret store) + **email** integration end-to-end (actions only) | worker acts on a real service |
| **D** | Trigger Router (webhook ingress + pollers + subscriptions) + action approval checkpoint | **secretary reacts to incoming email** |
| **E** | Memory: private memory tools + shared wiki (FTS + versioned propose/approve) + console curation | knowledge substrate |
| **F** | Multi-worker coordination beyond channels: ephemeral delegation (agents-as-tools) + durable handoffs (task table), routed by worker `description` | the "workforce" |
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
- **Channel files.** Wanted: a shared folder per channel (see
  [Workspaces, Channels & DMs](#deferred-wanted)).
- **Cross-worker protocol (Phase F).** Channels (B2) already let workers hand work to each
  other in the open. Beyond that, leaning toward *both*: agents-as-tools for short,
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
| `axum` (+ `tower`, `tower-http`) | HTTP + WebSocket server | **approved** (`axum`; `tower`/`tower-http` only if needed) |
| `sqlx` (sqlite) | SQLite persistence + FTS5 | **in use** (chosen in A3) |
| ~~`rusqlite`~~ | alternative to `sqlx` | not chosen |
| AEAD crate (e.g. `chacha20poly1305`) | encrypt credentials at rest | pending approval |
| `tokio-cron-scheduler` or similar | pollers / scheduled triggers | pending approval |
| React + Vite + TypeScript | web console | **approved** (separate `console/` package) |
| `toml` | parse/write `worker.toml` | **approved** |

Already in the workspace and reused: `tokio`, `reqwest`, `serde`/`serde_json`, `clap`,
`anyhow`/`thiserror`, `async-trait`, `chrono`, `uuid`, `tracing`, `dirs`, `tokio-util`.
