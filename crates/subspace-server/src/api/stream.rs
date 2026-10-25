//! `GET /api/workers/{id}/stream` — a WebSocket of one worker's events.
//!
//! Protocol (server → client, JSON text frames, see [`StreamMessage`]):
//! 1. `hello` with the server epoch and the cursor the stream starts after.
//! 2. `event` frames: buffered events with `seq > since`, then live ones.
//! 3. `resync` (then close) when the client can't be caught up — stale
//!    epoch, evicted or future cursor, or it fell behind the live bus. The
//!    client should refetch history and reconnect with its fresh cursor.
//!
//! Query: `since` (resume after this seq; default: the worker's cursor) and
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
use crate::event::{Replay, ServerEvent};
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

    let since = match (query.since, query.epoch.as_deref()) {
        (None, _) => cursor,
        (Some(since), Some(epoch)) if epoch == manager.epoch() => since,
        (Some(_), Some(_)) => {
            let reason = "the server restarted; refetch history and reconnect".to_string();
            return Ok(ws.on_upgrade(move |socket| resync_and_close(socket, reason)));
        }
        (Some(_), None) => {
            let reason = "`since` requires the `epoch` it came from".to_string();
            return Ok(ws.on_upgrade(move |socket| resync_and_close(socket, reason)));
        }
    };

    let (replay, rx) = manager.subscribe_worker(&id, since)?;
    let epoch = manager.epoch().to_string();
    Ok(ws.on_upgrade(move |socket| run(socket, epoch, id, since, replay, rx)))
}

async fn run(
    mut socket: WebSocket,
    epoch: String,
    id: WorkerId,
    since: u64,
    replay: Replay,
    mut rx: tokio::sync::broadcast::Receiver<ServerEvent>,
) {
    let events = match replay {
        Replay::Events(events) => events,
        Replay::Gap => {
            let reason = format!("events after seq {since} are no longer available");
            return resync_and_close(socket, reason).await;
        }
    };

    let hello = StreamMessage::Hello {
        epoch,
        worker_id: id.clone(),
        since,
    };
    if send(&mut socket, &hello).await.is_err() {
        return;
    }
    let mut last_sent = since;
    for event in events {
        last_sent = event.seq;
        if send(&mut socket, &StreamMessage::Event { event })
            .await
            .is_err()
        {
            return;
        }
    }

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(event) if event.worker_id == id && event.seq > last_sent => {
                    last_sent = event.seq;
                    if send(&mut socket, &StreamMessage::Event { event }).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    let reason = "the stream fell behind and dropped events".to_string();
                    return resync_and_close(socket, reason).await;
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

async fn resync_and_close(mut socket: WebSocket, reason: String) {
    let _ = send(&mut socket, &StreamMessage::Resync { reason }).await;
    let _ = socket.send(WsMessage::Close(None)).await;
}

async fn send(socket: &mut WebSocket, msg: &StreamMessage) -> Result<(), axum::Error> {
    let text = serde_json::to_string(msg).expect("stream messages serialize");
    socket.send(WsMessage::Text(text.into())).await
}
