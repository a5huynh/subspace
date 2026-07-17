//! The Worker Manager: lifecycle for all workers (spawn, deliver, list, shutdown).

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use subspace::agent::{Agent, AgentConfig};
use subspace::provider::Provider;
use subspace::tools::ToolRegistry;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::WorkerId;
use crate::event::ServerEvent;
use crate::worker::{Stimulus, WorkerConfig, WorkerStatus, run_worker};

const EVENT_BUS_CAPACITY: usize = 1024;
const MAILBOX_CAPACITY: usize = 64;

struct WorkerHandle {
    config: WorkerConfig,
    tx: mpsc::Sender<Stimulus>,
    status: Arc<Mutex<WorkerStatus>>,
    task: JoinHandle<()>,
}

/// A lightweight view of a worker for listing.
#[derive(Debug, Clone)]
pub struct WorkerSummary {
    pub id: WorkerId,
    pub name: String,
    pub status: WorkerStatus,
}

/// Owns and supervises the lifecycle of all workers.
pub struct WorkerManager {
    provider: Arc<dyn Provider>,
    events: broadcast::Sender<ServerEvent>,
    workers: Mutex<HashMap<WorkerId, WorkerHandle>>,
}

impl WorkerManager {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUS_CAPACITY);
        Self {
            provider,
            events,
            workers: Mutex::new(HashMap::new()),
        }
    }

    /// Subscribe to the live event stream across all workers.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    /// Create and start a worker. Returns its id.
    pub fn spawn(&self, config: WorkerConfig) -> WorkerId {
        let id = config.id.clone();

        let agent_config = AgentConfig {
            system_prompt: config.role.clone(),
            model: config.model.clone(),
            thinking: config.thinking,
            ..AgentConfig::default()
        };
        // Phase A workers have no tools yet — integrations arrive in Phase C.
        let agent = Agent::new(agent_config, self.provider.clone(), ToolRegistry::new());

        let (tx, mailbox) = mpsc::channel(MAILBOX_CAPACITY);
        let status = Arc::new(Mutex::new(WorkerStatus::Idle));
        let seq = Arc::new(AtomicU64::new(0));

        let task = tokio::spawn(run_worker(
            id.clone(),
            agent,
            mailbox,
            self.events.clone(),
            status.clone(),
            seq,
        ));

        let handle = WorkerHandle {
            config,
            tx,
            status,
            task,
        };
        self.workers.lock().unwrap().insert(id.clone(), handle);
        id
    }

    /// Deliver a stimulus to a worker's mailbox.
    pub async fn deliver(&self, id: &str, stimulus: Stimulus) -> Result<()> {
        // Clone the sender out of the lock so we don't hold the guard across await.
        let tx = {
            let workers = self.workers.lock().unwrap();
            workers.get(id).map(|h| h.tx.clone())
        };
        match tx {
            Some(tx) => tx
                .send(stimulus)
                .await
                .map_err(|_| anyhow!("worker '{id}' mailbox is closed")),
            None => Err(anyhow!("no such worker: {id}")),
        }
    }

    /// Convenience: send a user prompt to a worker.
    pub async fn prompt(&self, id: &str, text: impl Into<String>) -> Result<()> {
        self.deliver(id, Stimulus::UserPrompt { text: text.into() })
            .await
    }

    /// Current status of a worker, if it exists.
    pub fn status(&self, id: &str) -> Option<WorkerStatus> {
        let workers = self.workers.lock().unwrap();
        workers.get(id).map(|h| h.status.lock().unwrap().clone())
    }

    /// List all workers.
    pub fn list(&self) -> Vec<WorkerSummary> {
        let workers = self.workers.lock().unwrap();
        workers
            .values()
            .map(|h| WorkerSummary {
                id: h.config.id.clone(),
                name: h.config.name.clone(),
                status: h.status.lock().unwrap().clone(),
            })
            .collect()
    }

    /// Ask a worker to shut down, then remove it and await its task.
    pub async fn shutdown(&self, id: &str) -> Result<()> {
        self.deliver(id, Stimulus::Shutdown).await?;
        let handle = self.workers.lock().unwrap().remove(id);
        if let Some(h) = handle {
            let _ = h.task.await;
        }
        Ok(())
    }
}
