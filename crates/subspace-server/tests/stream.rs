//! WebSocket stream tests against a real server on a random port.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{CapturingProvider, GatedProvider, spawn_server};
use futures_util::StreamExt;
use serde_json::Value;
use subspace::provider::Provider;
use subspace_server::{WorkerConfig, WorkerManager};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Server {
    base: String,
    manager: Arc<WorkerManager>,
}

async fn server(provider: Arc<dyn Provider>) -> Server {
    let manager = Arc::new(WorkerManager::new(provider));
    let base = spawn_server(manager.clone()).await;
    Server { base, manager }
}

impl Server {
    async fn connect(&self, id: &str, query: &str) -> Ws {
        let url = format!(
            "{}/api/workers/{id}/stream{query}",
            self.base.replace("http", "ws")
        );
        connect_async(url).await.expect("handshake").0
    }
}

/// Next JSON text frame, skipping pings.
async fn next(ws: &mut Ws) -> Option<Value> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for a frame")?
            .ok()?;
        match msg {
            Message::Text(text) => return Some(serde_json::from_str(&text).unwrap()),
            Message::Close(_) => return None,
            _ => {}
        }
    }
}

/// Event frames up to and including the worker returning to idle after a run.
async fn events_until_idle(ws: &mut Ws) -> Vec<Value> {
    let mut events = Vec::new();
    let mut ended = false;
    while let Some(frame) = next(ws).await {
        assert_eq!(frame["type"], "event", "{frame}");
        let event = frame["event"].clone();
        let inner = &event["inner"];
        if inner["type"] == "agent" && inner["data"]["type"] == "agent_end" {
            ended = true;
        }
        let idle = inner["type"] == "status_changed" && inner["data"]["state"] == "idle";
        events.push(event);
        if ended && idle {
            break;
        }
    }
    events
}

fn seqs(events: &[Value]) -> Vec<u64> {
    events.iter().map(|e| e["seq"].as_u64().unwrap()).collect()
}

fn ok_provider() -> Arc<dyn Provider> {
    Arc::new(CapturingProvider::with_text("hello back"))
}

#[tokio::test]
async fn hello_then_live_events_for_this_worker_only() {
    let s = server(ok_provider()).await;
    let id = s
        .manager
        .spawn(WorkerConfig::new("A", "role"))
        .await
        .unwrap();
    let other = s
        .manager
        .spawn(WorkerConfig::new("B", "role"))
        .await
        .unwrap();

    let mut ws = s.connect(&id, "").await;
    let hello = next(&mut ws).await.unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["epoch"], s.manager.epoch());
    assert_eq!(hello["worker_id"], id.as_str());
    assert_eq!(hello["since"], 0);

    s.manager.prompt(&other, "not for you").await.unwrap();
    s.manager.prompt(&id, "hello").await.unwrap();
    let events = events_until_idle(&mut ws).await;

    assert!(events.iter().all(|e| e["worker_id"] == id.as_str()));
    let seqs = seqs(&events);
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<_>>(),
        "contiguous from 1"
    );
    let kinds: Vec<String> = events
        .iter()
        .map(|e| {
            let inner = &e["inner"];
            match inner["type"].as_str().unwrap() {
                "agent" => format!("agent:{}", inner["data"]["type"].as_str().unwrap()),
                t => t.to_string(),
            }
        })
        .collect();
    assert_eq!(kinds.first().unwrap(), "status_changed");
    assert!(kinds.contains(&"user_message".to_string()));
    assert!(kinds.contains(&"agent:text_delta".to_string()));
    assert!(kinds.contains(&"agent:agent_end".to_string()));
}

