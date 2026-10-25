//! The Worker Manager: lifecycle for all workers (spawn, deliver, list,
//! update, shutdown) plus access to their history and event streams.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use serde::Serialize;
use subspace::agent::{Agent, AgentConfig};
use subspace::provider::Provider;
use subspace::tools::ToolRegistry;
use subspace::types::{Message, new_id};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use ts_rs::TS;

use crate::WorkerId;
use crate::event::{EventLog, Replay, ServerEvent};
use crate::home::Homes;
use crate::home_tools::{HomeScope, home_tools};
use crate::skills::{self, Skill, SkillSource};
use crate::store::SqliteStore;
use crate::worker::{Stimulus, WorkerConfig, WorkerStatus, run_worker};

const EVENT_BUS_CAPACITY: usize = 1024;
const MAILBOX_CAPACITY: usize = 64;

/// Errors callers may want to tell apart (e.g. to map onto HTTP statuses).
/// Returned wrapped in [`anyhow::Error`]; use `downcast_ref` to inspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerError {
    NotFound(WorkerId),
    AlreadyExists(WorkerId),
    /// The worker is mid-run; retry once it is idle.
    Busy(WorkerId),
}

impl fmt::Display for ManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "no such worker: {id}"),
            Self::AlreadyExists(id) => write!(f, "worker '{id}' already exists"),
            Self::Busy(id) => write!(f, "worker '{id}' is busy; try again when it is idle"),
        }
    }
}

impl std::error::Error for ManagerError {}

struct WorkerHandle {
    config: WorkerConfig,
    tx: mpsc::Sender<Stimulus>,
    status: Arc<Mutex<WorkerStatus>>,
    log: Arc<EventLog>,
    task: JoinHandle<()>,
}

/// Where workers persist: identity in homes, index + history in SQLite.
struct Persistence {
    store: Arc<SqliteStore>,
    homes: Homes,
}

/// A lightweight view of a worker for listing.
#[derive(Debug, Clone, Serialize, TS)]
pub struct WorkerSummary {
    pub id: WorkerId,
    pub name: String,
    pub description: String,
    pub status: WorkerStatus,
}

/// A worker's persisted messages plus the stream position to resume from.
#[derive(Debug, Clone, Serialize, TS)]
pub struct History {
    /// The manager's boot epoch; a stream resumed with a different epoch
    /// must resync.
    pub epoch: String,
    /// Resume the event stream with `since = cursor`. Taken *before* reading
    /// messages, so replayed events may repeat messages already listed here;
    /// clients merge by message id.
    pub cursor: u64,
    pub messages: Vec<Message>,
}

/// Owns and supervises the lifecycle of all workers.
pub struct WorkerManager {
    provider: Arc<dyn Provider>,
    events: broadcast::Sender<ServerEvent>,
    workers: Mutex<HashMap<WorkerId, WorkerHandle>>,
    persistence: Option<Persistence>,
    /// Random per-process id; event `seq`s are only comparable within one.
    epoch: String,
}

