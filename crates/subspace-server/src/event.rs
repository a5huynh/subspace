//! Events emitted by workers, wrapped with attribution for the console.

use chrono::{DateTime, Utc};
use subspace::agent::AgentEvent;

use crate::WorkerId;
use crate::worker::WorkerStatus;

/// An event emitted by a worker, wrapped with attribution.
///
/// This is the payload the console will consume over WebSocket in Phase B.
#[derive(Debug, Clone)]
pub struct ServerEvent {
    pub worker_id: WorkerId,
    /// Monotonic per-worker sequence number (enables client resume/replay).
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub inner: WorkerEvent,
}

/// The kinds of events a worker produces.
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    /// The worker received a stimulus (summary string for now).
    StimulusReceived(String),
    /// A streamed event from the underlying engine agent loop.
    Agent(AgentEvent),
    /// The worker's status changed.
    StatusChanged(WorkerStatus),
}
