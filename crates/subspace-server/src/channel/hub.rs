//! The channel hub: workspace/channel management, posting, mention routing
//! (with the hop limit), and per-channel event logs.
//!
//! Workers are woken with [`Stimulus::ChannelTurn`]; the worker loop calls
//! back into the hub to build its turn ([`ChannelHub::prepare_turn`]), stream
//! its activity, and post its reply ([`ChannelHub::post_reply`]). The hub
//! holds only *weak* mailbox senders, so it never keeps a worker alive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::Utc;
use subspace::types::new_id;
use tokio::sync::{broadcast, mpsc};

use super::{
    Author, BACKLOG_LIMIT, Channel, ChannelMember, ChannelPost, Conversation, HOP_LIMIT,
    OPERATOR_HANDLE, Workspace, handle_for, parse_mentions, render_turn,
};
use crate::WorkerId;
use crate::event::{
    ChannelEvent, ChannelEventKind, ChannelLog, Notice, NoticeLevel, Replay, WorkerEvent,
};
use crate::manager::ManagerError;
use crate::store::SqliteStore;
use crate::worker::{Stimulus, WorkerConfig};

const CHANNEL_BUS_CAPACITY: usize = 1024;

/// What a post's replies inherit: the chain it belongs to and its depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub root_id: String,
    pub hops: u32,
}

/// A prepared channel turn: the prompt to run in the worker's channel session.
#[derive(Debug, Clone)]
pub struct Turn {
    pub channel: Channel,
    pub prompt: String,
}

struct Registered {
    name: String,
    description: String,
    tx: mpsc::WeakSender<Stimulus>,
}

pub struct ChannelHub {
    store: Arc<SqliteStore>,
    bus: broadcast::Sender<ChannelEvent>,
    logs: Mutex<HashMap<String, Arc<ChannelLog>>>,
    workers: Mutex<HashMap<WorkerId, Registered>>,
}

impl ChannelHub {
    pub fn new(store: Arc<SqliteStore>) -> Self {
        let (bus, _) = broadcast::channel(CHANNEL_BUS_CAPACITY);
        Self {
            store,
            bus,
            logs: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
        }
    }

    /// Make a running worker reachable from channels (call again after a
    /// restart or rename).
    pub(crate) fn register(&self, config: &WorkerConfig, tx: &mpsc::Sender<Stimulus>) {
        self.workers.lock().unwrap().insert(
            config.id.clone(),
            Registered {
                name: config.name.clone(),
                description: config.description.clone(),
                tx: tx.downgrade(),
            },
        );
    }

    pub(crate) fn unregister(&self, id: &str) {
        self.workers.lock().unwrap().remove(id);
    }

    fn is_registered(&self, id: &str) -> bool {
        self.workers.lock().unwrap().contains_key(id)
    }

    // --- Event logs -------------------------------------------------------

    /// A channel's event log, created on first use.
    pub fn log(&self, channel_id: &str) -> Arc<ChannelLog> {
        self.logs
            .lock()
            .unwrap()
            .entry(channel_id.to_string())
            .or_insert_with(|| Arc::new(ChannelLog::new(channel_id, self.bus.clone())))
            .clone()
    }

    /// Subscribe to one channel's events after `since` (see
    /// [`crate::event::EventLog::subscribe`]). The receiver carries every
    /// channel's events; filter by `channel_id`.
    pub fn subscribe(
        &self,
        channel_id: &str,
        since: u64,
    ) -> (Replay<ChannelEvent>, broadcast::Receiver<ChannelEvent>) {
        self.log(channel_id).subscribe(since)
    }

    fn emit(&self, channel_id: &str, kind: ChannelEventKind) {
        self.log(channel_id).emit(kind);
    }

    /// Stream a worker's live activity in a channel.
    pub(crate) fn emit_worker(&self, channel_id: &str, worker_id: &str, event: WorkerEvent) {
        self.emit(
            channel_id,
            ChannelEventKind::Worker {
                worker_id: worker_id.to_string(),
                event,
            },
        );
    }

    /// Persist and publish a channel notice. `worker_id` is the worker it
    /// concerns, or empty for channel-wide notices.
    pub(crate) async fn notice(
        &self,
        channel_id: &str,
        worker_id: &str,
        level: NoticeLevel,
        text: impl Into<String>,
    ) {
        let notice = Notice::new(level, text);
        let conversation = Conversation::Channel(channel_id.to_string());
        if let Err(e) = self
            .store
            .append_conversation_notice(worker_id, &conversation, &notice)
            .await
        {
            tracing::warn!("failed to persist channel notice: {e:#}");
        }
        self.emit(channel_id, ChannelEventKind::Notice(notice));
    }

