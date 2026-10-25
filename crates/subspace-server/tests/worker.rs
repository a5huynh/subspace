use std::sync::Arc;
use std::time::Duration;

use subspace::agent::AgentEvent;
use subspace::provider::Provider;
use subspace::provider::mock::MockProvider;
use subspace_server::{Stimulus, WorkerConfig, WorkerEvent, WorkerManager, WorkerStatus};

#[tokio::test]
async fn worker_handles_user_prompt() {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::with_text("Hi there"));
    let manager = WorkerManager::new(provider);
    let mut events = manager.subscribe();

    let id = manager
        .spawn(WorkerConfig::new("Secretary", "You are a helpful secretary."))
        .await
        .unwrap();
    manager.prompt(&id, "hello").await.unwrap();

    let mut saw_running = false;
    let mut saw_text = false;
    let mut saw_end = false;

    let collected = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(ev) = events.recv().await {
            match ev.inner {
                WorkerEvent::StatusChanged(WorkerStatus::Running) => saw_running = true,
                WorkerEvent::Agent(AgentEvent::TextDelta(t)) if t.contains("Hi there") => {
                    saw_text = true
                }
                WorkerEvent::Agent(AgentEvent::AgentEnd { .. }) => {
                    saw_end = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(collected.is_ok(), "timed out waiting for worker events");
    assert!(saw_running, "expected StatusChanged(Running)");
    assert!(saw_text, "expected TextDelta containing 'Hi there'");
    assert!(saw_end, "expected AgentEnd");
}

#[tokio::test]
async fn manager_tracks_and_shuts_down_workers() {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::with_text("ok"));
    let manager = WorkerManager::new(provider);

    let id = manager
        .spawn(WorkerConfig::new("Worker A", "role"))
        .await
        .unwrap();
    assert_eq!(manager.list().len(), 1);
    assert_eq!(manager.status(&id), Some(WorkerStatus::Idle));

    manager.shutdown(&id).await.unwrap();
    assert!(manager.list().is_empty());
    assert_eq!(manager.status(&id), None);

    // Delivering to a removed worker is an error, not a panic.
    assert!(manager.deliver(&id, Stimulus::Shutdown).await.is_err());
}

#[tokio::test]
async fn spawning_a_duplicate_id_is_rejected() {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::with_text("ok"));
    let manager = WorkerManager::new(provider);

    let cfg = WorkerConfig::new("Worker A", "role");
    manager.spawn(cfg.clone()).await.unwrap();
    assert!(manager.spawn(cfg.clone()).await.is_err());
    assert_eq!(manager.list().len(), 1);

    // The original worker is still alive and reachable.
    manager.shutdown(&cfg.id).await.unwrap();
}
