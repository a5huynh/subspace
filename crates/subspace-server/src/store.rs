//! SQLite persistence for the worker index and message history.
//!
//! Worker identity lives in on-disk homes ([`crate::home`]); the `workers`
//! table is only an index of them.

use std::path::Path;
use std::str::FromStr;

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use subspace::types::Message;
use subspace::types::new_id;

use crate::channel::{
    Author, Channel, ChannelPost, Conversation, DEFAULT_WORKSPACE, Routing, Workspace,
};
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
        sqlx::query("DELETE FROM workspace_members WHERE worker_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM channel_members WHERE worker_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM workers WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Append messages to a worker's DM history, in order.
    pub async fn append_messages(&self, worker_id: &str, msgs: &[Message]) -> Result<()> {
        self.append_session(worker_id, &Conversation::Dm, msgs)
            .await
    }

    /// Append messages to one of a worker's sessions, in order.
    pub async fn append_session(
        &self,
        worker_id: &str,
        conversation: &Conversation,
        msgs: &[Message],
    ) -> Result<()> {
        for m in msgs {
            sqlx::query(
                "INSERT INTO messages (worker_id, conversation, data, created_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(worker_id)
            .bind(conversation.key())
            .bind(serde_json::to_string(m)?)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Record a run error or warning in a worker's DM.
    pub async fn append_notice(&self, worker_id: &str, notice: &Notice) -> Result<()> {
        self.append_conversation_notice(worker_id, &Conversation::Dm, notice)
            .await
    }

    /// Record a notice in a conversation. `worker_id` is the worker it concerns
    /// (empty for channel-wide notices such as the hop limit).
    pub async fn append_conversation_notice(
        &self,
        worker_id: &str,
        conversation: &Conversation,
        notice: &Notice,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO notices (id, worker_id, conversation, level, text, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&notice.id)
        .bind(worker_id)
        .bind(conversation.key())
        .bind(serde_json::to_string(&notice.level)?)
        .bind(&notice.text)
        .bind(notice.timestamp.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A worker's DM notices, oldest first.
    pub async fn load_notices(&self, worker_id: &str) -> Result<Vec<Notice>> {
        let rows = sqlx::query(
            "SELECT id, level, text, created_at FROM notices
             WHERE worker_id = ? AND conversation = 'dm'
             ORDER BY created_at, id",
        )
        .bind(worker_id)
        .fetch_all(&self.pool)
        .await?;
        notices_from(rows)
    }

    /// All notices in a conversation (any worker), oldest first.
    pub async fn load_conversation_notices(
        &self,
        conversation: &Conversation,
    ) -> Result<Vec<Notice>> {
        let rows = sqlx::query(
            "SELECT id, level, text, created_at FROM notices WHERE conversation = ?
             ORDER BY created_at, id",
        )
        .bind(conversation.key())
        .fetch_all(&self.pool)
        .await?;
        notices_from(rows)
    }

    /// Load a worker's full DM history, oldest first.
    pub async fn load_history(&self, worker_id: &str) -> Result<Vec<Message>> {
        self.load_session(worker_id, &Conversation::Dm).await
    }

    /// Load one of a worker's sessions, oldest first.
    pub async fn load_session(
        &self,
        worker_id: &str,
        conversation: &Conversation,
    ) -> Result<Vec<Message>> {
        let rows = sqlx::query(
            "SELECT data FROM messages WHERE worker_id = ? AND conversation = ? ORDER BY seq",
        )
        .bind(worker_id)
        .bind(conversation.key())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let data: String = row.get("data");
            out.push(serde_json::from_str(&data)?);
        }
        Ok(out)
    }

    // --- Workspaces -------------------------------------------------------

    /// The default workspace (the oldest one), creating `General` if there
    /// are none yet.
    pub async fn ensure_default_workspace(&self) -> Result<Workspace> {
        if let Some(ws) = self.list_workspaces().await?.into_iter().next() {
            return Ok(ws);
        }
        let ws = Workspace {
            id: new_id(),
            name: DEFAULT_WORKSPACE.to_string(),
            created_at: Utc::now(),
        };
        sqlx::query("INSERT INTO workspaces (id, name, created_at) VALUES (?, ?, ?)")
            .bind(&ws.id)
            .bind(&ws.name)
            .bind(ws.created_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(ws)
    }

    /// All workspaces, oldest first.
    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let rows =
            sqlx::query("SELECT id, name, created_at FROM workspaces ORDER BY created_at, id")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|row| {
                Ok(Workspace {
                    id: row.get("id"),
                    name: row.get("name"),
                    created_at: parse_ts(row.get("created_at"))?,
                })
            })
            .collect()
    }

    pub async fn get_workspace(&self, id: &str) -> Result<Option<Workspace>> {
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .find(|w| w.id == id))
    }

    /// Add a worker to a workspace (no-op if already a member).
    pub async fn add_workspace_member(&self, workspace_id: &str, worker_id: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO workspace_members (workspace_id, worker_id, joined_at)
             VALUES (?, ?, ?)",
        )
        .bind(workspace_id)
        .bind(worker_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove a worker from a workspace and from all of its channels.
    pub async fn remove_workspace_member(&self, workspace_id: &str, worker_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM channel_members WHERE worker_id = ?
             AND channel_id IN (SELECT id FROM channels WHERE workspace_id = ?)",
        )
        .bind(worker_id)
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM workspace_members WHERE workspace_id = ? AND worker_id = ?")
            .bind(workspace_id)
            .bind(worker_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Worker ids in a workspace, in join order.
    pub async fn workspace_members(&self, workspace_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT worker_id FROM workspace_members WHERE workspace_id = ?
             ORDER BY joined_at, worker_id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|row| row.get("worker_id")).collect())
    }

    /// Workspaces a worker belongs to.
    pub async fn worker_workspaces(&self, worker_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT workspace_id FROM workspace_members WHERE worker_id = ?")
            .bind(worker_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| row.get("workspace_id"))
            .collect())
    }

    // --- Channels ---------------------------------------------------------

    /// Create a channel. Returns `None` if the workspace already has a
    /// channel with this name.
    pub async fn create_channel(
        &self,
        workspace_id: &str,
        name: &str,
        topic: &str,
    ) -> Result<Option<Channel>> {
        let channel = Channel {
            id: new_id(),
            workspace_id: workspace_id.to_string(),
            name: name.to_string(),
            topic: topic.to_string(),
            created_at: Utc::now(),
        };
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO channels (id, workspace_id, name, topic, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&channel.id)
        .bind(&channel.workspace_id)
        .bind(&channel.name)
        .bind(&channel.topic)
        .bind(channel.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok((inserted == 1).then_some(channel))
    }

    /// Rename a channel and/or change its topic. Returns `false` if the new
    /// name is taken in its workspace.
    pub async fn update_channel(&self, id: &str, name: &str, topic: &str) -> Result<bool> {
        let result = sqlx::query("UPDATE OR IGNORE channels SET name = ?, topic = ? WHERE id = ?")
            .bind(name)
            .bind(topic)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Delete a channel with its posts, memberships, notices, and every
    /// worker's session for it.
    pub async fn delete_channel(&self, id: &str) -> Result<()> {
        let conversation = Conversation::Channel(id.to_string()).key();
        let mut tx = self.pool.begin().await?;
        for (sql, key) in [
            ("DELETE FROM channel_posts WHERE channel_id = ?", id),
            ("DELETE FROM channel_members WHERE channel_id = ?", id),
            (
                "DELETE FROM messages WHERE conversation = ?",
                conversation.as_str(),
            ),
            (
                "DELETE FROM notices WHERE conversation = ?",
                conversation.as_str(),
            ),
            ("DELETE FROM channels WHERE id = ?", id),
        ] {
            sqlx::query(sql).bind(key).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_channel(&self, id: &str) -> Result<Option<Channel>> {
        let row = sqlx::query(
            "SELECT id, workspace_id, name, topic, created_at FROM channels WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(channel_from).transpose()
    }

    /// A workspace's channels, by name.
    pub async fn list_channels(&self, workspace_id: &str) -> Result<Vec<Channel>> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, name, topic, created_at FROM channels
             WHERE workspace_id = ? ORDER BY name",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(channel_from).collect()
    }

    /// Add a worker to a channel (no-op if already a member).
    pub async fn add_channel_member(&self, channel_id: &str, worker_id: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO channel_members (channel_id, worker_id, joined_at)
             VALUES (?, ?, ?)",
        )
        .bind(channel_id)
        .bind(worker_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_channel_member(&self, channel_id: &str, worker_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM channel_members WHERE channel_id = ? AND worker_id = ?")
            .bind(channel_id)
            .bind(worker_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Worker ids in a channel, in join order.
    pub async fn channel_members(&self, channel_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT worker_id FROM channel_members WHERE channel_id = ?
             ORDER BY joined_at, worker_id",
        )
        .bind(channel_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|row| row.get("worker_id")).collect())
    }

    /// The last post seq a member has been shown, or `None` if it isn't a member.
    pub async fn last_seen(&self, channel_id: &str, worker_id: &str) -> Result<Option<u64>> {
        let row = sqlx::query(
            "SELECT last_seen_seq FROM channel_members WHERE channel_id = ? AND worker_id = ?",
        )
        .bind(channel_id)
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get::<i64, _>("last_seen_seq") as u64))
    }

    pub async fn set_last_seen(&self, channel_id: &str, worker_id: &str, seq: u64) -> Result<()> {
        sqlx::query(
            "UPDATE channel_members SET last_seen_seq = MAX(last_seen_seq, ?)
             WHERE channel_id = ? AND worker_id = ?",
        )
        .bind(seq as i64)
        .bind(channel_id)
        .bind(worker_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- Posts ------------------------------------------------------------

    /// Persist a post, assigning and returning its `seq`.
    pub async fn append_post(&self, post: &ChannelPost) -> Result<u64> {
        let (kind, author_id) = match &post.author {
            Author::Operator => ("operator", None),
            Author::Worker(id) => ("worker", Some(id.as_str())),
        };
        let row = sqlx::query(
            "INSERT INTO channel_posts
                 (id, channel_id, author_kind, author_id, author_name, text, mentions,
                  root_id, hops, routing, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING seq",
        )
        .bind(&post.id)
        .bind(&post.channel_id)
        .bind(kind)
        .bind(author_id)
        .bind(&post.author_name)
        .bind(&post.text)
        .bind(serde_json::to_string(&post.mentions)?)
        .bind(&post.root_id)
        .bind(post.hops as i64)
        .bind(serde_json::to_string(&post.routing)?)
        .bind(post.timestamp.to_rfc3339())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("seq") as u64)
    }

    /// Record how a post was routed.
    pub async fn set_routing(&self, post_id: &str, routing: &Routing) -> Result<()> {
        sqlx::query("UPDATE channel_posts SET routing = ? WHERE id = ?")
            .bind(serde_json::to_string(routing)?)
            .bind(post_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// A channel's posts, oldest first.
    pub async fn list_posts(&self, channel_id: &str) -> Result<Vec<ChannelPost>> {
        self.posts_after(channel_id, 0).await
    }

    /// A channel's posts with `seq > after`, oldest first.
    pub async fn posts_after(&self, channel_id: &str, after: u64) -> Result<Vec<ChannelPost>> {
        let rows = sqlx::query(
            "SELECT seq, id, channel_id, author_kind, author_id, author_name, text, mentions,
                    root_id, hops, routing, created_at
             FROM channel_posts WHERE channel_id = ? AND seq > ? ORDER BY seq",
        )
        .bind(channel_id)
        .bind(after as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let author = match row.get::<String, _>("author_kind").as_str() {
                    "worker" => Author::Worker(row.get("author_id")),
                    _ => Author::Operator,
                };
                let mentions: String = row.get("mentions");
                let routing: String = row.get("routing");
                Ok(ChannelPost {
                    id: row.get("id"),
                    channel_id: row.get("channel_id"),
                    seq: row.get::<i64, _>("seq") as u64,
                    author,
                    author_name: row.get("author_name"),
                    text: row.get("text"),
                    mentions: serde_json::from_str(&mentions)?,
                    root_id: row.get("root_id"),
                    hops: row.get::<i64, _>("hops") as u32,
                    routing: serde_json::from_str(&routing)?,
                    timestamp: parse_ts(row.get("created_at"))?,
                })
            })
            .collect()
    }
}

fn notices_from(rows: Vec<sqlx::sqlite::SqliteRow>) -> Result<Vec<Notice>> {
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

fn channel_from(row: sqlx::sqlite::SqliteRow) -> Result<Channel> {
    Ok(Channel {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        topic: row.get("topic"),
        created_at: parse_ts(row.get("created_at"))?,
    })
}

fn parse_ts(raw: String) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(&raw)?.to_utc())
}