    /// A failed turn: tell the channel which worker failed and why.
    pub(crate) async fn turn_failed(
        &self,
        channel_id: &str,
        worker_id: &str,
        level: NoticeLevel,
        text: &str,
    ) {
        let name = self.name_of(worker_id);
        self.notice(channel_id, worker_id, level, format!("{name}: {text}"))
            .await;
    }

    fn name_of(&self, worker_id: &str) -> String {
        self.workers
            .lock()
            .unwrap()
            .get(worker_id)
            .map_or_else(|| worker_id.to_string(), |w| w.name.clone())
    }

    // --- Workspaces -------------------------------------------------------

    /// All workspaces, oldest (the default) first. There is always at least one.
    pub async fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.store.ensure_default_workspace().await?;
        self.store.list_workspaces().await
    }

    pub async fn workspace(&self, id: &str) -> Result<Workspace> {
        self.store
            .get_workspace(id)
            .await?
            .ok_or_else(|| ManagerError::NoSuchWorkspace(id.to_string()).into())
    }

    pub async fn workspace_members(&self, id: &str) -> Result<Vec<WorkerId>> {
        self.workspace(id).await?;
        self.store.workspace_members(id).await
    }

    pub async fn add_workspace_member(&self, id: &str, worker_id: &str) -> Result<()> {
        self.workspace(id).await?;
        if !self.is_registered(worker_id) {
            return Err(ManagerError::NotFound(worker_id.to_string()).into());
        }
        self.store.add_workspace_member(id, worker_id).await
    }

    /// Remove a worker from a workspace and all of its channels.
    pub async fn remove_workspace_member(&self, id: &str, worker_id: &str) -> Result<()> {
        self.workspace(id).await?;
        let mut affected = Vec::new();
        for channel in self.store.list_channels(id).await? {
            if self
                .store
                .last_seen(&channel.id, worker_id)
                .await?
                .is_some()
            {
                affected.push(channel.id);
            }
        }
        self.store.remove_workspace_member(id, worker_id).await?;
        for channel_id in affected {
            self.emit(&channel_id, ChannelEventKind::Changed);
        }
        Ok(())
    }

    /// Channels a worker is about to lose (before it is deleted), so they can
    /// be told their membership changed.
    pub(crate) async fn channels_of(&self, worker_id: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for ws in self.store.worker_workspaces(worker_id).await? {
            for channel in self.store.list_channels(&ws).await? {
                if self
                    .store
                    .last_seen(&channel.id, worker_id)
                    .await?
                    .is_some()
                {
                    out.push(channel.id);
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn changed(&self, channel_id: &str) {
        self.emit(channel_id, ChannelEventKind::Changed);
    }

    // --- Channels ---------------------------------------------------------

    pub async fn channels(&self, workspace_id: &str) -> Result<Vec<Channel>> {
        self.workspace(workspace_id).await?;
        self.store.list_channels(workspace_id).await
    }

    pub async fn channel(&self, id: &str) -> Result<Channel> {
        self.store
            .get_channel(id)
            .await?
            .ok_or_else(|| ManagerError::NoSuchChannel(id.to_string()).into())
    }

    /// Create a channel with initial worker members. Names are normalized
    /// like handles (`Launch Plan` → `launch-plan`).
    pub async fn create_channel(
        &self,
        workspace_id: &str,
        name: &str,
        topic: &str,
        members: &[WorkerId],
    ) -> Result<Channel> {
        self.workspace(workspace_id).await?;
        let name = channel_name(name)?;
        // Validate members up front so a bad id doesn't leave a half-made channel.
        let mut handles = vec![OPERATOR_HANDLE.to_string()];
        for worker_id in members {
            self.check_can_join(workspace_id, worker_id, &handles)
                .await?;
            handles.push(handle_for(&self.name_of(worker_id)));
        }
        let channel = self
            .store
            .create_channel(workspace_id, &name, topic.trim())
            .await?
            .ok_or_else(|| ManagerError::Conflict(format!("channel #{name} already exists")))?;
        for worker_id in members {
            self.store
                .add_channel_member(&channel.id, worker_id)
                .await?;
        }
        Ok(channel)
    }

    pub async fn update_channel(
        &self,
        id: &str,
        name: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Channel> {
        let current = self.channel(id).await?;
        let name = match name {
            Some(name) => channel_name(name)?,
            None => current.name,
        };
        let topic = topic.map_or(current.topic, |t| t.trim().to_string());
        if !self.store.update_channel(id, &name, &topic).await? {
            return Err(ManagerError::Conflict(format!("channel #{name} already exists")).into());
        }
        self.changed(id);
        self.channel(id).await
    }

    pub async fn delete_channel(&self, id: &str) -> Result<()> {
        self.channel(id).await?;
        self.store.delete_channel(id).await?;
        self.emit(id, ChannelEventKind::Deleted);
        Ok(())
    }

    /// A channel's members with their handles, in join order. Members that
    /// aren't running are described from the worker index.
    pub async fn members(&self, channel_id: &str) -> Result<Vec<ChannelMember>> {
        let ids = self.store.channel_members(channel_id).await?;
        let mut members = Vec::with_capacity(ids.len());
        let mut missing = Vec::new();
        {
            let workers = self.workers.lock().unwrap();
            for id in ids {
                match workers.get(&id) {
                    Some(w) => members.push(member(id, &w.name, &w.description)),
                    None => {
                        missing.push(members.len());
                        members.push(member(id, "", ""));
                    }
                }
            }
        }
        if !missing.is_empty() {
            let index = self.store.list_index().await?;
            for i in missing {
                let id = members[i].worker_id.clone();
                members[i] = match index.iter().find(|e| e.id == id) {
                    Some(e) => member(id, &e.name, &e.description),
                    None => member(id.clone(), &id, ""),
                };
            }
        }
        Ok(members)
    }

    pub async fn add_member(&self, channel_id: &str, worker_id: &str) -> Result<()> {
        let channel = self.channel(channel_id).await?;
        let members = self.members(channel_id).await?;
        if members.iter().any(|m| m.worker_id == worker_id) {
            return Ok(());
        }
        let mut handles = vec![OPERATOR_HANDLE.to_string()];
        handles.extend(members.into_iter().map(|m| m.handle));
        self.check_can_join(&channel.workspace_id, worker_id, &handles)
            .await?;
        self.store.add_channel_member(channel_id, worker_id).await?;
        self.changed(channel_id);
        Ok(())
    }

    pub async fn remove_member(&self, channel_id: &str, worker_id: &str) -> Result<()> {
        self.channel(channel_id).await?;
        self.store
            .remove_channel_member(channel_id, worker_id)
            .await?;
        self.changed(channel_id);
        Ok(())
    }

    async fn check_can_join(
        &self,
        workspace_id: &str,
        worker_id: &str,
        taken_handles: &[String],
    ) -> Result<()> {
        if !self.is_registered(worker_id) {
            return Err(ManagerError::NotFound(worker_id.to_string()).into());
        }
        let in_workspace = self
            .store
            .worker_workspaces(worker_id)
            .await?
            .iter()
            .any(|ws| ws == workspace_id);
        let name = self.name_of(worker_id);
        if !in_workspace {
            return Err(
                ManagerError::Invalid(format!("{name} isn't a member of this workspace")).into(),
            );
        }
        let handle = handle_for(&name);
        if taken_handles.contains(&handle) {
            return Err(ManagerError::Conflict(format!(
                "@{handle} is already taken in this channel; rename {name} first"
            ))
            .into());
        }
        Ok(())
    }

    // --- Posting and routing ----------------------------------------------

    /// A channel's posts, oldest first.
    pub async fn posts(&self, channel_id: &str) -> Result<Vec<ChannelPost>> {
        self.store.list_posts(channel_id).await
    }

    /// A channel's notices, oldest first.
    pub async fn notices(&self, channel_id: &str) -> Result<Vec<Notice>> {
        self.store
            .load_conversation_notices(&Conversation::Channel(channel_id.to_string()))
            .await
    }

    /// Post as the operator, waking the workers it mentions.
    pub async fn post_operator(&self, channel_id: &str, text: &str) -> Result<ChannelPost> {
        self.channel(channel_id).await?;
        let text = text.trim();
        if text.is_empty() {
            return Err(ManagerError::Invalid("post text must not be empty".into()).into());
        }
        let members = self.members(channel_id).await?;
        let id = new_id();
        let mut post = ChannelPost {
            id: id.clone(),
            channel_id: channel_id.to_string(),
            seq: 0,
            author: Author::Operator,
            author_name: "You".to_string(),
            text: text.to_string(),
            mentions: resolve_mentions(text, &members, None),
            root_id: id.clone(),
            hops: 0,
            timestamp: Utc::now(),
        };
        post.seq = self.store.append_post(&post).await?;
        self.emit(channel_id, ChannelEventKind::Post(post.clone()));

        let trigger = Trigger {
            root_id: id,
            hops: 0,
        };
        for worker_id in &post.mentions {
            self.wake(channel_id, worker_id, &trigger).await;
        }
        Ok(post)
    }

    /// Post a worker's reply. Its mentions are woken unless the chain has hit
    /// [`HOP_LIMIT`].
    pub(crate) async fn post_reply(
        &self,
        channel_id: &str,
        worker_id: &str,
        text: &str,
        trigger: &Trigger,
    ) -> Result<()> {
        let members = self.members(channel_id).await?;
        let hops = trigger.hops + 1;
        let mut post = ChannelPost {
            id: new_id(),
            channel_id: channel_id.to_string(),
            seq: 0,
            author: Author::Worker(worker_id.to_string()),
            author_name: self.name_of(worker_id),
            text: text.trim().to_string(),
            mentions: resolve_mentions(text, &members, Some(worker_id)),
            root_id: trigger.root_id.clone(),
            hops,
            timestamp: Utc::now(),
        };
        post.seq = self.store.append_post(&post).await?;
        // The author has seen its own post.
        self.store
            .set_last_seen(channel_id, worker_id, post.seq)
            .await?;
        self.emit(channel_id, ChannelEventKind::Post(post.clone()));

        if post.mentions.is_empty() {
            return Ok(());
        }
        if hops >= HOP_LIMIT {
            self.notice(
                channel_id,
                "",
                NoticeLevel::Warning,
                format!(
                    "Stopped after {HOP_LIMIT} agent replies in a row; post to continue the conversation."
                ),
            )
            .await;
            return Ok(());
        }
        let next = Trigger {
            root_id: post.root_id.clone(),
            hops,
        };
        for mentioned in &post.mentions {
            self.wake(channel_id, mentioned, &next).await;
        }
        Ok(())
    }

    /// Queue a channel turn on a worker without blocking: a stopped worker or
    /// a full mailbox becomes a notice instead.
    async fn wake(&self, channel_id: &str, worker_id: &str, trigger: &Trigger) {
        let tx = self
            .workers
            .lock()
            .unwrap()
            .get(worker_id)
            .and_then(|w| w.tx.upgrade());
        let stimulus = Stimulus::ChannelTurn {
            channel_id: channel_id.to_string(),
            trigger: trigger.clone(),
        };
        let failure = match tx.map(|tx| tx.try_send(stimulus)) {
            Some(Ok(())) => return,
            Some(Err(mpsc::error::TrySendError::Full(_))) => "is too busy to respond right now",
            Some(Err(mpsc::error::TrySendError::Closed(_))) | None => "isn't running",
        };
        let name = self.name_of(worker_id);
        self.notice(
            channel_id,
            worker_id,
            NoticeLevel::Warning,
            format!("{name} {failure}"),
        )
        .await;
    }

    /// Build a worker's next turn in a channel from the posts it hasn't seen,
    /// marking them seen. `None` if it isn't a member or nothing is new.
    pub(crate) async fn prepare_turn(
        &self,
        channel_id: &str,
        worker_id: &str,
    ) -> Result<Option<Turn>> {
        let Some(last_seen) = self.store.last_seen(channel_id, worker_id).await? else {
            return Ok(None);
        };
        let Some(channel) = self.store.get_channel(channel_id).await? else {
            return Ok(None);
        };
        let posts = self.store.posts_after(channel_id, last_seen).await?;
        let Some(through) = posts.last().map(|p| p.seq) else {
            return Ok(None);
        };
        self.store
            .set_last_seen(channel_id, worker_id, through)
            .await?;

        let mut unseen: Vec<_> = posts
            .into_iter()
            .filter(|p| p.author != Author::Worker(worker_id.to_string()))
            .collect();
        if unseen.is_empty() {
            return Ok(None);
        }
        let omitted = unseen.len().saturating_sub(BACKLOG_LIMIT);
        unseen.drain(..omitted);

        let members = self.members(channel_id).await?;
        let Some(me) = members.iter().find(|m| m.worker_id == worker_id) else {
            return Ok(None);
        };
        let workspace = self
            .store
            .get_workspace(&channel.workspace_id)
            .await?
            .map(|w| w.name)
            .unwrap_or_default();
        let prompt = render_turn(&workspace, &channel, &members, me, &unseen, omitted);
        Ok(Some(Turn { channel, prompt }))
    }
}

fn member(worker_id: WorkerId, name: &str, description: &str) -> ChannelMember {
    ChannelMember {
        worker_id,
        name: name.to_string(),
        handle: handle_for(name),
        description: description.to_string(),
    }
}

/// The member workers `text` mentions, excluding `author`.
fn resolve_mentions(text: &str, members: &[ChannelMember], author: Option<&str>) -> Vec<WorkerId> {
    let mut out = Vec::new();
    for handle in parse_mentions(text) {
        if let Some(m) = members.iter().find(|m| m.handle == handle)
            && Some(m.worker_id.as_str()) != author
            && !out.contains(&m.worker_id)
        {
            out.push(m.worker_id.clone());
        }
    }
    out
}

/// Normalize a channel name: lowercase words joined by `-`.
fn channel_name(raw: &str) -> Result<String> {
    if raw.trim().is_empty() || !raw.chars().any(|c| c.is_ascii_alphanumeric()) {
        return Err(ManagerError::Invalid(
            "channel names need at least one letter or digit".into(),
        )
        .into());
    }
    Ok(handle_for(raw))
}
