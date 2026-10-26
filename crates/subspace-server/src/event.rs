//! Events emitted by workers and channels, wrapped with attribution for the
//! console, plus the [`EventLog`] that numbers, buffers, and publishes them.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use subspace::agent::AgentEvent;
use subspace::types::Message;
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::WorkerId;
use crate::channel::{ChannelId, ChannelPost, Routing};
use crate::worker::WorkerStatus;

/// How many recent events each worker keeps for stream replay.
pub const EVENT_BUFFER_CAPACITY: usize = 4096;

/// An event emitted by a worker, wrapped with attribution. This is the
/// payload streamed to console clients.
#[derive(Debug, Clone, Serialize, TS)]
pub struct ServerEvent {
    pub worker_id: WorkerId,
    /// Monotonic per-worker sequence number, starting at 1. Resets only when
    /// the server restarts, which changes the manager's epoch.
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub inner: WorkerEvent,
}

/// The kinds of events a worker produces.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkerEvent {
    /// The worker received a stimulus (human-readable summary).
    StimulusReceived(String),
    /// A user message was accepted and persisted; carries its stable id so
    /// clients can merge it with fetched history.
    UserMessage(Message),
    /// A streamed event from the underlying engine agent loop.
    Agent(AgentEvent),
    /// A run error or warning, persisted so it survives reloads. Carries a
    /// stable id so clients can merge it with fetched history.
    Notice(Notice),
    /// The worker's status changed.
    StatusChanged(WorkerStatus),
}

/// How serious a [`Notice`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Warning,
    Error,
}

/// An error or warning from a worker run (e.g. a provider failure). Shown to
/// humans alongside the conversation, but never sent to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Notice {
    pub id: String,
    pub level: NoticeLevel,
    pub text: String,
    pub timestamp: DateTime<Utc>,
}

impl Notice {
    pub fn new(level: NoticeLevel, text: impl Into<String>) -> Self {
        Self {
            id: subspace::types::new_id(),
            level,
            text: text.into(),
            timestamp: Utc::now(),
        }
    }
}

/// An event in a channel's log: posts, notices, and the live activity of
/// workers taking a turn there.
#[derive(Debug, Clone, Serialize, TS)]
pub struct ChannelEvent {
    pub channel_id: ChannelId,
    /// Monotonic per-channel sequence number, starting at 1 (per epoch).
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub inner: ChannelEventKind,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ChannelEventKind {
    /// A post was persisted (stable id; merge with fetched posts).
    Post(ChannelPost),
    /// An operator post's routing was decided (or failed).
    Routed { post_id: String, routing: Routing },
    /// A persisted notice: a failed turn, the hop limit, an undeliverable wake.
    Notice(Notice),
    /// Live activity from a worker taking its turn in this channel.
    Worker {
        worker_id: WorkerId,
        event: WorkerEvent,
    },
    /// Name, topic, or membership changed; refetch the channel.
    Changed,
    /// The channel was deleted.
    Deleted,
}

/// An event type an [`EventLog`] can number, buffer, and publish.
pub trait LogEvent: Clone + Send + Sync + 'static {
    type Inner;

    fn wrap(source: &str, seq: u64, inner: Self::Inner) -> Self;
    fn seq(&self) -> u64;
    /// Whether this event starts (`true`) or ends (`false`) a run, keyed so
    /// a log can track several concurrent runs.
    fn run_edge(inner: &Self::Inner) -> Option<(String, bool)>;
}

impl LogEvent for ServerEvent {
    type Inner = WorkerEvent;

    fn wrap(source: &str, seq: u64, inner: WorkerEvent) -> Self {
        Self {
            worker_id: source.to_string(),
            seq,
            ts: Utc::now(),
            inner,
        }
    }

    fn seq(&self) -> u64 {
        self.seq
    }

    fn run_edge(inner: &WorkerEvent) -> Option<(String, bool)> {
        match inner {
            WorkerEvent::StatusChanged(status) => {
                Some((String::new(), *status == WorkerStatus::Running))
            }
            _ => None,
        }
    }
}

