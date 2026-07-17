//! Worker configuration, status, stimuli, and the per-worker runtime loop.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use subspace::agent::Agent;
use subspace::types::{ThinkingLevel, new_id};
use tokio::sync::{broadcast, mpsc};

use crate::WorkerId;
use crate::event::{ServerEvent, WorkerEvent};

/// Static configuration for a worker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerConfig {
    pub id: WorkerId,
    pub name: String,
    /// The worker's role / persona — becomes the agent system prompt.
    pub role: String,
    pub model: String,
    pub thinking: ThinkingLevel,
}

impl WorkerConfig {
    /// Create a config with a fresh id and engine-default model/thinking level.
    pub fn new(name: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            role: role.into(),
            model: subspace::agent::AgentConfig::default().model,
            thinking: ThinkingLevel::Low,
        }
    }
}

/// Runtime status of a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    Idle,
    Running,
    Paused,
    Error(String),
}

/// Anything that can wake a worker. Prompts and events share one loop.
#[derive(Debug)]
pub enum Stimulus {
    /// A prompt from a human (via the console/API).
    UserPrompt { text: String },
    /// Ask the worker loop to stop.
    Shutdown,
}

/// The per-worker runtime loop. Consumes stimuli from the mailbox until it is
/// closed or a [`Stimulus::Shutdown`] arrives.
pub(crate) async fn run_worker(
    id: WorkerId,
    agent: Agent,
    mut mailbox: mpsc::Receiver<Stimulus>,
    events: broadcast::Sender<ServerEvent>,
    status: std::sync::Arc<Mutex<WorkerStatus>>,
    seq: std::sync::Arc<AtomicU64>,
) {
    while let Some(stimulus) = mailbox.recv().await {
        match stimulus {
            Stimulus::Shutdown => break,
            Stimulus::UserPrompt { text } => {
                set_status(&status, WorkerStatus::Running);
                emit(
                    &events,
                    &id,
                    &seq,
                    WorkerEvent::StatusChanged(WorkerStatus::Running),
                );
                emit(
                    &events,
                    &id,
                    &seq,
                    WorkerEvent::StimulusReceived(format!("user prompt ({} chars)", text.len())),
                );

                // Drive the engine agent loop and forward every event.
                let (mut rx, handle) = agent.prompt(&text).await;
                while let Some(ev) = rx.recv().await {
                    emit(&events, &id, &seq, WorkerEvent::Agent(ev));
                }
                let _ = handle.await;

                set_status(&status, WorkerStatus::Idle);
                emit(
                    &events,
                    &id,
                    &seq,
                    WorkerEvent::StatusChanged(WorkerStatus::Idle),
                );
            }
        }
    }
}

fn set_status(status: &Mutex<WorkerStatus>, next: WorkerStatus) {
    *status.lock().unwrap() = next;
}

fn emit(
    events: &broadcast::Sender<ServerEvent>,
    id: &WorkerId,
    seq: &AtomicU64,
    inner: WorkerEvent,
) {
    let ev = ServerEvent {
        worker_id: id.clone(),
        seq: seq.fetch_add(1, Ordering::SeqCst),
        ts: Utc::now(),
        inner,
    };
    // Ignore send errors: it just means no console clients are subscribed.
    let _ = events.send(ev);
}
