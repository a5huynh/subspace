//! Black-box REST API tests against a real server on a random port.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{CapturingProvider, GatedProvider, spawn_server};
use reqwest::StatusCode;
use serde_json::{Value, json};
use subspace::provider::Provider;
use subspace::types::new_id;
use subspace_server::home::{Homes, MANIFEST_FILE, ROLE_FILE};
use subspace_server::store::SqliteStore;
use subspace_server::{WorkerManager, WorkerStatus};

struct Api {
    base: String,
    http: reqwest::Client,
    manager: Arc<WorkerManager>,
    data: PathBuf,
}

impl Drop for Api {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.data);
    }
}

impl Api {
    async fn new(provider: Arc<dyn Provider>) -> Self {
        let data = std::env::temp_dir().join(format!("subspace_api_test_{}", new_id()));
        fs::create_dir_all(&data).unwrap();
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
        let manager = Arc::new(WorkerManager::with_persistence(
            provider,
            store,
            Homes::open(&data).unwrap(),
        ));
        let base = spawn_server(manager.clone()).await;
        Self {
            base,
            http: reqwest::Client::new(),
            manager,
            data,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let res = self.http.get(self.url(path)).send().await.unwrap();
        (res.status(), res.json().await.unwrap_or(Value::Null))
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Value) -> (StatusCode, Value) {
        let res = self
            .http
            .request(method, self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap();
        (res.status(), res.json().await.unwrap_or(Value::Null))
    }

    async fn create(&self, body: Value) -> String {
        let (status, worker) = self.send(reqwest::Method::POST, "/api/workers", body).await;
        assert_eq!(status, StatusCode::CREATED, "{worker}");
        worker["config"]["id"].as_str().unwrap().to_string()
    }

    /// Poll until the worker reports `state`.
    async fn wait_for_state(&self, id: &str, state: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (_, w) = self.get(&format!("/api/workers/{id}")).await;
                if w["status"]["state"] == state {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("worker never became {state}"));
    }
}

fn ok_provider() -> Arc<dyn Provider> {
    Arc::new(CapturingProvider::with_text("hello back"))
}

#[tokio::test]
async fn health_reports_the_epoch() {
    let api = Api::new(ok_provider()).await;
    let (status, body) = api.get("/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "ok", "epoch": api.manager.epoch()}));
}

#[tokio::test]
async fn create_get_and_list_workers() {
    let api = Api::new(ok_provider()).await;
    let (status, body) = api.get("/api/workers").await;
    assert_eq!((status, body), (StatusCode::OK, json!([])));

    let id = api
        .create(json!({
            "name": "  Secretary ",
            "description": "Triages email",
            "role": "You manage the inbox.",
            "thinking": "high",
            "skills": {"disabled": ["noisy"]}
        }))
        .await;

    let (status, w) = api.get(&format!("/api/workers/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(w["config"]["name"], "Secretary", "name is trimmed");
    assert_eq!(w["config"]["thinking"], "high");
    assert_eq!(w["config"]["skills"]["disabled"], json!(["noisy"]));
    assert!(
        w["config"]["model"].as_str().is_some_and(|m| !m.is_empty()),
        "default model"
    );
    assert_eq!(w["status"], json!({"state": "idle"}));

    let (_, list) = api.get("/api/workers").await;
    assert_eq!(
        list,
        json!([{"id": id, "name": "Secretary", "description": "Triages email", "status": {"state": "idle"}}])
    );

    let home = api.data.join("workers").join(&id);
    assert_eq!(
        fs::read_to_string(home.join(ROLE_FILE)).unwrap(),
        "You manage the inbox."
    );
}

#[tokio::test]
async fn invalid_requests_are_json_400s() {
    let api = Api::new(ok_provider()).await;
    for body in [
        json!({"name": "  "}),
        json!({"name": "x", "model": ""}),
        json!({"role": "no name"}),
    ] {
        let (status, err) = api
            .send(reqwest::Method::POST, "/api/workers", body.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            err["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{err}"
        );
    }

    let res = api
        .http
        .post(api.url("/api/workers"))
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(res.json::<Value>().await.unwrap()["error"].is_string());

    let id = api.create(json!({"name": "A"})).await;
    let (status, err) = api
        .send(
            reqwest::Method::POST,
            &format!("/api/workers/{id}/prompt"),
            json!({"text": " \n"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"], "prompt text must not be empty");
}

#[tokio::test]
async fn unknown_workers_are_json_404s() {
    let api = Api::new(ok_provider()).await;
    let base = "/api/workers/nope";
    let cases = [
        (reqwest::Method::GET, base.to_string(), Value::Null),
        (
            reqwest::Method::PATCH,
            base.to_string(),
            json!({"name": "x"}),
        ),
        (reqwest::Method::DELETE, base.to_string(), Value::Null),
        (
            reqwest::Method::POST,
            format!("{base}/prompt"),
            json!({"text": "hi"}),
        ),
        (reqwest::Method::GET, format!("{base}/history"), Value::Null),
    ];
    for (method, path, body) in cases {
        let (status, err) = api.send(method.clone(), &path, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(err["error"], "no such worker: nope", "{method} {path}");
    }
}

#[tokio::test]
async fn prompt_runs_and_history_returns_messages() {
    let api = Api::new(ok_provider()).await;
    let id = api.create(json!({"name": "Secretary"})).await;

    let (status, _) = api
        .send(
            reqwest::Method::POST,
            &format!("/api/workers/{id}/prompt"),
            json!({"text": "hello"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // The run is over once both messages are persisted *and* the worker is
    // idle again (it is idle before the run starts, too). Mid-run the cursor
    // deliberately points before the run.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, h) = api.get(&format!("/api/workers/{id}/history")).await;
            if h["messages"].as_array().is_some_and(|m| m.len() >= 2) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("history never filled in");
    api.wait_for_state(&id, "idle").await;
    let (status, h) = api.get(&format!("/api/workers/{id}/history")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h["epoch"], api.manager.epoch());
    let messages = h["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{h}");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(
        messages[0]["content"][0],
        json!({"type": "text", "text": "hello"})
    );
    assert_eq!(messages[1]["role"], "assistant");

    let (replay, _) = api.manager.subscribe_worker(&id, 0).unwrap();
    let subspace_server::Replay::Events(events) = replay else {
        panic!("gap");
    };
    assert_eq!(
        h["cursor"].as_u64(),
        events.last().map(|e| e.seq),
        "idle: cursor is the latest event"
    );
}

#[tokio::test]
async fn patch_updates_the_home_and_is_rejected_mid_run() {
    let provider = Arc::new(GatedProvider::with_text("ok"));
    let api = Api::new(provider.clone()).await;
    let id = api
        .create(json!({"name": "Secretary", "role": "old"}))
        .await;

    let (status, w) = api
        .send(
            reqwest::Method::PATCH,
            &format!("/api/workers/{id}"),
            json!({"name": "Chief of Staff", "role": "new persona"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{w}");
    assert_eq!(w["config"]["name"], "Chief of Staff");
    assert_eq!(
        w["config"]["thinking"], "low",
        "unspecified fields are kept"
    );
    let home = api.data.join("workers").join(&id);
    assert_eq!(
        fs::read_to_string(home.join(ROLE_FILE)).unwrap(),
        "new persona"
    );
    assert!(
        fs::read_to_string(home.join(MANIFEST_FILE))
            .unwrap()
            .contains("Chief of Staff")
    );

    // Start a run that the gated provider holds open.
    api.send(
        reqwest::Method::POST,
        &format!("/api/workers/{id}/prompt"),
        json!({"text": "hi"}),
    )
    .await;
    api.wait_for_state(&id, "running").await;
    let (status, err) = api
        .send(
            reqwest::Method::PATCH,
            &format!("/api/workers/{id}"),
            json!({"name": "x"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err["error"].as_str().unwrap().contains("busy"), "{err}");

    provider.release();
    api.wait_for_state(&id, "idle").await;
    assert_eq!(api.manager.status(&id), Some(WorkerStatus::Idle));
}

#[tokio::test]
async fn delete_removes_the_worker_and_its_home() {
    let api = Api::new(ok_provider()).await;
    let id = api.create(json!({"name": "Temp"})).await;
    let home = api.data.join("workers").join(&id);
    assert!(home.exists());

    let res = api
        .http
        .delete(api.url(&format!("/api/workers/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert!(!home.exists());
    let (status, _) = api.get(&format!("/api/workers/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
