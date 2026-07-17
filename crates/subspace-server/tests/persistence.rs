use std::sync::Arc;
use std::time::Duration;

use subspace::agent::AgentEvent;
use subspace::provider::Provider;
use subspace::provider::mock::MockProvider;
use subspace_server::store::SqliteStore;
use subspace_server::{WorkerConfig, WorkerEvent, WorkerManager};

#[tokio::test]
async fn history_is_persisted_and_worker_restored() {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::with_text("Hi there"));
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());

    let manager = WorkerManager::with_store(provider.clone(), store.clone());
    let mut events = manager.subscribe();

    let id = manager
        .spawn(WorkerConfig::new("Secretary", "role"))
        .await
        .unwrap();
    manager.prompt(&id, "hello").await.unwrap();

    // Wait until the run completes.
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(ev) = events.recv().await {
            if matches!(ev.inner, WorkerEvent::Agent(AgentEvent::AgentEnd { .. })) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for AgentEnd");

    // History should contain at least the user prompt + assistant reply.
    let history = store.load_history(&id).await.unwrap();
    assert!(
        history.len() >= 2,
        "expected user + assistant messages, got {}",
        history.len()
    );
    assert_eq!(history[0].text(), "hello");

    // A fresh manager sharing the store should restore the worker on boot.
    let manager2 = WorkerManager::with_store(provider, store.clone());
    let restored = manager2.boot().await.unwrap();
    assert_eq!(restored, 1);
    assert_eq!(manager2.list().len(), 1);
    assert_eq!(manager2.list()[0].id, id);
}
