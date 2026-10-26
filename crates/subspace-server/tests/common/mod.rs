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

/// Fails the first `failures` completions with "provider exploded", then
/// delegates to a [`CapturingProvider`].
pub struct FlakyProvider {
    pub inner: CapturingProvider,
    failures: Mutex<usize>,
}

impl FlakyProvider {
    pub fn new(failures: usize, then: &str) -> Self {
        Self {
            inner: CapturingProvider::with_text(then),
            failures: Mutex::new(failures),
        }
    }
}

impl Provider for FlakyProvider {
    fn name(&self) -> &str {
        "flaky"
    }

    fn complete<'a, 'b>(
        &'a self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<CompletionStream>> + Send + 'b>>
    where
        'a: 'b,
        Self: 'b,
    {
        let failing = {
            let mut left = self.failures.lock().unwrap();
            let failing = *left > 0;
            *left = left.saturating_sub(1);
            failing
        };
        if failing {
            Box::pin(async { Err(anyhow::anyhow!("provider exploded")) })
        } else {
            self.inner.complete(request)
        }
    }
}

/// Answers each completion with `script(request)`, so replies can depend on
/// who is asked (system prompt) and what (last user message). Records requests.
pub struct ScriptedProvider {
    script: Box<dyn Fn(&CompletionRequest) -> String + Send + Sync>,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl ScriptedProvider {
    pub fn new(script: impl Fn(&CompletionRequest) -> String + Send + Sync + 'static) -> Self {
        Self {
            script: Box::new(script),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Requests made by the worker whose system prompt starts with `role`.
    pub fn requests_from(&self, role: &str) -> Vec<CompletionRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.system_prompt.as_deref().unwrap_or("").starts_with(role))
            .collect()
    }
}

impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
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
        let reply = (self.script)(&request);
        Box::pin(async move { MockProvider::with_text(reply).complete(request).await })
    }
}

/// The text of a request's last user message.
pub fn last_user_text(request: &CompletionRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == subspace::types::Role::User)
        .map(|m| m.text())
        .unwrap_or_default()
}

/// Whether a request is a channel routing call (not a worker turn).
pub fn is_router(request: &CompletionRequest) -> bool {
    request
        .system_prompt
        .as_deref()
        .is_some_and(|p| p.starts_with("You route messages"))
}

/// Wait until `channel` has at least `posts` posts, no routing is pending,
/// and every worker is idle.
pub async fn settle(manager: &WorkerManager, channel: &str, posts: usize) {
    let hub = manager.channels().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let all = hub.posts(channel).await.unwrap();
            let count = all.len();
            let routed = all
                .iter()
                .all(|p| p.routing != subspace_server::channel::Routing::Pending);
            let idle = manager
                .list()
                .iter()
                .all(|w| w.status == WorkerStatus::Idle);
            if count >= posts && routed && idle {
                // Let any just-queued turn start before declaring quiet.
                tokio::time::sleep(Duration::from_millis(30)).await;
                if manager
                    .list()
                    .iter()
                    .all(|w| w.status == WorkerStatus::Idle)
                    && hub.posts(channel).await.unwrap().len() == count
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed out waiting for the channel to settle");
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