impl WorkerManager {
    /// Create an in-memory manager with no persistence.
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self::build(provider, None)
    }

    /// Create a manager that keeps worker identity in `homes` and the worker
    /// index + history in `store`.
    pub fn with_persistence(
        provider: Arc<dyn Provider>,
        store: Arc<SqliteStore>,
        homes: Homes,
    ) -> Self {
        Self::build(provider, Some(Persistence { store, homes }))
    }

    fn build(provider: Arc<dyn Provider>, persistence: Option<Persistence>) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUS_CAPACITY);
        Self {
            provider,
            events,
            workers: Mutex::new(HashMap::new()),
            persistence,
            epoch: new_id(),
        }
    }

    /// This process's epoch (see [`History::epoch`]).
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Subscribe to the live event stream across all workers.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    /// Subscribe to one worker's events after `since`: a replay of buffered
    /// events (or a [`Replay::Gap`]) plus a live receiver. The receiver
    /// carries all workers' events; filter by `worker_id`.
    pub fn subscribe_worker(
        &self,
        id: &str,
        since: u64,
    ) -> Result<(Replay, broadcast::Receiver<ServerEvent>)> {
        Ok(self.log(id)?.subscribe(since))
    }

    /// Rebuild the index from homes and start every worker found there.
    /// Returns how many workers were started.
    ///
    /// A3-era workers still stored in SQLite are migrated into homes first.
    /// Broken homes are logged and skipped. Index entries whose home is gone
    /// are dropped (their history is kept). Already-running workers are left
    /// alone, so calling this twice is harmless.
    pub async fn boot(&self) -> Result<usize> {
        let Some(p) = &self.persistence else {
            return Ok(0);
        };

        migrate_legacy_workers(p).await?;

        let listing = p.homes.list()?;
        for (id, err) in &listing.invalid {
            tracing::warn!("skipping worker home '{id}': {err:#}");
        }

        let on_disk: HashSet<&str> = listing
            .workers
            .iter()
            .map(|w| w.id.as_str())
            .chain(listing.invalid.iter().map(|(id, _)| id.as_str()))
            .collect();
        for entry in p.store.list_index().await? {
            if !on_disk.contains(entry.id.as_str()) {
                tracing::warn!(
                    "worker '{}' ({}) has no home at {}; dropping it from the index",
                    entry.id,
                    entry.name,
                    entry.home_path
                );
                p.store.remove_index(&entry.id).await?;
            }
        }

        let mut started = 0;
        for cfg in listing.workers {
            if self.workers.lock().unwrap().contains_key(&cfg.id) {
                continue;
            }
            p.store.upsert_index(&cfg, &p.homes.path(&cfg.id)?).await?;
            let history = p.store.load_history(&cfg.id).await?;
            self.start(cfg, history, None).await?;
            started += 1;
        }
        Ok(started)
    }

    /// Create and start a new worker. With persistence, its home is created
    /// first and then indexed.
    pub async fn spawn(&self, config: WorkerConfig) -> Result<WorkerId> {
        let exists = self.workers.lock().unwrap().contains_key(&config.id)
            || match &self.persistence {
                Some(p) => p.homes.exists(&config.id)?,
                None => false,
            };
        if exists {
            return Err(ManagerError::AlreadyExists(config.id).into());
        }
        if let Some(p) = &self.persistence {
            let home = p.homes.create(&config)?;
            if let Err(e) = p.store.upsert_index(&config, &home).await {
                let _ = p.homes.remove(&config.id);
                return Err(e);
            }
        }
        self.start(config, Vec::new(), None).await
    }

    /// Build the engine agent and register the worker's runtime loop. Pass the
    /// worker's previous `log` when restarting so `seq` keeps increasing.
    async fn start(
        &self,
        config: WorkerConfig,
        history: Vec<Message>,
        log: Option<Arc<EventLog>>,
    ) -> Result<WorkerId> {
        let id = config.id.clone();

        let (system_prompt, tools) = self.prepare(&config);
        let agent_config = AgentConfig {
            system_prompt,
            model: config.model.clone(),
            thinking: config.thinking,
            ..AgentConfig::default()
        };
        let agent = Agent::new(agent_config, self.provider.clone(), tools);
        if !history.is_empty() {
            agent.replace_messages(history).await;
        }

        // Check and register under one lock so a duplicate id can never leave
        // an orphaned task running.
        let mut workers = self.workers.lock().unwrap();
        if workers.contains_key(&id) {
            return Err(ManagerError::AlreadyExists(id).into());
        }

        let (tx, mailbox) = mpsc::channel(MAILBOX_CAPACITY);
        let status = Arc::new(Mutex::new(WorkerStatus::Idle));
        let log = log.unwrap_or_else(|| Arc::new(EventLog::new(id.clone(), self.events.clone())));

        let task = tokio::spawn(run_worker(
            agent,
            mailbox,
            log.clone(),
            status.clone(),
            self.persistence.as_ref().map(|p| p.store.clone()),
            id.clone(),
        ));

        workers.insert(
            id.clone(),
            WorkerHandle {
                config,
                tx,
                status,
                log,
                task,
            },
        );
        Ok(id)
    }

    /// The worker's system prompt (role + resolved skills) and tool registry
    /// (home-scoped file tools). Both need persistence, since they live in homes.
    fn prepare(&self, config: &WorkerConfig) -> (String, ToolRegistry) {
        let mut tools = ToolRegistry::new();
        let Some(p) = &self.persistence else {
            return (config.role.clone(), tools);
        };

        let skills = resolve_skills(&p.homes, config).unwrap_or_else(|e| {
            tracing::warn!("worker '{}': skills unavailable: {e:#}", config.id);
            Vec::new()
        });

        // The offered skills' real dirs are readable, so the <skills> locations
        // always resolve even when skills are symlinked from elsewhere.
        let skill_dirs = skills
            .iter()
            .filter_map(|s| s.path.parent().map(Path::to_path_buf));
        let scope = p
            .homes
            .path(&config.id)
            .and_then(|home| HomeScope::new(&home, p.homes.global_skills_dir(), skill_dirs));
        match scope {
            Ok(scope) => {
                for tool in home_tools(Arc::new(scope)) {
                    tools.register(tool);
                }
            }
            Err(e) => tracing::warn!("worker '{}': home tools unavailable: {e:#}", config.id),
        }

        (skills::compose_system_prompt(&config.role, &skills), tools)
    }

    fn log(&self, id: &str) -> Result<Arc<EventLog>> {
        let workers = self.workers.lock().unwrap();
        workers
            .get(id)
            .map(|h| h.log.clone())
            .ok_or_else(|| ManagerError::NotFound(id.to_string()).into())
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
            None => Err(ManagerError::NotFound(id.to_string()).into()),
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

    /// A running worker's current config.
    pub fn get(&self, id: &str) -> Option<WorkerConfig> {
        let workers = self.workers.lock().unwrap();
        workers.get(id).map(|h| h.config.clone())
    }

    /// List all in-memory (running) workers, sorted by name.
    pub fn list(&self) -> Vec<WorkerSummary> {
        let workers = self.workers.lock().unwrap();
        let mut out: Vec<_> = workers
            .values()
            .map(|h| WorkerSummary {
                id: h.config.id.clone(),
                name: h.config.name.clone(),
                description: h.config.description.clone(),
                status: h.status.lock().unwrap().clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// A worker's persisted messages and the cursor to resume its stream from.
    /// In-memory workers have no persisted messages.
    pub async fn history(&self, id: &str) -> Result<History> {
        // Cursor first: anything that lands in between is replayed (and
        // deduplicated by id) rather than lost.
        let cursor = self.log(id)?.cursor();
        let messages = match &self.persistence {
            Some(p) => p.store.load_history(id).await?,
            None => Vec::new(),
        };
        Ok(History {
            epoch: self.epoch.clone(),
            cursor,
            messages,
        })
    }

    /// Replace an idle worker's config: save it to the home, refresh the
    /// index, and restart the worker (keeping history and its event log) so
    /// the change applies to its next session. The id cannot change.
    pub async fn update(&self, config: WorkerConfig) -> Result<()> {
        let id = config.id.clone();
        match self.status(&id) {
            None => return Err(ManagerError::NotFound(id).into()),
            Some(WorkerStatus::Running) => return Err(ManagerError::Busy(id).into()),
            Some(_) => {}
        }
        if let Some(p) = &self.persistence {
            p.homes.save(&config)?;
            p.store.upsert_index(&config, &p.homes.path(&id)?).await?;
        }
        let log = self.stop(&id).await?;
        let history = match &self.persistence {
            Some(p) => p.store.load_history(&id).await?,
            None => Vec::new(),
        };
        self.start(config, history, Some(log)).await?;
        Ok(())
    }

    /// Stop a worker's task and remove it from the in-memory set, returning
    /// its event log. Queued stimuli are processed before it stops.
    async fn stop(&self, id: &str) -> Result<Arc<EventLog>> {
        self.deliver(id, Stimulus::Shutdown).await?;
        let handle = self
            .workers
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        let _ = handle.task.await;
        Ok(handle.log)
    }

    /// Stop a worker's task and remove it from the in-memory set.
    ///
    /// The worker remains persisted and can be restored via [`Self::boot`].
    pub async fn shutdown(&self, id: &str) -> Result<()> {
        self.stop(id).await.map(|_| ())
    }

    /// Permanently delete a worker: stop it, drop its index entry and history,
    /// then delete its home. Database first, so a failure mid-way leaves only
    /// an orphaned (harmless) home directory.
    pub async fn remove(&self, id: &str) -> Result<()> {
        let running = self.workers.lock().unwrap().contains_key(id);
        let has_home = match &self.persistence {
            Some(p) => p.homes.exists(id)?,
            None => false,
        };
        if !running && !has_home {
            return Err(ManagerError::NotFound(id.to_string()).into());
        }

        if running {
            self.shutdown(id).await?;
        }
        if let Some(p) = &self.persistence {
            p.store.delete_worker(id).await?;
            p.homes.remove(id)?;
        }
        Ok(())
    }
}

/// Move A3-era workers (config in SQLite) into homes. Returns how many moved.
///
/// The home is created before the DB is touched, and the DB side is one
/// transaction, so a crash at any point is recovered by the next boot: an
/// existing home is kept as the source of truth and only the DB is finalized.
/// Rows that can't be converted are logged and left in place.
async fn migrate_legacy_workers(p: &Persistence) -> Result<usize> {
    let mut migrated = 0;
    for legacy in p.store.list_legacy_workers().await? {
        let home = match p.homes.path(&legacy.id) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("not migrating legacy worker: {e:#}");
                continue;
            }
        };
        // An existing home means a previous migration didn't finish its DB step;
        // the home wins over the legacy row.
        let home_exists = p.homes.exists(&legacy.id)?;
        let from_home = if home_exists {
            p.homes.load(&legacy.id).ok()
        } else {
            None
        };
        let cfg = match from_home.map_or_else(|| legacy.to_config(), Ok) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("not migrating legacy worker '{}': {e:#}", legacy.id);
                continue;
            }
        };
        if !home_exists {
            p.homes.create(&cfg)?;
        }
        p.store
            .complete_legacy_migration(&cfg, &home, &legacy.created_at)
            .await?;
        tracing::info!(
            "migrated worker '{}' ({}) into {}",
            cfg.id,
            cfg.name,
            home.display()
        );
        migrated += 1;
    }
    Ok(migrated)
}

/// Discover a worker's skills (its own, then global), logging broken ones.
fn resolve_skills(homes: &Homes, config: &WorkerConfig) -> Result<Vec<Skill>> {
    let worker = skills::discover(&homes.skills_dir(&config.id)?, SkillSource::Worker)?;
    let global = skills::discover(homes.global_skills_dir(), SkillSource::Global)?;
    for (path, err) in worker.invalid.iter().chain(&global.invalid) {
        tracing::warn!("skipping skill {}: {err:#}", path.display());
    }
    Ok(skills::resolve(worker, global, &config.skills))
}
