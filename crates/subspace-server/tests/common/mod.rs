//! Test helpers shared across integration tests.

#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

use subspace::agent::AgentEvent;
use subspace::provider::mock::{MockProvider, MockResponse};
use subspace::provider::{CompletionRequest, CompletionStream, Provider};
use subspace_server::{WorkerEvent, WorkerManager};

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

/// Prompt a worker and wait for its run to finish.
pub async fn prompt_and_wait(manager: &WorkerManager, id: &str, text: &str) {
    let mut events = manager.subscribe();
    manager.prompt(id, text).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(ev) = events.recv().await {
            if matches!(ev.inner, WorkerEvent::Agent(AgentEvent::AgentEnd { .. })) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for AgentEnd");
}
