//! Workspace/channel REST and the channel WebSocket, against a real server.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{ScriptedProvider, spawn_server};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use subspace::types::new_id;
use subspace_server::store::SqliteStore;
use subspace_server::{Homes, WorkerManager};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Server {
    base: String,
    http: Client,
    data: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.data);
    }
}

async fn server() -> Server {
    let provider = Arc::new(ScriptedProvider::new(|r| {
        match r.system_prompt.as_deref() {
            Some("lead") => "On it. @helper can you check?".into(),
            _ => "Checked.".into(),
        }
    }));
    let data = std::env::temp_dir().join(format!("subspace_api_channels_{}", new_id()));
    fs::create_dir_all(&data).unwrap();
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
    let manager = Arc::new(WorkerManager::with_persistence(
        provider,
        store,
        Homes::open(&data).unwrap(),
    ));
    Server {
        base: spawn_server(manager).await,
        http: Client::new(),
        data,
    }
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let res = self.http.get(self.url(path)).send().await.unwrap();
        (res.status(), res.json().await.unwrap_or(Value::Null))
    }

    async fn send(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let req = match method {
            "POST" => self.http.post(self.url(path)),
            "PATCH" => self.http.patch(self.url(path)),
            "DELETE" => self.http.delete(self.url(path)),
            _ => unreachable!(),
        };
        let res = req.json(&body).send().await.unwrap();
        (res.status(), res.json().await.unwrap_or(Value::Null))
    }

    async fn worker(&self, name: &str, role: &str) -> String {
        let (status, body) = self
            .send("POST", "/api/workers", json!({"name": name, "role": role}))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["config"]["id"].as_str().unwrap().to_string()
    }

    async fn general(&self) -> String {
        let (_, body) = self.get("/api/workspaces").await;
        body[0]["id"].as_str().unwrap().to_string()
    }

    async fn connect(&self, channel: &str, query: &str) -> Ws {
        let url = format!(
            "{}/api/channels/{channel}/stream{query}",
            self.base.replace("http", "ws")
        );
        connect_async(url).await.expect("handshake").0
    }
}

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

/// Event frames until `count` posts have been seen and every worker that
/// started a turn has gone idle again.
async fn events_until_posts(ws: &mut Ws, count: usize) -> Vec<Value> {
    let mut events = Vec::new();
    let mut posts = 0;
    let mut running = 0i32;
    while let Some(frame) = next(ws).await {
        assert_eq!(frame["type"], "event", "{frame}");
        let inner = &frame["event"]["inner"];
        match inner["type"].as_str() {
            Some("post") => posts += 1,
            Some("worker") if inner["data"]["event"]["type"] == "status_changed" => {
                if inner["data"]["event"]["data"]["state"] == "running" {
                    running += 1;
                } else {
                    running -= 1;
                }
            }
            _ => {}
        }
        events.push(frame["event"].clone());
        if posts >= count && running == 0 {
            break;
        }
    }
    events
}

