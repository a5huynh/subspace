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

    /// Remove a worker's index entry and its history.
    pub async fn delete_worker(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM messages WHERE worker_id = ?")
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
