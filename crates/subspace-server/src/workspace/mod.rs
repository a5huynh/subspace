//! Teams, workspaces, and DMs: the shared-conversation model.
//!
//! A workspace's public transcript is a list of [`WorkspacePost`]s. Each worker
//! that takes part keeps its own private session per [`Conversation`]; a
//! workspace turn renders the posts it hasn't seen into one user message.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::WorkerId;

pub mod hub;
pub mod router;

pub type TeamId = String;
pub type WorkspaceId = String;

/// Name of the team seeded on first boot.
pub const DEFAULT_TEAM: &str = "General";
/// Worker-to-worker hops allowed per operator post before routing stops.
pub const HOP_LIMIT: u32 = 5;
/// At most this many unseen posts are shown in one workspace turn.
pub const BACKLOG_LIMIT: usize = 50;
/// The operator's handle in workspaces.
pub const OPERATOR_HANDLE: &str = "you";

/// Which of a worker's sessions a message belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Conversation {
    /// The worker's 1:1 conversation with the operator.
    Dm,
    Workspace(WorkspaceId),
}

impl Conversation {
    /// The `conversation` column value.
    pub fn key(&self) -> String {
        match self {
            Self::Dm => "dm".to_string(),
            Self::Workspace(id) => format!("workspace:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Team {
    pub id: TeamId,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub team_id: TeamId,
    pub name: String,
    pub topic: String,
    pub created_at: DateTime<Utc>,
}

/// Who wrote a post.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Author {
    /// The (single) human operator.
    Operator,
    Worker(WorkerId),
}

/// One post in a workspace's public transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct WorkspacePost {
    pub id: String,
    pub workspace_id: WorkspaceId,
    /// Position in the transcript (increasing; not contiguous per workspace).
    pub seq: u64,
    pub author: Author,
    /// The author's display name when the post was written.
    pub author_name: String,
    pub text: String,
    /// Workers this post mentions (and so wakes, hop limit permitting).
    pub mentions: Vec<WorkerId>,
    /// The operator post that started this chain of replies.
    pub root_id: String,
    /// Worker-to-worker hops since `root_id` (0 for operator posts).
    pub hops: u32,
    /// Who was picked to answer, for operator posts that mention nobody.
    pub routing: Routing,
    pub timestamp: DateTime<Utc>,
}

/// How a post found its responder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Routing {
    /// Not routed: it mentions workers, comes from a worker, or the workspace
    /// has no running workers.
    Direct,
    /// The router is choosing.
    Pending,
    Chosen {
        worker_id: WorkerId,
        reason: String,
    },
    /// The router decided the post needs no reply.
    Nobody {
        reason: String,
    },
    /// Routing failed; nobody was woken.
    Failed {
        error: String,
    },
}

/// A worker's membership in a workspace, as shown to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct WorkspaceMember {
    pub worker_id: WorkerId,
    pub name: String,
    pub handle: String,
    pub description: String,
}

/// The mention handle for a worker name: lowercase, with runs of anything
/// but ASCII letters and digits collapsed to `-` (`Research Bot` →
/// `research-bot`).
pub fn handle_for(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "worker".to_string()
    } else {
        out
    }
}

/// The distinct `@handles` in `text`, lowercased, in order of appearance.
/// A mention starts at `@` not preceded by a word character (so emails don't
/// count) and runs over ASCII letters, digits, `-` and `_`.
pub fn parse_mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        let starts = c == '@' && !prev.is_some_and(|p| p.is_alphanumeric() || p == '_');
        prev = Some(c);
        if !starts {
            continue;
        }
        let rest = &text[i + 1..];
        let end = rest
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
            .unwrap_or(rest.len());
        let handle = rest[..end]
            .trim_end_matches(['-', '_'])
            .to_ascii_lowercase();
        if !handle.is_empty() && !out.contains(&handle) {
            out.push(handle);
        }
    }
    out
}

/// The user message for a workspace turn: who is here, who the worker is, and
/// the posts it hasn't seen yet.
pub fn render_turn(
    team: &str,
    workspace: &Workspace,
    members: &[WorkspaceMember],
    me: &WorkspaceMember,
    posts: &[WorkspacePost],
    omitted: usize,
) -> String {
    let handle_of = |post: &WorkspacePost| match &post.author {
        Author::Operator => OPERATOR_HANDLE.to_string(),
        Author::Worker(id) => members
            .iter()
            .find(|m| &m.worker_id == id)
            .map_or_else(|| handle_for(&post.author_name), |m| m.handle.clone()),
    };

    let mut roster = vec![format!("@{OPERATOR_HANDLE} (human operator)")];
    for m in members {
        roster.push(if m.description.is_empty() {
            format!("@{} ({})", m.handle, m.name)
        } else {
            format!("@{} ({}: {})", m.handle, m.name, m.description)
        });
    }

    let mut out = format!(
        "<workspace name=\"{}\" team=\"{}\">\n",
        workspace.name, team
    );
    if !workspace.topic.is_empty() {
        out.push_str(&format!("Topic: {}\n", workspace.topic));
    }
    out.push_str(&format!("Members: {}\n", roster.join(", ")));
    out.push_str(&format!("You are @{}.\n", me.handle));
    out.push_str("New posts:\n");
    if omitted > 0 {
        out.push_str(&format!("({omitted} earlier posts omitted)\n"));
    }
    for post in posts {
        out.push_str(&format!("[@{}] {}\n", handle_of(post), post.text));
    }
    out.push_str("</workspace>\n");
    out.push_str(
        "Reply with your next post in this workspace. Mention @handle to ask a member to respond.",
    );
    out
}
