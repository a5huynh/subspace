//! The workspace hub: team/workspace management, posting, mention routing
//! (with the hop limit), and per-workspace event logs.
//!
//! Workers are woken with [`Stimulus::WorkspaceTurn`]; the worker loop calls
//! back into the hub to build its turn ([`WorkspaceHub::prepare_turn`]), stream
//! its activity, and post its reply ([`WorkspaceHub::post_reply`]). The hub
//! holds only *weak* mailbox senders, so it never keeps a worker alive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::Utc;
use subspace::types::new_id;
use tokio::sync::{broadcast, mpsc};

use super::router::{CONTEXT_POSTS, Candidate, Router};
use super::{
    Author, BACKLOG_LIMIT, Conversation, HOP_LIMIT, OPERATOR_HANDLE, Routing, Team, Workspace,
    WorkspaceMember, WorkspacePost, handle_for, parse_mentions, render_turn,
};
use crate::WorkerId;
use crate::event::{
    Notice, NoticeLevel, Replay, WorkerEvent, WorkspaceEvent, WorkspaceEventKind, WorkspaceLog,
};
use crate::manager::ManagerError;
use crate::store::SqliteStore;
use crate::worker::{Stimulus, WorkerConfig};

const WORKSPACE_BUS_CAPACITY: usize = 1024;
const MAX_WORKSPACE_NAME: usize = 80;

/// What a post's replies inherit: the chain it belongs to and its depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub root_id: String,
    pub hops: u32,
}

/// A prepared workspace turn: the prompt to run in the worker's workspace session.
#[derive(Debug, Clone)]
pub struct Turn {
    pub workspace: Workspace,
    pub prompt: String,
}

struct Registered {
    name: String,
    description: String,
    role: String,
    tx: mpsc::WeakSender<Stimulus>,
}

pub struct WorkspaceHub {
    store: Arc<SqliteStore>,
    bus: broadcast::Sender<WorkspaceEvent>,
    logs: Mutex<HashMap<String, Arc<WorkspaceLog>>>,
    workers: Mutex<HashMap<WorkerId, Registered>>,
    router: Router,
}

impl WorkspaceHub {
    pub fn new(store: Arc<SqliteStore>, router: Router) -> Self {
        let (bus, _) = broadcast::channel(WORKSPACE_BUS_CAPACITY);
        Self {
            store,
            bus,
            logs: Mutex::new(HashMap::new()),
            workers: Mutex::new(HashMap::new()),
            router,
        }
    }

    /// The model used to route unaddressed posts.
    pub fn router_model(&self) -> &str {
        self.router.model()
    }

