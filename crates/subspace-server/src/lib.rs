//! subspace-server — runs and supervises a fleet of long-lived Workers.
//!
//! Phase A: in-memory worker runtime and manager. Each worker owns an engine
//! [`subspace::agent::Agent`] driven from a mailbox of [`Stimulus`]. Events are
//! attributed per worker and streamed on a shared broadcast bus.

pub mod event;
pub mod home;
pub mod manager;
pub mod skills;
pub mod store;
pub mod worker;

/// Stable identifier for a worker (UUID-v4 string, from the engine).
pub type WorkerId = subspace::types::Id;

pub use event::{ServerEvent, WorkerEvent};
pub use home::{HomeListing, Homes};
pub use manager::{WorkerManager, WorkerSummary};
pub use store::{SqliteStore, WorkerIndexEntry};
pub use worker::{Stimulus, WorkerConfig, WorkerStatus};
