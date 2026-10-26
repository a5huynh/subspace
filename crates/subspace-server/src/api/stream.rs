//! WebSockets of one worker's events (`GET /api/workers/{id}/stream`) or one
//! channel's (`GET /api/channels/{id}/stream`).
//!
//! Protocol (server → client, JSON text frames, see [`StreamMessage`] and
//! [`ChannelStreamMessage`]):
//! 1. `hello` with the server epoch and the cursor the stream starts after.
//! 2. `event` frames: buffered events with `seq > since`, then live ones.
//! 3. `resync` (then close) when the client can't be caught up — stale
//!    epoch, evicted or future cursor, or it fell behind the live bus. The
//!    client should refetch history and reconnect with its fresh cursor.
//!
//! Query: `since` (resume after this seq; default: the current cursor) and
//! `epoch` (the epoch `since` came from; required for `since` to be trusted).

use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use ts_rs::TS;

use super::{ApiResult, AppState};
use crate::WorkerId;
use crate::event::{ChannelEvent, LogEvent, Replay, ServerEvent};
use crate::manager::ManagerError;

const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
pub(super) struct StreamQuery {
    since: Option<u64>,
    epoch: Option<String>,
}

/// A frame sent from the server on a worker stream.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamMessage {
    /// First frame: the stream replays and then streams events after `since`.
    Hello {
        epoch: String,
        worker_id: WorkerId,
        since: u64,
    },
    Event {
        event: ServerEvent,
    },
    /// The client must refetch history and reconnect; the server closes next.
    Resync {
        reason: String,
    },
}

/// A frame sent from the server on a channel stream (same protocol as
/// [`StreamMessage`]).
#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelStreamMessage {
    Hello {
        epoch: String,
        channel_id: String,
        since: u64,
    },
    Event {
        event: ChannelEvent,
    },
    Resync {
        reason: String,
    },
}

/// How one stream's frames are built and its events attributed.
trait Frames: Send + 'static {
    type Event: LogEvent;
    type Message: Serialize;

    fn hello(&self, epoch: String, since: u64) -> Self::Message;
    fn event(event: Self::Event) -> Self::Message;
    fn resync(reason: String) -> Self::Message;
    /// Whether a bus event belongs to this stream.
    fn owns(&self, event: &Self::Event) -> bool;
}

struct WorkerFrames(WorkerId);

impl Frames for WorkerFrames {
    type Event = ServerEvent;
    type Message = StreamMessage;

    fn hello(&self, epoch: String, since: u64) -> StreamMessage {
        StreamMessage::Hello {
            epoch,
            worker_id: self.0.clone(),
            since,
        }
    }
    fn event(event: ServerEvent) -> StreamMessage {
        StreamMessage::Event { event }
    }
    fn resync(reason: String) -> StreamMessage {
        StreamMessage::Resync { reason }
    }
    fn owns(&self, event: &ServerEvent) -> bool {
        event.worker_id == self.0
    }
}

struct ChannelFrames(String);

impl Frames for ChannelFrames {
    type Event = ChannelEvent;
    type Message = ChannelStreamMessage;

    fn hello(&self, epoch: String, since: u64) -> ChannelStreamMessage {
        ChannelStreamMessage::Hello {
            epoch,
            channel_id: self.0.clone(),
            since,
        }
    }
    fn event(event: ChannelEvent) -> ChannelStreamMessage {
        ChannelStreamMessage::Event { event }
    }
    fn resync(reason: String) -> ChannelStreamMessage {
        ChannelStreamMessage::Resync { reason }
    }
    fn owns(&self, event: &ChannelEvent) -> bool {
        event.channel_id == self.0
    }
}

/// Where a client asked to resume from: a trusted `since`, or a reason it
/// must resync instead.
fn resume_point(query: &StreamQuery, epoch: &str, cursor: u64) -> Result<u64, String> {
    match (query.since, query.epoch.as_deref()) {
        (None, _) => Ok(cursor),
        (Some(since), Some(e)) if e == epoch => Ok(since),
        (Some(_), Some(_)) => Err("the server restarted; refetch history and reconnect".into()),
        (Some(_), None) => Err("`since` requires the `epoch` it came from".into()),
    }
}

pub(super) async fn stream(
    State(manager): State<AppState>,
    Path(id): Path<WorkerId>,
    Query(query): Query<StreamQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    // Resolve the worker before upgrading, so unknown ids are plain 404s.
    let cursor = manager
        .history_cursor(&id)
        .ok_or_else(|| anyhow::Error::from(ManagerError::NotFound(id.clone())))?;
    let frames = WorkerFrames(id.clone());
    let since = match resume_point(&query, manager.epoch(), cursor) {
        Ok(since) => since,
        Err(reason) => {
            return Ok(ws.on_upgrade(move |s| resync_and_close::<WorkerFrames>(s, reason)));
        }
    };
    let (replay, rx) = manager.subscribe_worker(&id, since)?;
    let epoch = manager.epoch().to_string();
    Ok(ws.on_upgrade(move |socket| run(socket, frames, epoch, since, replay, rx)))
}

pub(super) async fn channel_stream(
    State(manager): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<StreamQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let hub = manager.channels()?;
    hub.channel(&id).await?;
    let frames = ChannelFrames(id.clone());
    let since = match resume_point(&query, manager.epoch(), hub.log(&id).cursor()) {
        Ok(since) => since,
        Err(reason) => {
            return Ok(ws.on_upgrade(move |s| resync_and_close::<ChannelFrames>(s, reason)));
        }
    };
    let (replay, rx) = hub.subscribe(&id, since);
    let epoch = manager.epoch().to_string();
    Ok(ws.on_upgrade(move |socket| run(socket, frames, epoch, since, replay, rx)))
}

async fn run<F: Frames>(
    mut socket: WebSocket,
    frames: F,
    epoch: String,
    since: u64,
    replay: Replay<F::Event>,
    mut rx: tokio::sync::broadcast::Receiver<F::Event>,
) {
    let events = match replay {
        Replay::Events(events) => events,
        Replay::Gap => {
            let reason = format!("events after seq {since} are no longer available");
            return resync_and_close::<F>(socket, reason).await;
        }
    };

    if send(&mut socket, &frames.hello(epoch, since))
        .await
        .is_err()
    {
        return;
    }
    let mut last_sent = since;
    for event in events {
        last_sent = event.seq();
        if send(&mut socket, &F::event(event)).await.is_err() {
            return;
        }
    }

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(event) if frames.owns(&event) && event.seq() > last_sent => {
                    last_sent = event.seq();
                    if send(&mut socket, &F::event(event)).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    let reason = "the stream fell behind and dropped events".to_string();
                    return resync_and_close::<F>(socket, reason).await;
                }
                Err(RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                // Clients don't send anything meaningful; just notice closes.
                Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if socket.send(WsMessage::Ping(Default::default())).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn resync_and_close<F: Frames>(mut socket: WebSocket, reason: String) {
    let _ = send(&mut socket, &F::resync(reason)).await;
    let _ = socket.send(WsMessage::Close(None)).await;
}

async fn send(socket: &mut WebSocket, msg: &impl Serialize) -> Result<(), axum::Error> {
    let text = serde_json::to_string(msg).expect("stream messages serialize");
    socket.send(WsMessage::Text(text.into())).await
}
