//! Worker configuration, status, stimuli, and the per-worker runtime loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use subspace::agent::{Agent, AgentEvent};
use subspace::types::{Message, Role, ThinkingLevel, new_id};
use tokio::sync::mpsc;
use ts_rs::TS;

use crate::WorkerId;
use crate::channel::hub::{ChannelHub, Trigger, Turn};
use crate::channel::{ChannelId, Conversation};
use crate::event::{EventLog, Notice, NoticeLevel, WorkerEvent};
use crate::skills::SkillSelection;
use crate::store::SqliteStore;

/// Static configuration for a worker.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct WorkerConfig {
    pub id: WorkerId,
    pub name: String,
    /// One line describing what this worker is good at (used for routing).
    #[serde(default)]
    pub description: String,
    /// The worker's role / persona — becomes the agent system prompt.
    pub role: String,
    pub model: String,
    pub thinking: ThinkingLevel,
    /// Which discovered skills are enabled (default: all).
    #[serde(default)]
    pub skills: SkillSelection,
}

impl WorkerConfig {
    /// Create a config with a fresh id and engine-default model/thinking level.
    pub fn new(name: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            description: String::new(),
            role: role.into(),
            model: subspace::agent::AgentConfig::default().model,
            thinking: ThinkingLevel::Low,
            skills: SkillSelection::default(),
        }
    }
}

/// Runtime status of a worker. Serialized as `{"state": "idle"}` or
/// `{"state": "error", "message": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "state", content = "message", rename_all = "snake_case")]
pub enum WorkerStatus {
    Idle,
    Running,
    Paused,
    Error(String),
}

/// Anything that can wake a worker. Prompts and events share one loop.
#[derive(Debug)]
pub enum Stimulus {
    /// A prompt from a human (via the console/API), in the worker's DM.
    UserPrompt { text: String },
    /// The worker was mentioned in a channel; take a turn there.
    ChannelTurn {
        channel_id: ChannelId,
        trigger: Trigger,
    },
    /// Ask the worker loop to stop.
    Shutdown,
}

/// Everything a worker's runtime loop owns.
pub(crate) struct Runtime {
    pub id: WorkerId,
    pub agent: Agent,
    pub log: Arc<EventLog>,
    pub status: Arc<Mutex<WorkerStatus>>,
    pub store: Option<Arc<SqliteStore>>,
    pub hub: Option<Arc<ChannelHub>>,
}