impl LogEvent for ChannelEvent {
    type Inner = ChannelEventKind;

    fn wrap(source: &str, seq: u64, inner: ChannelEventKind) -> Self {
        Self {
            channel_id: source.to_string(),
            seq,
            ts: Utc::now(),
            inner,
        }
    }

    fn seq(&self) -> u64 {
        self.seq
    }

    fn run_edge(inner: &ChannelEventKind) -> Option<(String, bool)> {
        match inner {
            ChannelEventKind::Worker {
                worker_id,
                event: WorkerEvent::StatusChanged(status),
            } => Some((worker_id.clone(), *status == WorkerStatus::Running)),
            _ => None,
        }
    }
}

/// What a new stream subscriber should do before consuming live events.
pub enum Replay<E = ServerEvent> {
    /// Send these buffered events (all with `seq > since`), then go live.
    Events(Vec<E>),
    /// Events after `since` are no longer buffered; the client must refetch
    /// history and resubscribe from a fresh cursor.
    Gap,
}

struct LogState<E> {
    next_seq: u64,
    buffer: VecDeque<E>,
    /// Runs in progress, each with the cursor from just before it started.
    runs: HashMap<String, u64>,
}

/// Numbers, buffers, and publishes one source's events (a worker's or a
/// channel's). Kept across worker restarts so `seq` never goes backwards
/// within an epoch.
pub struct EventLog<E: LogEvent = ServerEvent> {
    source: String,
    bus: broadcast::Sender<E>,
    state: Mutex<LogState<E>>,
}

/// A channel's event log.
pub type ChannelLog = EventLog<ChannelEvent>;

impl<E: LogEvent> EventLog<E> {
    pub fn new(source: impl Into<String>, bus: broadcast::Sender<E>) -> Self {
        Self {
            source: source.into(),
            bus,
            state: Mutex::new(LogState {
                next_seq: 1,
                buffer: VecDeque::new(),
                runs: HashMap::new(),
            }),
        }
    }

    /// Assign the next `seq`, buffer the event, and publish it. Holding the
    /// lock across publish keeps `subscribe` gap-free.
    pub fn emit(&self, inner: E::Inner) {
        let mut state = self.state.lock().unwrap();
        match E::run_edge(&inner) {
            Some((key, true)) => {
                let before = state.next_seq - 1;
                state.runs.entry(key).or_insert(before);
            }
            Some((key, false)) => {
                state.runs.remove(&key);
            }
            None => {}
        }
        let ev = E::wrap(&self.source, state.next_seq, inner);
        state.next_seq += 1;
        state.buffer.push_back(ev.clone());
        if state.buffer.len() > EVENT_BUFFER_CAPACITY {
            state.buffer.pop_front();
        }
        // No receivers just means no console is watching.
        let _ = self.bus.send(ev);
    }

    /// The `seq` a client should resume from after fetching history: the
    /// latest event, or (mid-run) just before the earliest run in progress
    /// started, so in-progress runs are replayed in full.
    pub fn cursor(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state
            .runs
            .values()
            .min()
            .copied()
            .unwrap_or(state.next_seq - 1)
    }

    /// Atomically snapshot events after `since` and subscribe to live ones.
    /// The receiver carries every source's events; callers filter by id.
    pub fn subscribe(&self, since: u64) -> (Replay<E>, broadcast::Receiver<E>) {
        let state = self.state.lock().unwrap();
        let rx = self.bus.subscribe();
        let last = state.next_seq - 1;
        let oldest = state.buffer.front().map_or(state.next_seq, |e| e.seq());
        let replay = if since > last {
            // A cursor from the future can't belong to this log.
            Replay::Gap
        } else if since + 1 < oldest {
            Replay::Gap
        } else {
            Replay::Events(
                state
                    .buffer
                    .iter()
                    .filter(|e| e.seq() > since)
                    .cloned()
                    .collect(),
            )
        };
        (replay, rx)
    }
}
