//! Picks who answers an operator post that mentions nobody: one quick model
//! call that sees the members (handle, description, start of their role) and
//! the recent conversation, so follow-ups go to whoever is already on it.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use subspace::provider::{CompletionRequest, Provider};
use subspace::types::{Message, ThinkingLevel};

use super::{Author, Channel, ChannelMember, ChannelPost, OPERATOR_HANDLE, Routing};

/// The routing model when the server doesn't set one.
pub const DEFAULT_ROUTER_MODEL: &str = "claude-haiku-5-5";
/// How much of each member's role the router sees.
pub const ROLE_EXCERPT_CHARS: usize = 600;
/// How many earlier posts the router sees for context.
pub const CONTEXT_POSTS: usize = 10;

const MAX_TOKENS: u32 = 300;

const SYSTEM_PROMPT: &str = "You route messages in a team chat channel. Pick the single member best \
suited to respond to the new message, based on their descriptions and roles. If the message \
follows up on the recent conversation, prefer the member already handling it. If the message \
needs no response (thanks, acknowledgements, FYIs, notes), pick nobody.\n\n\
Reply with only a JSON object: {\"handle\": \"<member handle without @>\", \"reason\": \"<a few \
words>\"} or {\"handle\": null, \"reason\": \"<a few words>\"}.";

/// A member the router may pick: a running worker in the channel.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub member: ChannelMember,
    pub role: String,
}

pub struct Router {
    provider: Arc<dyn Provider>,
    model: String,
}

#[derive(Deserialize)]
struct Answer {
    handle: Option<String>,
    #[serde(default)]
    reason: String,
}

impl Router {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Decide who answers `post`. A lone candidate is picked without a model
    /// call. Errors (provider failure, unusable answer) mean nobody is woken.
    pub async fn choose(
        &self,
        channel: &Channel,
        candidates: &[Candidate],
        recent: &[ChannelPost],
        post: &ChannelPost,
    ) -> Result<Routing> {
        match candidates {
            [] => return Ok(Routing::Direct),
            [only] => {
                return Ok(Routing::Chosen {
                    worker_id: only.member.worker_id.clone(),
                    reason: "the only worker in this channel".into(),
                });
            }
            _ => {}
        }

        let request = CompletionRequest {
            model: self.model.clone(),
            messages: vec![Message::user(render_request(
                channel, candidates, recent, post,
            ))],
            system_prompt: Some(SYSTEM_PROMPT.to_string()),
            tools: Vec::new(),
            max_tokens: MAX_TOKENS,
            thinking: ThinkingLevel::Off,
        };
        let mut stream = self.provider.complete(request).await?;
        // Drain events so the provider task never blocks on a full channel.
        while stream.events.recv().await.is_some() {}
        let reply = stream.handle.await.context("router task panicked")??;
        parse_answer(&reply.text(), candidates)
    }
}

/// The router's user message: channel, members, recent posts, and the post.
pub fn render_request(
    channel: &Channel,
    candidates: &[Candidate],
    recent: &[ChannelPost],
    post: &ChannelPost,
) -> String {
    let handle_of = |p: &ChannelPost| match &p.author {
        Author::Operator => OPERATOR_HANDLE.to_string(),
        Author::Worker(id) => candidates
            .iter()
            .find(|c| &c.member.worker_id == id)
            .map_or_else(
                || super::handle_for(&p.author_name),
                |c| c.member.handle.clone(),
            ),
    };

    let mut out = format!("Channel: #{}", channel.name);
    if !channel.topic.is_empty() {
        out.push_str(&format!(" (topic: {})", channel.topic));
    }
    out.push_str("\n\nMembers:\n");
    for c in candidates {
        let m = &c.member;
        out.push_str(&format!("- @{} ({})", m.handle, m.name));
        if !m.description.is_empty() {
            out.push_str(&format!(": {}", m.description));
        }
        out.push('\n');
        let role = excerpt(&c.role, ROLE_EXCERPT_CHARS);
        if !role.is_empty() {
            out.push_str(&format!("  Role: {}\n", role.replace('\n', " ")));
        }
    }
    if !recent.is_empty() {
        out.push_str("\nRecent posts:\n");
        for p in recent {
            out.push_str(&format!("[@{}] {}\n", handle_of(p), p.text));
        }
    }
    out.push_str(&format!(
        "\nNew message:\n[@{OPERATOR_HANDLE}] {}\n",
        post.text
    ));
    out
}

/// Interpret the router's reply; tolerates prose or code fences around the JSON.
pub fn parse_answer(reply: &str, candidates: &[Candidate]) -> Result<Routing> {
    let json = match (reply.find('{'), reply.rfind('}')) {
        (Some(start), Some(end)) if start < end => &reply[start..=end],
        _ => return Err(anyhow!("the router's reply wasn't JSON: {reply:?}")),
    };
    let answer: Answer = serde_json::from_str(json)
        .with_context(|| format!("the router's reply wasn't the expected JSON: {json}"))?;
    let reason = answer.reason.trim().to_string();
    let Some(handle) = answer.handle else {
        return Ok(Routing::Nobody { reason });
    };
    let handle = handle.trim().trim_start_matches('@').to_ascii_lowercase();
    candidates
        .iter()
        .find(|c| c.member.handle == handle)
        .map(|c| Routing::Chosen {
            worker_id: c.member.worker_id.clone(),
            reason,
        })
        .ok_or_else(|| anyhow!("the router picked @{handle}, who isn't in this channel"))
}

fn excerpt(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}
