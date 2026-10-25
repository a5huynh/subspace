//! SQLite persistence for the worker index and message history.
//!
//! Worker identity lives in on-disk homes ([`crate::home`]); the `workers`
//! table is only an index of them.

use std::path::Path;
use std::str::FromStr;

use anyhow::Result;
use chrono::Utc;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use subspace::types::Message;

use crate::event::Notice;
use crate::worker::WorkerConfig;

/// One row of the worker index.
#[derive(Debug, Clone)]
pub struct WorkerIndexEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub home_path: String,
    pub created_at: String,
}

/// An A3-era worker row (config stored in SQLite) awaiting migration to a home.
#[derive(Debug, Clone)]
pub struct LegacyWorker {
    pub id: String,
    pub name: String,
    pub role: String,
    pub model: String,
    /// JSON-encoded [`subspace::types::ThinkingLevel`], as A3 stored it.
    pub thinking: String,
    pub created_at: String,
}

impl LegacyWorker {
    pub fn to_config(&self) -> Result<WorkerConfig> {
        Ok(WorkerConfig {
            id: self.id.clone(),
            name: self.name.clone(),
            description: String::new(),
            role: self.role.clone(),
            model: self.model.clone(),
            thinking: serde_json::from_str(&self.thinking)?,
            skills: Default::default(),
        })
    }
}

/// SQLite-backed store. Cheap to clone (the pool is reference-counted).
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Connect (creating the file if needed) and run migrations.
    ///
    /// Use `"sqlite::memory:"` for an ephemeral in-memory database (tests).
    pub async fn connect(url: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        // In-memory databases live per-connection, so a single connection keeps
        // the schema and data consistent across queries.
        let in_memory = url.contains(":memory:");
        let pool = SqlitePoolOptions::new()
            .max_connections(if in_memory { 1 } else { 5 })
            .connect_with(opts)
            .await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Self { pool })
    }

    /// Insert or refresh a worker's index entry from its home config.
    /// `created_at` is set on first insert and preserved afterwards.
    pub async fn upsert_index(&self, cfg: &WorkerConfig, home_path: &Path) -> Result<()> {
        sqlx::query(
            "INSERT INTO workers (id, name, description, home_path, created_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 description = excluded.description,
                 home_path = excluded.home_path",
        )
        .bind(&cfg.id)
        .bind(&cfg.name)
        .bind(&cfg.description)
        .bind(home_path.to_string_lossy().as_ref())
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// All index entries, oldest first.
    pub async fn list_index(&self) -> Result<Vec<WorkerIndexEntry>> {
        let rows = sqlx::query(
            "SELECT id, name, description, home_path, created_at FROM workers
             ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| WorkerIndexEntry {
                id: row.get("id"),
                name: row.get("name"),
                description: row.get("description"),
                home_path: row.get("home_path"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    /// Drop a worker's index entry only; its history is kept.
    pub async fn remove_index(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM workers WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// A3 worker rows not yet migrated into homes, oldest first.
    pub async fn list_legacy_workers(&self) -> Result<Vec<LegacyWorker>> {
        let rows = sqlx::query(
            "SELECT id, name, role, model, thinking, created_at FROM legacy_workers
             ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| LegacyWorker {
                id: row.get("id"),
                name: row.get("name"),
                role: row.get("role"),
                model: row.get("model"),
                thinking: row.get("thinking"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    /// Finish migrating a legacy worker whose home now exists: index it
    /// (keeping the original `created_at`) and drop the legacy row, atomically.
    pub async fn complete_legacy_migration(
        &self,
        cfg: &WorkerConfig,
        home_path: &Path,
        created_at: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO workers (id, name, description, home_path, created_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 description = excluded.description,
                 home_path = excluded.home_path",
        )
        .bind(&cfg.id)
        .bind(&cfg.name)
        .bind(&cfg.description)
        .bind(home_path.to_string_lossy().as_ref())
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM legacy_workers WHERE id = ?")
            .bind(&cfg.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove a worker's index entry and its history.
    pub async fn delete_worker(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM messages WHERE worker_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM notices WHERE worker_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM workers WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Append messages to a worker's history, in order.
    pub async fn append_messages(&self, worker_id: &str, msgs: &[Message]) -> Result<()> {
        for m in msgs {
            sqlx::query("INSERT INTO messages (worker_id, data, created_at) VALUES (?, ?, ?)")
                .bind(worker_id)
                .bind(serde_json::to_string(m)?)
                .bind(Utc::now().to_rfc3339())
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Record a run error or warning for a worker.
    pub async fn append_notice(&self, worker_id: &str, notice: &Notice) -> Result<()> {
        sqlx::query(
            "INSERT INTO notices (id, worker_id, level, text, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&notice.id)
        .bind(worker_id)
        .bind(serde_json::to_string(&notice.level)?)
        .bind(&notice.text)
        .bind(notice.timestamp.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A worker's notices, oldest first.
    pub async fn load_notices(&self, worker_id: &str) -> Result<Vec<Notice>> {
        let rows = sqlx::query(
            "SELECT id, level, text, created_at FROM notices WHERE worker_id = ?
             ORDER BY created_at, id",
        )
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let level: String = row.get("level");
                let created_at: String = row.get("created_at");
                Ok(Notice {
                    id: row.get("id"),
                    level: serde_json::from_str(&level)?,
                    text: row.get("text"),
                    timestamp: chrono::DateTime::parse_from_rfc3339(&created_at)?.to_utc(),
                })
            })
            .collect()
    }

    /// Load a worker's full message history, oldest first.
    pub async fn load_history(&self, worker_id: &str) -> Result<Vec<Message>> {
        let rows = sqlx::query("SELECT data FROM messages WHERE worker_id = ? ORDER BY seq")
            .bind(worker_id)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let data: String = row.get("data");
            out.push(serde_json::from_str(&data)?);
        }
        Ok(out)
    }
}
