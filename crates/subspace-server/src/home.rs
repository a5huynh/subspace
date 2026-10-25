//! Worker Homes: the on-disk source of truth for a worker's identity.
//!
//! ```text
//! <data_dir>/workers/<worker_id>/
//! ├── worker.toml   # identity config (name, description, model, thinking)
//! ├── ROLE.md       # persona → system prompt
//! ├── skills/       # worker-specific skills
//! └── memory/       # private memory notes
//! ```
//!
//! Homes are keyed by id, never by name, so renames only touch `worker.toml`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use subspace::types::ThinkingLevel;

use crate::worker::WorkerConfig;

pub const WORKERS_DIR: &str = "workers";
pub const MANIFEST_FILE: &str = "worker.toml";
pub const ROLE_FILE: &str = "ROLE.md";
pub const SKILLS_DIR: &str = "skills";
pub const MEMORY_DIR: &str = "memory";

/// Prefix for in-progress homes; never listed as workers.
const STAGING_PREFIX: &str = ".staging-";

/// The `worker.toml` schema. `role` lives in `ROLE.md`, not here.
#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    model: String,
    #[serde(default)]
    thinking: ThinkingLevel,
}

/// Result of scanning all homes.
#[derive(Debug, Default)]
pub struct HomeListing {
    /// Successfully loaded workers, sorted by id.
    pub workers: Vec<WorkerConfig>,
    /// Homes that have a manifest but failed to load, with the reason.
    pub invalid: Vec<(String, anyhow::Error)>,
}

/// Handle to the directory containing all worker homes.
#[derive(Debug, Clone)]
pub struct Homes {
    root: PathBuf,
}

impl Homes {
    /// Homes under `<data_dir>/workers/`. The directory is created if missing.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let root = data_dir.as_ref().join(WORKERS_DIR);
        fs::create_dir_all(&root)
            .with_context(|| format!("creating workers dir {}", root.display()))?;
        Ok(Self { root })
    }

    /// Path of a worker's home (whether or not it exists).
    pub fn path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(id))
    }

    pub fn exists(&self, id: &str) -> Result<bool> {
        Ok(self.path(id)?.join(MANIFEST_FILE).is_file())
    }

    /// Create a new home. Fails if one already exists for this id.
    ///
    /// The home is assembled in a staging dir and renamed into place, so a
    /// partially written home is never visible to [`Self::list`].
    pub fn create(&self, cfg: &WorkerConfig) -> Result<PathBuf> {
        let dest = self.path(&cfg.id)?;
        if dest.exists() {
            bail!("worker home already exists: {}", dest.display());
        }

        let staging = self.root.join(format!("{STAGING_PREFIX}{}", cfg.id));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        let result = (|| {
            fs::create_dir_all(staging.join(SKILLS_DIR))?;
            fs::create_dir_all(staging.join(MEMORY_DIR))?;
            write_files(&staging, cfg)?;
            fs::rename(&staging, &dest)?;
            Ok(dest.clone())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Load a worker's config from its home.
    pub fn load(&self, id: &str) -> Result<WorkerConfig> {
        let dir = self.path(id)?;
        let manifest_path = dir.join(MANIFEST_FILE);
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let m: Manifest =
            toml::from_str(&raw).with_context(|| format!("parsing {}", manifest_path.display()))?;
        if m.id != id {
            bail!(
                "{} declares id '{}' but lives in home '{id}'",
                manifest_path.display(),
                m.id
            );
        }
        let role = match fs::read_to_string(dir.join(ROLE_FILE)) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).context("reading ROLE.md"),
        };
        Ok(WorkerConfig {
            id: m.id,
            name: m.name,
            description: m.description,
            role,
            model: m.model,
            thinking: m.thinking,
        })
    }

    /// Overwrite an existing home's `worker.toml` and `ROLE.md`.
    pub fn save(&self, cfg: &WorkerConfig) -> Result<()> {
        let dir = self.path(&cfg.id)?;
        if !dir.join(MANIFEST_FILE).is_file() {
            bail!("no worker home for '{}'", cfg.id);
        }
        write_files(&dir, cfg)
    }

    /// Load every home. Broken homes are reported in [`HomeListing::invalid`]
    /// rather than failing the whole scan, so one bad edit can't block boot.
    pub fn list(&self) -> Result<HomeListing> {
        let mut listing = HomeListing::default();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || validate_id(&name).is_err() {
                continue;
            }
            // A directory without a manifest isn't a worker (e.g. leftover files).
            if !entry.path().join(MANIFEST_FILE).is_file() {
                continue;
            }
            match self.load(&name) {
                Ok(cfg) => listing.workers.push(cfg),
                Err(e) => listing.invalid.push((name, e)),
            }
        }
        listing.workers.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(listing)
    }

    /// Delete a worker's home and everything in it.
    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.path(id)?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("removing {}", dir.display())),
        }
    }
}

fn write_files(dir: &Path, cfg: &WorkerConfig) -> Result<()> {
    let manifest = Manifest {
        id: cfg.id.clone(),
        name: cfg.name.clone(),
        description: cfg.description.clone(),
        model: cfg.model.clone(),
        thinking: cfg.thinking,
    };
    write_atomic(
        &dir.join(MANIFEST_FILE),
        &toml::to_string_pretty(&manifest)?,
    )?;
    write_atomic(&dir.join(ROLE_FILE), &cfg.role)
}

/// Write via a sibling temp file + rename so readers never see a torn file.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("invalid path {}", path.display()))?;
    let tmp = path.with_file_name(format!(".{}.tmp", file_name.to_string_lossy()));
    fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Ids become path segments, so only allow `[A-Za-z0-9_-]`.
fn validate_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(anyhow!("invalid worker id: {id:?}"))
    }
}