/// The per-worker runtime loop. Consumes stimuli from the mailbox until it is
/// closed or a [`Stimulus::Shutdown`] arrives.
pub(crate) async fn run_worker(rt: Runtime, mut mailbox: mpsc::Receiver<Stimulus>) {
    let mut sessions = Sessions::new();
    while let Some(stimulus) = mailbox.recv().await {
        match stimulus {
            Stimulus::Shutdown => break,
            Stimulus::UserPrompt { text } => {
                sessions.switch(&rt, Conversation::Dm).await;
                dm_turn(&rt, &text).await;
            }
            Stimulus::ChannelTurn {
                channel_id,
                trigger,
            } => {
                let Some(hub) = &rt.hub else { continue };
                match hub.prepare_turn(&channel_id, &rt.id).await {
                    Ok(Some(turn)) => {
                        sessions
                            .switch(&rt, Conversation::Channel(channel_id))
                            .await;
                        channel_turn(&rt, hub, turn, &trigger).await;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("worker '{}': channel turn failed: {e:#}", rt.id),
                }
            }
        }
    }
}

/// Which conversation the agent currently holds, and (without persistence)
/// the others' messages.
struct Sessions {
    current: Conversation,
    parked: HashMap<Conversation, Vec<Message>>,
}

impl Sessions {
    fn new() -> Self {
        Self {
            current: Conversation::Dm,
            parked: HashMap::new(),
        }
    }

    /// Load `next`'s session into the agent. Sessions are persisted after
    /// every run, so with a store the database is the source of truth.
    async fn switch(&mut self, rt: &Runtime, next: Conversation) {
        if next == self.current {
            return;
        }
        let previous = std::mem::replace(&mut self.current, next.clone());
        let messages = match &rt.store {
            Some(store) => store.load_session(&rt.id, &next).await.unwrap_or_else(|e| {
                tracing::warn!("worker '{}': failed to load session: {e:#}", rt.id);
                Vec::new()
            }),
            None => {
                self.parked.insert(previous, rt.agent.messages().await);
                self.parked.remove(&next).unwrap_or_default()
            }
        };
        rt.agent.replace_messages(messages).await;
    }
}

async fn dm_turn(rt: &Runtime, text: &str) {
    set_status(rt, WorkerStatus::Running);
    rt.log.emit(WorkerEvent::StimulusReceived(format!(
        "user prompt ({} chars)",
        text.len()
    )));

    // Persist the user turn before running, then publish it with its stable
    // id so clients can merge it with fetched history.
    let user = Message::user(text);
    if let Some(store) = &rt.store
        && let Err(e) = store
            .append_messages(&rt.id, std::slice::from_ref(&user))
            .await
    {
        tracing::warn!("failed to persist user message: {e}");
    }
    rt.log.emit(WorkerEvent::UserMessage(user));

    // Drive the engine agent loop, forwarding events and persisting the
    // produced messages when the run completes.
    let (mut rx, handle) = rt.agent.prompt(text).await;
    while let Some(ev) = rx.recv().await {
        if let AgentEvent::AgentEnd { messages, .. } = &ev
            && let Some(store) = &rt.store
            && let Err(e) = store.append_messages(&rt.id, messages).await
        {
            tracing::warn!("failed to persist agent messages: {e}");
        }
        let notice = notice_for(&ev);
        rt.log.emit(WorkerEvent::Agent(ev));
        if let Some(notice) = notice {
            // Persisted so a reload still explains a failed run.
            if let Some(store) = &rt.store
                && let Err(e) = store.append_notice(&rt.id, &notice).await
            {
                tracing::warn!("failed to persist notice: {e}");
            }
            rt.log.emit(WorkerEvent::Notice(notice));
        }
    }
    let _ = handle.await;

    set_status(rt, WorkerStatus::Idle);
}

/// Take a turn in a channel: run the prompt in the channel session, stream
/// activity to the channel, and post the final reply there.
async fn channel_turn(rt: &Runtime, hub: &ChannelHub, turn: Turn, trigger: &Trigger) {
    let channel_id = turn.channel.id.as_str();
    set_status(rt, WorkerStatus::Running);
    hub.emit_worker(
        channel_id,
        &rt.id,
        WorkerEvent::StatusChanged(WorkerStatus::Running),
    );
    rt.log.emit(WorkerEvent::StimulusReceived(format!(
        "turn in #{}",
        turn.channel.name
    )));

    let conversation = Conversation::Channel(channel_id.to_string());
    let user = Message::user(&turn.prompt);
    if let Some(store) = &rt.store
        && let Err(e) = store
            .append_session(&rt.id, &conversation, std::slice::from_ref(&user))
            .await
    {
        tracing::warn!("failed to persist channel turn: {e}");
    }

    let mut reply = String::new();
    let (mut rx, handle) = rt.agent.prompt(&turn.prompt).await;
    while let Some(ev) = rx.recv().await {
        if let AgentEvent::AgentEnd { messages, .. } = &ev {
            if let Some(store) = &rt.store
                && let Err(e) = store.append_session(&rt.id, &conversation, messages).await
            {
                tracing::warn!("failed to persist channel session: {e}");
            }
            if let Some(last) = messages.iter().rev().find(|m| m.role == Role::Assistant) {
                reply = last.text();
            }
        }
        let failure = match &ev {
            AgentEvent::Error(text) => Some((NoticeLevel::Error, text.clone())),
            AgentEvent::Warning(text) => Some((NoticeLevel::Warning, text.clone())),
            _ => None,
        };
        hub.emit_worker(channel_id, &rt.id, WorkerEvent::Agent(ev));
        if let Some((level, text)) = failure {
            hub.turn_failed(channel_id, &rt.id, level, &text).await;
        }
    }
    let _ = handle.await;

    if !reply.trim().is_empty()
        && let Err(e) = hub.post_reply(channel_id, &rt.id, &reply, trigger).await
    {
        tracing::warn!("worker '{}': failed to post reply: {e:#}", rt.id);
    }
    hub.emit_worker(
        channel_id,
        &rt.id,
        WorkerEvent::StatusChanged(WorkerStatus::Idle),
    );
    set_status(rt, WorkerStatus::Idle);
}

fn notice_for(ev: &AgentEvent) -> Option<Notice> {
    match ev {
        AgentEvent::Error(text) => Some(Notice::new(NoticeLevel::Error, text)),
        AgentEvent::Warning(text) => Some(Notice::new(NoticeLevel::Warning, text)),
        _ => None,
    }
}

fn set_status(rt: &Runtime, next: WorkerStatus) {
    *rt.status.lock().unwrap() = next.clone();
    rt.log.emit(WorkerEvent::StatusChanged(next));
}
