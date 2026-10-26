//! Workspaces, channels, and DMs: the shared-conversation model.
//!
//! A channel's public transcript is a list of [`ChannelPost`]s. Each worker
//! that takes part keeps its own private session per [`Conversation`]; a
//! channel turn renders the posts it hasn't seen into one user message.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::WorkerId;

pub mod hub;

pub type WorkspaceId = String;
pub type ChannelId = String;

/// Name of the workspace seeded on first boot.
pub const DEFAULT_WORKSPACE: &str = "General";
/// Worker-to-worker hops allowed per operator post before routing stops.
pub const HOP_LIMIT: u32 = 5;
/// At most this many unseen posts are shown in one channel turn.
pub const BACKLOG_LIMIT: usize = 50;
/// The operator's handle in channels.
pub const OPERATOR_HANDLE: &str = "you";

/// Which of a worker's sessions a message belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Conversation {
    /// The worker's 1:1 conversation with the operator.
    Dm,
    Channel(ChannelId),
}

impl Conversation {
    /// The `conversation` column value.
    pub fn key(&self) -> String {
        match self {
            Self::Dm => "dm".to_string(),
            Self::Channel(id) => format!("channel:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct Channel {
    pub id: ChannelId,
    pub workspace_id: WorkspaceId,
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

/// One post in a channel's public transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct ChannelPost {
    pub id: String,
    pub channel_id: ChannelId,
    /// Position in the transcript (increasing; not contiguous per channel).
    #[ts(type = "number")]
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
    pub timestamp: DateTime<Utc>,
}

/// A worker's membership in a channel, as shown to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct ChannelMember {
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

/// The user message for a channel turn: who is here, who the worker is, and
/// the posts it hasn't seen yet.
pub fn render_turn(
    workspace: &str,
    channel: &Channel,
    members: &[ChannelMember],
    me: &ChannelMember,
    posts: &[ChannelPost],
    omitted: usize,
) -> String {
    let handle_of = |post: &ChannelPost| match &post.author {
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
        "<channel name=\"{}\" workspace=\"{}\">\n",
        channel.name, workspace
    );
    if !channel.topic.is_empty() {
        out.push_str(&format!("Topic: {}\n", channel.topic));
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
    out.push_str("</channel>\n");
    out.push_str(
        "Reply with your next post in this channel. Mention @handle to ask a member to respond.",
    );
    out
}