#[tokio::test]
async fn workspace_and_channel_crud() {
    let s = server().await;
    let ws = s.general().await;
    let alpha = s.worker("Alpha", "lead").await;

    let (status, detail) = s.get(&format!("/api/workspaces/{ws}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["workspace"]["name"], "General");
    assert_eq!(detail["members"], json!([alpha]));
    assert_eq!(detail["channels"], json!([]));

    let (status, channel) = s
        .send(
            "POST",
            &format!("/api/workspaces/{ws}/channels"),
            json!({"name": "Launch Plan", "topic": "ship it", "members": [alpha]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{channel}");
    assert_eq!(channel["name"], "launch-plan");
    let id = channel["id"].as_str().unwrap();

    let (status, body) = s
        .send(
            "POST",
            &format!("/api/workspaces/{ws}/channels"),
            json!({"name": "launch plan"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (_, detail) = s.get(&format!("/api/channels/{id}")).await;
    assert_eq!(detail["channel"]["topic"], "ship it");
    assert_eq!(detail["members"][0]["handle"], "alpha");
    assert_eq!(detail["posts"], json!([]));
    assert!(detail["epoch"].is_string() && detail["cursor"].is_number());

    let (status, updated) = s
        .send(
            "PATCH",
            &format!("/api/channels/{id}"),
            json!({"topic": "v2"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (updated["name"].clone(), updated["topic"].clone()),
        (json!("launch-plan"), json!("v2"))
    );

    let (status, _) = s
        .send(
            "DELETE",
            &format!("/api/channels/{id}/members/{alpha}"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = s
        .send(
            "POST",
            &format!("/api/channels/{id}/members"),
            json!({"worker_id": alpha}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = s
        .send("DELETE", &format!("/api/channels/{id}"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = s.get(&format!("/api/channels/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("no such channel"));
}

#[tokio::test]
async fn errors_are_json_with_the_right_status() {
    let s = server().await;
    let ws = s.general().await;
    let alpha = s.worker("Alpha", "lead").await;
    let (_, channel) = s
        .send(
            "POST",
            &format!("/api/workspaces/{ws}/channels"),
            json!({"name": "c"}),
        )
        .await;
    let id = channel["id"].as_str().unwrap();

    let cases = [
        (
            "GET",
            "/api/workspaces/nope".to_string(),
            Value::Null,
            StatusCode::NOT_FOUND,
        ),
        (
            "POST",
            format!("/api/channels/{id}/posts"),
            json!({"text": "  "}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/channels/{id}/posts"),
            json!({}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            format!("/api/channels/{id}/members"),
            json!({"worker_id": "ghost"}),
            StatusCode::NOT_FOUND,
        ),
        (
            "POST",
            format!("/api/workspaces/{ws}/channels"),
            json!({"name": "!!"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/api/channels/nope/posts".to_string(),
            json!({"text": "hi"}),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (method, path, body, expected) in cases {
        let (status, body) = if method == "GET" {
            s.get(&path).await
        } else {
            s.send(method, &path, body).await
        };
        assert_eq!(status, expected, "{method} {path}: {body}");
        assert!(body["error"].is_string(), "{method} {path}: {body}");
    }

    // A worker removed from the workspace can't join its channels.
    let (status, _) = s
        .send(
            "DELETE",
            &format!("/api/workspaces/{ws}/members/{alpha}"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = s
        .send(
            "POST",
            &format!("/api/channels/{id}/members"),
            json!({"worker_id": alpha}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn posting_streams_the_conversation_and_resuming_replays_it() {
    let s = server().await;
    let ws = s.general().await;
    let lead = s.worker("Lead", "lead").await;
    let helper = s.worker("Helper", "helper").await;
    let (_, channel) = s
        .send(
            "POST",
            &format!("/api/workspaces/{ws}/channels"),
            json!({"name": "work", "members": [lead, helper]}),
        )
        .await;
    let id = channel["id"].as_str().unwrap().to_string();
    let (_, detail) = s.get(&format!("/api/channels/{id}")).await;
    let epoch = detail["epoch"].as_str().unwrap().to_string();

    let mut stream = s.connect(&id, "").await;
    let hello = next(&mut stream).await.unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["channel_id"], id);

    let (status, post) = s
        .send(
            "POST",
            &format!("/api/channels/{id}/posts"),
            json!({"text": "@lead go"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{post}");
    assert_eq!(post["author"], json!({"kind": "operator"}));
    assert_eq!(post["mentions"], json!([lead]));

    let events = events_until_posts(&mut stream, 3).await;
    let posts: Vec<_> = events
        .iter()
        .filter(|e| e["inner"]["type"] == "post")
        .map(|e| {
            (
                e["inner"]["data"]["author_name"].clone(),
                e["inner"]["data"]["text"].clone(),
            )
        })
        .collect();
    assert_eq!(
        posts,
        [
            (json!("You"), json!("@lead go")),
            (json!("Lead"), json!("On it. @helper can you check?")),
            (json!("Helper"), json!("Checked.")),
        ]
    );
    // Workers' live activity is attributed to them.
    assert!(events.iter().any(|e| e["inner"]["type"] == "worker"
        && e["inner"]["data"]["worker_id"] == helper
        && e["inner"]["data"]["event"]["data"]["type"] == "text_delta"));

    // Resuming from the start replays the same events.
    let mut resumed = s.connect(&id, &format!("?since=0&epoch={epoch}")).await;
    assert_eq!(next(&mut resumed).await.unwrap()["type"], "hello");
    let replayed = events_until_posts(&mut resumed, 3).await;
    let seqs = |ev: &[Value]| {
        ev.iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(seqs(&replayed), seqs(&events));

    // History has the same posts.
    let (_, detail) = s.get(&format!("/api/channels/{id}")).await;
    assert_eq!(detail["posts"].as_array().unwrap().len(), 3);

    // A stale epoch resyncs.
    let mut stale = s.connect(&id, "?since=1&epoch=old").await;
    assert_eq!(next(&mut stale).await.unwrap()["type"], "resync");
}

#[tokio::test]
async fn deleting_a_channel_tells_its_stream() {
    let s = server().await;
    let ws = s.general().await;
    let (_, channel) = s
        .send(
            "POST",
            &format!("/api/workspaces/{ws}/channels"),
            json!({"name": "tmp"}),
        )
        .await;
    let id = channel["id"].as_str().unwrap().to_string();
    let mut stream = s.connect(&id, "").await;
    assert_eq!(next(&mut stream).await.unwrap()["type"], "hello");

    s.send("DELETE", &format!("/api/channels/{id}"), Value::Null)
        .await;
    let frame = next(&mut stream).await.unwrap();
    assert_eq!(frame["event"]["inner"]["type"], "deleted", "{frame}");

    // Unknown channels fail the handshake.
    let url = format!("{}/api/channels/{id}/stream", s.base.replace("http", "ws"));
    assert!(connect_async(url).await.is_err());
}