    /// Make a running worker reachable from workspaces (call again after a
    /// restart or rename).
    pub(crate) fn register(&self, config: &WorkerConfig, tx: &mpsc::Sender<Stimulus>) {
        self.workers.lock().unwrap().insert(
            config.id.clone(),
            Registered {
                name: config.name.clone(),
                description: config.description.clone(),
                role: config.role.clone(),
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

    /// A workspace's event log, created on first use.
    pub fn log(&self, workspace_id: &str) -> Arc<WorkspaceLog> {
        self.logs
            .lock()
            .unwrap()
            .entry(workspace_id.to_string())
            .or_insert_with(|| Arc::new(WorkspaceLog::new(workspace_id, self.bus.clone())))
            .clone()
    }

    /// Subscribe to one workspace's events after `since` (see
    /// [`crate::event::EventLog::subscribe`]). The receiver carries every
    /// workspace's events; filter by `workspace_id`.
    pub fn subscribe(
        &self,
        workspace_id: &str,
        since: u64,
    ) -> (Replay<WorkspaceEvent>, broadcast::Receiver<WorkspaceEvent>) {
        self.log(workspace_id).subscribe(since)
    }

    fn emit(&self, workspace_id: &str, kind: WorkspaceEventKind) {
        self.log(workspace_id).emit(kind);
    }

    /// Stream a worker's live activity in a workspace.
    pub(crate) fn emit_worker(&self, workspace_id: &str, worker_id: &str, event: WorkerEvent) {
        self.emit(
            workspace_id,
            WorkspaceEventKind::Worker {
                worker_id: worker_id.to_string(),
                event,
            },
        );
    }

    /// Persist and publish a workspace notice. `worker_id` is the worker it
    /// concerns, or empty for workspace-wide notices.
    pub(crate) async fn notice(
        &self,
        workspace_id: &str,
        worker_id: &str,
        level: NoticeLevel,
        text: impl Into<String>,
    ) {
        let notice = Notice::new(level, text);
        let conversation = Conversation::Workspace(workspace_id.to_string());
        if let Err(e) = self
            .store
            .append_conversation_notice(worker_id, &conversation, &notice)
            .await
        {
            tracing::warn!("failed to persist workspace notice: {e:#}");
        }
        self.emit(workspace_id, WorkspaceEventKind::Notice(notice));
    }

    /// A failed turn: tell the workspace which worker failed and why.
    pub(crate) async fn turn_failed(
        &self,
        workspace_id: &str,
        worker_id: &str,
        level: NoticeLevel,
        text: &str,
    ) {
        let name = self.name_of(worker_id);
        self.notice(workspace_id, worker_id, level, format!("{name}: {text}"))
            .await;
    }

    fn name_of(&self, worker_id: &str) -> String {
        self.workers
            .lock()
            .unwrap()
            .get(worker_id)
            .map_or_else(|| worker_id.to_string(), |w| w.name.clone())
    }

    // --- Teams -------------------------------------------------------

    /// All teams, oldest (the default) first. There is always at least one.
    pub async fn teams(&self) -> Result<Vec<Team>> {
        self.store.ensure_default_team().await?;
        self.store.list_teams().await
    }

    pub async fn team(&self, id: &str) -> Result<Team> {
        self.store
            .get_team(id)
            .await?
            .ok_or_else(|| ManagerError::NoSuchTeam(id.to_string()).into())
    }

    pub async fn team_members(&self, id: &str) -> Result<Vec<WorkerId>> {
        self.team(id).await?;
        self.store.team_members(id).await
    }

    pub async fn add_team_member(&self, id: &str, worker_id: &str) -> Result<()> {
        self.team(id).await?;
        if !self.is_registered(worker_id) {
            return Err(ManagerError::NotFound(worker_id.to_string()).into());
        }
        self.store.add_team_member(id, worker_id).await
    }

    /// Remove a worker from a team and all of its workspaces.
    pub async fn remove_team_member(&self, id: &str, worker_id: &str) -> Result<()> {
        self.team(id).await?;
        let mut affected = Vec::new();
        for workspace in self.store.list_workspaces(id).await? {
            if self
                .store
                .last_seen(&workspace.id, worker_id)
                .await?
                .is_some()
            {
                affected.push(workspace.id);
            }
        }
        self.store.remove_team_member(id, worker_id).await?;
        for workspace_id in affected {
            self.emit(&workspace_id, WorkspaceEventKind::Changed);
        }
        Ok(())
    }

    /// Workspaces a worker is about to lose (before it is deleted), so they can
    /// be told their membership changed.
    pub(crate) async fn workspaces_of(&self, worker_id: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for ws in self.store.worker_teams(worker_id).await? {
            for workspace in self.store.list_workspaces(&ws).await? {
                if self
                    .store
                    .last_seen(&workspace.id, worker_id)
                    .await?
                    .is_some()
                {
                    out.push(workspace.id);
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn changed(&self, workspace_id: &str) {
        self.emit(workspace_id, WorkspaceEventKind::Changed);
    }

    // --- Workspaces ---------------------------------------------------------

    pub async fn workspaces(&self, team_id: &str) -> Result<Vec<Workspace>> {
        self.team(team_id).await?;
        self.store.list_workspaces(team_id).await
    }

    pub async fn workspace(&self, id: &str) -> Result<Workspace> {
        self.store
            .get_workspace(id)
            .await?
            .ok_or_else(|| ManagerError::NoSuchWorkspace(id.to_string()).into())
    }

    /// Create a workspace with initial worker members. Names keep their
    /// wording; whitespace is tidied and they must be unique within the team.
    pub async fn create_workspace(
        &self,
        team_id: &str,
        name: &str,
        topic: &str,
        members: &[WorkerId],
    ) -> Result<Workspace> {
        self.team(team_id).await?;
        let name = workspace_name(name)?;
        // Validate members up front so a bad id doesn't leave a half-made workspace.
        let mut handles = vec![OPERATOR_HANDLE.to_string()];
        for worker_id in members {
            self.check_can_join(team_id, worker_id, &handles).await?;
            handles.push(handle_for(&self.name_of(worker_id)));
        }
        let workspace = self
            .store
            .create_workspace(team_id, &name, topic.trim())
            .await?
            .ok_or_else(|| {
                ManagerError::Conflict(format!("a workspace named \"{name}\" already exists"))
            })?;
        for worker_id in members {
            self.store
                .add_workspace_member(&workspace.id, worker_id)
                .await?;
        }
        Ok(workspace)
    }

    pub async fn update_workspace(
        &self,
        id: &str,
        name: Option<&str>,
        topic: Option<&str>,
    ) -> Result<Workspace> {
        let current = self.workspace(id).await?;
        let name = match name {
            Some(name) => workspace_name(name)?,
            None => current.name,
        };
        let topic = topic.map_or(current.topic, |t| t.trim().to_string());
        if !self.store.update_workspace(id, &name, &topic).await? {
            return Err(ManagerError::Conflict(format!(
                "a workspace named \"{name}\" already exists"
            ))
            .into());
        }
        self.changed(id);
        self.workspace(id).await
    }

    pub async fn delete_workspace(&self, id: &str) -> Result<()> {
        self.workspace(id).await?;
        self.store.delete_workspace(id).await?;
        self.emit(id, WorkspaceEventKind::Deleted);
        Ok(())
    }

    /// A workspace's members with their handles, in join order. Members that
    /// aren't running are described from the worker index.
    pub async fn members(&self, workspace_id: &str) -> Result<Vec<WorkspaceMember>> {
        let ids = self.store.workspace_members(workspace_id).await?;
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

    pub async fn add_member(&self, workspace_id: &str, worker_id: &str) -> Result<()> {
        let workspace = self.workspace(workspace_id).await?;
        let members = self.members(workspace_id).await?;
        if members.iter().any(|m| m.worker_id == worker_id) {
            return Ok(());
        }
        let mut handles = vec![OPERATOR_HANDLE.to_string()];
        handles.extend(members.into_iter().map(|m| m.handle));
        self.check_can_join(&workspace.team_id, worker_id, &handles)
            .await?;
        self.store
            .add_workspace_member(workspace_id, worker_id)
            .await?;
        self.changed(workspace_id);
        Ok(())
    }

    pub async fn remove_member(&self, workspace_id: &str, worker_id: &str) -> Result<()> {
        self.workspace(workspace_id).await?;
        self.store
            .remove_workspace_member(workspace_id, worker_id)
            .await?;
        self.changed(workspace_id);
        Ok(())
    }

    async fn check_can_join(
        &self,
        team_id: &str,
        worker_id: &str,
        taken_handles: &[String],
    ) -> Result<()> {
        if !self.is_registered(worker_id) {
            return Err(ManagerError::NotFound(worker_id.to_string()).into());
        }
        let in_team = self
            .store
            .worker_teams(worker_id)
            .await?
            .iter()
            .any(|ws| ws == team_id);
        let name = self.name_of(worker_id);
        if !in_team {
            return Err(
                ManagerError::Invalid(format!("{name} isn't a member of this team")).into(),
            );
        }
        let handle = handle_for(&name);
        if taken_handles.contains(&handle) {
            return Err(ManagerError::Conflict(format!(
                "@{handle} is already taken in this workspace; rename {name} first"
            ))
            .into());
        }
        Ok(())
    }

    // --- Posting and routing ----------------------------------------------

    /// A workspace's posts, oldest first.
    pub async fn posts(&self, workspace_id: &str) -> Result<Vec<WorkspacePost>> {
        self.store.list_posts(workspace_id).await
    }

    /// A workspace's notices, oldest first.
    pub async fn notices(&self, workspace_id: &str) -> Result<Vec<Notice>> {
        self.store
            .load_conversation_notices(&Conversation::Workspace(workspace_id.to_string()))
            .await
    }

    /// Post as the operator. A post that mentions workers wakes them; one
    /// that mentions nobody is routed to the best-suited running member (in
    /// the background when that takes a model call).
    pub async fn post_operator(
        self: &Arc<Self>,
        workspace_id: &str,
        text: &str,
    ) -> Result<WorkspacePost> {
        let workspace = self.workspace(workspace_id).await?;
        let text = text.trim();
        if text.is_empty() {
            return Err(ManagerError::Invalid("post text must not be empty".into()).into());
        }
        let members = self.members(workspace_id).await?;
        let mentions = resolve_mentions(text, &members, None);
        let candidates = if mentions.is_empty() {
            self.candidates(&members)
        } else {
            Vec::new()
        };
        let routing = match candidates.len() {
            0 => Routing::Direct,
            1 => Routing::Chosen {
                worker_id: candidates[0].member.worker_id.clone(),
                reason: "the only worker in this workspace".into(),
            },
            _ => Routing::Pending,
        };

        let id = new_id();
        let mut post = WorkspacePost {
            id: id.clone(),
            workspace_id: workspace_id.to_string(),
            seq: 0,
            author: Author::Operator,
            author_name: "You".to_string(),
            text: text.to_string(),
            mentions,
            root_id: id.clone(),
            hops: 0,
            routing,
            timestamp: Utc::now(),
        };
        // Context for the router: the posts before this one.
        let recent = if post.routing == Routing::Pending {
            let mut all = self.store.list_posts(workspace_id).await?;
            all.drain(..all.len().saturating_sub(CONTEXT_POSTS));
            all
        } else {
            Vec::new()
        };
        post.seq = self.store.append_post(&post).await?;
        self.emit(workspace_id, WorkspaceEventKind::Post(post.clone()));

        let trigger = Trigger {
            root_id: id,
            hops: 0,
        };
        match &post.routing {
            Routing::Chosen { worker_id, .. } => {
                self.wake(workspace_id, worker_id, &trigger).await;
            }
            Routing::Pending => {
                let hub = self.clone();
                let post = post.clone();
                tokio::spawn(async move {
                    hub.route(workspace, candidates, recent, post, trigger)
                        .await;
                });
            }
            _ => {
                for worker_id in &post.mentions {
                    self.wake(workspace_id, worker_id, &trigger).await;
                }
            }
        }
        Ok(post)
    }

    /// Running members the router may pick.
    fn candidates(&self, members: &[WorkspaceMember]) -> Vec<Candidate> {
        let workers = self.workers.lock().unwrap();
        members
            .iter()
            .filter_map(|m| {
                workers.get(&m.worker_id).map(|w| Candidate {
                    member: m.clone(),
                    role: w.role.clone(),
                })
            })
            .collect()
    }

    /// Ask the router, record its decision on the post, and wake the pick.
    async fn route(
        &self,
        workspace: Workspace,
        candidates: Vec<Candidate>,
        recent: Vec<WorkspacePost>,
        post: WorkspacePost,
        trigger: Trigger,
    ) {
        let routing = match self
            .router
            .choose(&workspace, &candidates, &recent, &post)
            .await
        {
            Ok(routing) => routing,
            Err(e) => {
                tracing::warn!("routing a post in \"{}\" failed: {e:#}", workspace.name);
                self.notice(
                    &workspace.id,
                    "",
                    NoticeLevel::Warning,
                    "Couldn't pick a worker to answer; @mention someone instead.",
                )
                .await;
                Routing::Failed {
                    error: format!("{e:#}"),
                }
            }
        };
        if let Err(e) = self.store.set_routing(&post.id, &routing).await {
            tracing::warn!("failed to persist routing: {e:#}");
        }
        self.emit(
            &workspace.id,
            WorkspaceEventKind::Routed {
                post_id: post.id.clone(),
                routing: routing.clone(),
            },
        );
        if let Routing::Chosen { worker_id, .. } = &routing {
            self.wake(&workspace.id, worker_id, &trigger).await;
        }
    }

    /// Post a worker's reply. Its mentions are woken unless the chain has hit
    /// [`HOP_LIMIT`].
    pub(crate) async fn post_reply(
        &self,
        workspace_id: &str,
        worker_id: &str,
        text: &str,
        trigger: &Trigger,
    ) -> Result<()> {
        let members = self.members(workspace_id).await?;
        let hops = trigger.hops + 1;
        let mut post = WorkspacePost {
            id: new_id(),
            workspace_id: workspace_id.to_string(),
            seq: 0,
            author: Author::Worker(worker_id.to_string()),
            author_name: self.name_of(worker_id),
            text: text.trim().to_string(),
            mentions: resolve_mentions(text, &members, Some(worker_id)),
            root_id: trigger.root_id.clone(),
            hops,
            routing: Routing::Direct,
            timestamp: Utc::now(),
        };
        post.seq = self.store.append_post(&post).await?;
        // The author has seen its own post.
        self.store
            .set_last_seen(workspace_id, worker_id, post.seq)
            .await?;
        self.emit(workspace_id, WorkspaceEventKind::Post(post.clone()));

        if post.mentions.is_empty() {
            return Ok(());
        }
        if hops >= HOP_LIMIT {
            self.notice(
                workspace_id,
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
            self.wake(workspace_id, mentioned, &next).await;
        }
        Ok(())
    }

    /// Queue a workspace turn on a worker without blocking: a stopped worker or
    /// a full mailbox becomes a notice instead.
    async fn wake(&self, workspace_id: &str, worker_id: &str, trigger: &Trigger) {
        let tx = self
            .workers
            .lock()
            .unwrap()
            .get(worker_id)
            .and_then(|w| w.tx.upgrade());
        let stimulus = Stimulus::WorkspaceTurn {
            workspace_id: workspace_id.to_string(),
            trigger: trigger.clone(),
        };
        let failure = match tx.map(|tx| tx.try_send(stimulus)) {
            Some(Ok(())) => return,
            Some(Err(mpsc::error::TrySendError::Full(_))) => "is too busy to respond right now",
            Some(Err(mpsc::error::TrySendError::Closed(_))) | None => "isn't running",
        };
        let name = self.name_of(worker_id);
        self.notice(
            workspace_id,
            worker_id,
            NoticeLevel::Warning,
            format!("{name} {failure}"),
        )
        .await;
    }

    /// Build a worker's next turn in a workspace from the posts it hasn't seen,
    /// marking them seen. `None` if it isn't a member or nothing is new.
    pub(crate) async fn prepare_turn(
        &self,
        workspace_id: &str,
        worker_id: &str,
    ) -> Result<Option<Turn>> {
        let Some(last_seen) = self.store.last_seen(workspace_id, worker_id).await? else {
            return Ok(None);
        };
        let Some(workspace) = self.store.get_workspace(workspace_id).await? else {
            return Ok(None);
        };
        let posts = self.store.posts_after(workspace_id, last_seen).await?;
        let Some(through) = posts.last().map(|p| p.seq) else {
            return Ok(None);
        };
        self.store
            .set_last_seen(workspace_id, worker_id, through)
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

        let members = self.members(workspace_id).await?;
        let Some(me) = members.iter().find(|m| m.worker_id == worker_id) else {
            return Ok(None);
        };
        let team = self
            .store
            .get_team(&workspace.team_id)
            .await?
            .map(|w| w.name)
            .unwrap_or_default();
        let prompt = render_turn(&team, &workspace, &members, me, &unseen, omitted);
        Ok(Some(Turn { workspace, prompt }))
    }
}

fn member(worker_id: WorkerId, name: &str, description: &str) -> WorkspaceMember {
    WorkspaceMember {
        worker_id,
        name: name.to_string(),
        handle: handle_for(name),
        description: description.to_string(),
    }
}

/// The member workers `text` mentions, excluding `author`.
fn resolve_mentions(
    text: &str,
    members: &[WorkspaceMember],
    author: Option<&str>,
) -> Vec<WorkerId> {
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

/// Normalize a workspace name: trimmed, inner whitespace collapsed.
fn workspace_name(raw: &str) -> Result<String> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err(ManagerError::Invalid("workspace names must not be empty".into()).into());
    }
    if name.chars().count() > MAX_WORKSPACE_NAME {
        return Err(ManagerError::Invalid(format!(
            "workspace names are at most {MAX_WORKSPACE_NAME} characters"
        ))
        .into());
    }
    Ok(name)
}
