//! The Worker Manager: lifecycle for all workers (spawn, deliver, list, shutdown).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use subspace::agent::{Agent, AgentConfig};
use subspace::provider::Provider;
use subspace::tools::ToolRegistry;
use subspace::types::Message;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::WorkerId;
use crate::event::ServerEvent;
use crate::home::Homes;
use crate::home_tools::{HomeScope, home_tools};
use crate::skills::{self, Skill, SkillSource};
use crate::store::SqliteStore;
use crate::worker::{Stimulus, WorkerConfig, WorkerStatus, run_worker};

const EVENT_BUS_CAPACITY: usize = 1024;
const MAILBOX_CAPACITY: usize = 64;

struct WorkerHandle {
    config: WorkerConfig,
    tx: mpsc::Sender<Stimulus>,
    status: Arc<Mutex<WorkerStatus>>,
    task: JoinHandle<()>,
}

/// Where workers persist: identity in homes, index + history in SQLite.
struct Persistence {
    store: Arc<SqliteStore>,
    homes: Homes,
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
    persistence: Option<Persistence>,
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
        }
    }

    /// Subscribe to the live event stream across all workers.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
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
            self.start(cfg, history).await?;
            started += 1;
        }
        Ok(started)
    }

    /// Create and start a new worker. With persistence, its home is created
    /// first and then indexed.
    pub async fn spawn(&self, config: WorkerConfig) -> Result<WorkerId> {
        if self.workers.lock().unwrap().contains_key(&config.id) {
            return Err(anyhow!("worker '{}' is already running", config.id));
        }
        if let Some(p) = &self.persistence {
            let home = p.homes.create(&config)?;
            if let Err(e) = p.store.upsert_index(&config, &home).await {
                let _ = p.homes.remove(&config.id);
                return Err(e);
            }
        }
        self.start(config, Vec::new()).await
    }

    /// Build the engine agent and register the worker's runtime loop.
    async fn start(&self, config: WorkerConfig, history: Vec<Message>) -> Result<WorkerId> {
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
            return Err(anyhow!("worker '{id}' is already running"));
        }

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
            self.persistence.as_ref().map(|p| p.store.clone()),
        ));

        workers.insert(
            id.clone(),
            WorkerHandle {
                config,
                tx,
                status,
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

    /// List all in-memory (running) workers.
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

    /// Stop a worker's task and remove it from the in-memory set.
    ///
    /// The worker remains persisted and can be restored via [`Self::boot`].
    pub async fn shutdown(&self, id: &str) -> Result<()> {
        self.deliver(id, Stimulus::Shutdown).await?;
        let handle = self.workers.lock().unwrap().remove(id);
        if let Some(h) = handle {
            let _ = h.task.await;
        }
        Ok(())
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
            return Err(anyhow!("no such worker: {id}"));
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
