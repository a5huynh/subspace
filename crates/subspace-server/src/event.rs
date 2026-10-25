//! Events emitted by workers, wrapped with attribution for the console, plus
//! the per-worker [`EventLog`] that numbers, buffers, and publishes them.

use std::collections::VecDeque;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;
use subspace::agent::AgentEvent;
use subspace::types::Message;
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::WorkerId;
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
    /// The worker's status changed.
    StatusChanged(WorkerStatus),
}

/// What a new stream subscriber should do before consuming live events.
pub enum Replay {
    /// Send these buffered events (all with `seq > since`), then go live.
    Events(Vec<ServerEvent>),
    /// Events after `since` are no longer buffered; the client must refetch
    /// history and resubscribe from a fresh cursor.
    Gap,
}

struct LogState {
    next_seq: u64,
    buffer: VecDeque<ServerEvent>,
    /// While a run is in progress, the cursor from just before it started.
    run_start: Option<u64>,
}

/// Numbers, buffers, and publishes one worker's events. Kept across worker
/// restarts so `seq` never goes backwards within an epoch.
pub struct EventLog {
    worker_id: WorkerId,
    bus: broadcast::Sender<ServerEvent>,
    state: Mutex<LogState>,
}

impl EventLog {
    pub fn new(worker_id: WorkerId, bus: broadcast::Sender<ServerEvent>) -> Self {
        Self {
            worker_id,
            bus,
            state: Mutex::new(LogState {
                next_seq: 1,
                buffer: VecDeque::new(),
                run_start: None,
            }),
        }
    }

    /// Assign the next `seq`, buffer the event, and publish it. Holding the
    /// lock across publish keeps `subscribe` gap-free.
    pub fn emit(&self, inner: WorkerEvent) {
        let mut state = self.state.lock().unwrap();
        match &inner {
            WorkerEvent::StatusChanged(WorkerStatus::Running) if state.run_start.is_none() => {
                state.run_start = Some(state.next_seq - 1);
            }
            WorkerEvent::StatusChanged(WorkerStatus::Running) => {}
            WorkerEvent::StatusChanged(_) => state.run_start = None,
            _ => {}
        }
        let ev = ServerEvent {
            worker_id: self.worker_id.clone(),
            seq: state.next_seq,
            ts: Utc::now(),
            inner,
        };
        state.next_seq += 1;
        state.buffer.push_back(ev.clone());
        if state.buffer.len() > EVENT_BUFFER_CAPACITY {
            state.buffer.pop_front();
        }
        // No receivers just means no console is watching.
        let _ = self.bus.send(ev);
    }

    /// The `seq` a client should resume from after fetching history: the
    /// latest event, or (mid-run) just before the current run started, so the
    /// in-progress run is replayed in full.
    pub fn cursor(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state.run_start.unwrap_or(state.next_seq - 1)
    }

    /// Atomically snapshot events after `since` and subscribe to live ones.
    /// The receiver carries every worker's events; callers filter by id.
    pub fn subscribe(&self, since: u64) -> (Replay, broadcast::Receiver<ServerEvent>) {
        let state = self.state.lock().unwrap();
        let rx = self.bus.subscribe();
        let last = state.next_seq - 1;
        let oldest = state.buffer.front().map_or(state.next_seq, |e| e.seq);
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
                    .filter(|e| e.seq > since)
                    .cloned()
                    .collect(),
            )
        };
        (replay, rx)
    }
}
