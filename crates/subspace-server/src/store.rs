//! SQLite persistence for workers and message history.

use std::str::FromStr;

use anyhow::Result;
use chrono::Utc;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use subspace::types::Message;

use crate::worker::WorkerConfig;

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

    /// Insert or update a worker's configuration.
    pub async fn save_worker(&self, cfg: &WorkerConfig) -> Result<()> {
        sqlx::query(
            "INSERT INTO workers (id, name, role, model, thinking, created_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 role = excluded.role,
                 model = excluded.model,
                 thinking = excluded.thinking",
        )
        .bind(&cfg.id)
        .bind(&cfg.name)
        .bind(&cfg.role)
        .bind(&cfg.model)
        .bind(serde_json::to_string(&cfg.thinking)?)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load all persisted worker configs, oldest first.
    pub async fn list_workers(&self) -> Result<Vec<WorkerConfig>> {
        let rows =
            sqlx::query("SELECT id, name, role, model, thinking FROM workers ORDER BY created_at")
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let thinking_s: String = row.get("thinking");
            out.push(WorkerConfig {
                id: row.get("id"),
                name: row.get("name"),
                description: String::new(),
                role: row.get("role"),
                model: row.get("model"),
                thinking: serde_json::from_str(&thinking_s)?,
            });
        }
        Ok(out)
    }

    /// Remove a worker and its history.
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