#[tokio::test]
async fn resuming_from_a_cursor_replays_exactly_what_was_missed() {
    let s = server(Arc::new(CapturingProvider::new(vec![
        subspace::provider::mock::MockResponse::text("one"),
        subspace::provider::mock::MockResponse::text("two"),
    ])))
    .await;
    let id = s
        .manager
        .spawn(WorkerConfig::new("A", "role"))
        .await
        .unwrap();

    let mut ws = s.connect(&id, "").await;
    next(&mut ws).await.unwrap();
    s.manager.prompt(&id, "first").await.unwrap();
    let first = events_until_idle(&mut ws).await;
    let cursor = *seqs(&first).last().unwrap();
    drop(ws);

    // Missed while disconnected.
    let mut watcher = s.connect(&id, "").await;
    next(&mut watcher).await.unwrap();
    s.manager.prompt(&id, "second").await.unwrap();
    let missed = events_until_idle(&mut watcher).await;

    let epoch = s.manager.epoch().to_string();
    let mut ws = s
        .connect(&id, &format!("?since={cursor}&epoch={epoch}"))
        .await;
    let hello = next(&mut ws).await.unwrap();
    assert_eq!(hello["since"], cursor);
    let replayed = events_until_idle(&mut ws).await;
    assert_eq!(seqs(&replayed), seqs(&missed));
    assert_eq!(replayed.first().unwrap()["seq"], cursor + 1);
}

#[tokio::test]
async fn joining_mid_run_without_since_replays_the_whole_run() {
    let provider = Arc::new(GatedProvider::with_text("slow"));
    let s = server(provider.clone()).await;
    let id = s
        .manager
        .spawn(WorkerConfig::new("A", "role"))
        .await
        .unwrap();

    s.manager.prompt(&id, "hi").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.manager.status(&id) != Some(subspace_server::WorkerStatus::Running) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    let mut ws = s.connect(&id, "").await;
    assert_eq!(next(&mut ws).await.unwrap()["since"], 0);
    provider.release();
    let events = events_until_idle(&mut ws).await;
    assert_eq!(
        events.first().unwrap()["seq"],
        1,
        "includes the run's start"
    );
}

#[tokio::test]
async fn unresumable_cursors_get_a_resync_then_close() {
    let s = server(ok_provider()).await;
    let id = s
        .manager
        .spawn(WorkerConfig::new("A", "role"))
        .await
        .unwrap();
    let epoch = s.manager.epoch().to_string();

    for query in [
        "?since=0&epoch=some-old-epoch".to_string(),
        "?since=0".to_string(),
        format!("?since=999&epoch={epoch}"),
    ] {
        let mut ws = s.connect(&id, &query).await;
        let frame = next(&mut ws).await.unwrap();
        assert_eq!(frame["type"], "resync", "{query}: {frame}");
        assert!(frame["reason"].as_str().is_some_and(|r| !r.is_empty()));
        assert!(
            next(&mut ws).await.is_none(),
            "{query}: closed after resync"
        );
    }
}

#[tokio::test]
async fn unknown_workers_fail_the_handshake_with_404() {
    let s = server(ok_provider()).await;
    let url = format!("{}/api/workers/nope/stream", s.base.replace("http", "ws"));
    match connect_async(url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => {
            assert_eq!(res.status(), 404)
        }
        other => panic!("expected an HTTP 404, got {other:?}"),
    }
}

#[tokio::test]
async fn the_stream_survives_a_worker_update() {
    let s = server(Arc::new(CapturingProvider::new(vec![
        subspace::provider::mock::MockResponse::text("one"),
        subspace::provider::mock::MockResponse::text("two"),
    ])))
    .await;
    let mut cfg = WorkerConfig::new("A", "role");
    let id = s.manager.spawn(cfg.clone()).await.unwrap();

    let mut ws = s.connect(&id, "").await;
    next(&mut ws).await.unwrap();
    s.manager.prompt(&id, "first").await.unwrap();
    let before = events_until_idle(&mut ws).await;

    cfg.role = "new role".into();
    s.manager.update(cfg).await.unwrap();
    s.manager.prompt(&id, "second").await.unwrap();
    let after = events_until_idle(&mut ws).await;
    assert_eq!(
        after.first().unwrap()["seq"].as_u64().unwrap(),
        seqs(&before).last().unwrap() + 1,
        "same connection, seq continues"
    );
}
