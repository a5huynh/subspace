//! Worker configuration, status, stimuli, and the per-worker runtime loop.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use subspace::agent::{Agent, AgentEvent};
use subspace::types::{Message, ThinkingLevel, new_id};
use tokio::sync::mpsc;
use ts_rs::TS;

use crate::WorkerId;
use crate::event::{EventLog, Notice, NoticeLevel, WorkerEvent};
use crate::skills::SkillSelection;
use crate::store::SqliteStore;

/// Static configuration for a worker.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct WorkerConfig {
    pub id: WorkerId,
    pub name: String,
    /// One line describing what this worker is good at (used for routing).
    #[serde(default)]
    pub description: String,
    /// The worker's role / persona — becomes the agent system prompt.
    pub role: String,
    pub model: String,
    pub thinking: ThinkingLevel,
    /// Which discovered skills are enabled (default: all).
    #[serde(default)]
    pub skills: SkillSelection,
}

impl WorkerConfig {
    /// Create a config with a fresh id and engine-default model/thinking level.
    pub fn new(name: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            description: String::new(),
            role: role.into(),
            model: subspace::agent::AgentConfig::default().model,
            thinking: ThinkingLevel::Low,
            skills: SkillSelection::default(),
        }
    }
}

/// Runtime status of a worker. Serialized as `{"state": "idle"}` or
/// `{"state": "error", "message": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "state", content = "message", rename_all = "snake_case")]
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
    agent: Agent,
    mut mailbox: mpsc::Receiver<Stimulus>,
    log: Arc<EventLog>,
    status: Arc<Mutex<WorkerStatus>>,
    store: Option<Arc<SqliteStore>>,
    id: WorkerId,
) {
    while let Some(stimulus) = mailbox.recv().await {
        match stimulus {
            Stimulus::Shutdown => break,
            Stimulus::UserPrompt { text } => {
                set_status(&status, &log, WorkerStatus::Running);
                log.emit(WorkerEvent::StimulusReceived(format!(
                    "user prompt ({} chars)",
                    text.len()
                )));

                // Persist the user turn before running, then publish it with
                // its stable id so clients can merge it with fetched history.
                let user = Message::user(&text);
                if let Some(store) = &store
                    && let Err(e) = store
                        .append_messages(&id, std::slice::from_ref(&user))
                        .await
                {
                    tracing::warn!("failed to persist user message: {e}");
                }
                log.emit(WorkerEvent::UserMessage(user));

                // Drive the engine agent loop, forwarding events and persisting
                // the produced messages when the run completes.
                let (mut rx, handle) = agent.prompt(&text).await;
                while let Some(ev) = rx.recv().await {
                    if let AgentEvent::AgentEnd { messages, .. } = &ev
                        && let Some(store) = &store
                        && let Err(e) = store.append_messages(&id, messages).await
                    {
                        tracing::warn!("failed to persist agent messages: {e}");
                    }
                    let notice = match &ev {
                        AgentEvent::Error(text) => Some(Notice::new(NoticeLevel::Error, text)),
                        AgentEvent::Warning(text) => Some(Notice::new(NoticeLevel::Warning, text)),
                        _ => None,
                    };
                    log.emit(WorkerEvent::Agent(ev));
                    if let Some(notice) = notice {
                        // Persisted so a reload still explains a failed run.
                        if let Some(store) = &store
                            && let Err(e) = store.append_notice(&id, &notice).await
                        {
                            tracing::warn!("failed to persist notice: {e}");
                        }
                        log.emit(WorkerEvent::Notice(notice));
                    }
                }
                let _ = handle.await;

                set_status(&status, &log, WorkerStatus::Idle);
            }
        }
    }
}

fn set_status(status: &Mutex<WorkerStatus>, log: &EventLog, next: WorkerStatus) {
    *status.lock().unwrap() = next.clone();
    log.emit(WorkerEvent::StatusChanged(next));
}
