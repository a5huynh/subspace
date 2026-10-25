//! Test helpers shared across integration tests.

#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use subspace::agent::AgentEvent;
use subspace::provider::mock::{MockProvider, MockResponse};
use subspace::provider::{CompletionRequest, CompletionStream, Provider};
use subspace_server::{WorkerEvent, WorkerManager, WorkerStatus};

/// Records every completion request, then delegates to a [`MockProvider`].
pub struct CapturingProvider {
    inner: MockProvider,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl CapturingProvider {
    pub fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            inner: MockProvider::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn with_text(s: &str) -> Self {
        Self::new(vec![MockResponse::text(s)])
    }

    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

// `Provider` is an `#[async_trait]`; implementing its desugared form avoids a
// test-only dependency on `async-trait`.
impl Provider for CapturingProvider {
    fn name(&self) -> &str {
        "capturing"
    }

    fn complete<'a, 'b>(
        &'a self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<CompletionStream>> + Send + 'b>>
    where
        'a: 'b,
        Self: 'b,
    {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.complete(request)
    }
}

/// Holds every completion until [`GatedProvider::release`] is called, so a
/// test can observe a worker mid-run.
pub struct GatedProvider {
    inner: MockProvider,
    gate: tokio::sync::Semaphore,
}

impl GatedProvider {
    pub fn with_text(s: &str) -> Self {
        Self {
            inner: MockProvider::with_text(s),
            gate: tokio::sync::Semaphore::new(0),
        }
    }

    /// Let one pending (or future) completion proceed.
    pub fn release(&self) {
        self.gate.add_permits(1);
    }
}

impl Provider for GatedProvider {
    fn name(&self) -> &str {
        "gated"
    }

    fn complete<'a, 'b>(
        &'a self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<CompletionStream>> + Send + 'b>>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            self.gate.acquire().await.unwrap().forget();
            self.inner.complete(request).await
        })
    }
}

/// Serve the API for `manager` on a random localhost port; returns the base URL.
pub async fn spawn_server(manager: Arc<WorkerManager>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = subspace_server::api::router(manager);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Prompt a worker and wait for its run to finish: the run's `AgentEnd`
/// followed by the worker returning to idle (after history is persisted).
pub async fn prompt_and_wait(manager: &WorkerManager, id: &str, text: &str) {
    let mut events = manager.subscribe();
    manager.prompt(id, text).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut ended = false;
        while let Ok(ev) = events.recv().await {
            if ev.worker_id != id {
                continue;
            }
            match ev.inner {
                WorkerEvent::Agent(AgentEvent::AgentEnd { .. }) => ended = true,
                WorkerEvent::StatusChanged(WorkerStatus::Idle) if ended => break,
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for the run to finish");
}
