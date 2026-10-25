//! subspace-server — runs and supervises a fleet of long-lived Workers.
//!
//! Each worker owns an engine [`subspace::agent::Agent`] driven from a mailbox
//! of [`Stimulus`]. Identity lives in on-disk homes; the index and history live
//! in SQLite. Events are numbered and buffered per worker ([`event::EventLog`])
//! and published on a shared broadcast bus.

pub mod api;
pub mod bindings;
pub mod event;
pub mod home;
pub mod home_tools;
pub mod manager;
pub mod skills;
pub mod store;
pub mod worker;

/// Stable identifier for a worker (UUID-v4 string, from the engine).
pub type WorkerId = subspace::types::Id;

pub use event::{Notice, NoticeLevel, Replay, ServerEvent, WorkerEvent};
pub use home::{HomeListing, Homes};
pub use manager::{History, ManagerError, WorkerManager, WorkerSummary};
pub use store::{SqliteStore, WorkerIndexEntry};
pub use worker::{Stimulus, WorkerConfig, WorkerStatus};
