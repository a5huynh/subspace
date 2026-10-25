use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use subspace::agent::AgentEvent;
use subspace::provider::Provider;
use subspace::provider::mock::MockProvider;
use subspace::types::{Message, new_id};
use subspace_server::home::{Homes, MANIFEST_FILE, ROLE_FILE};
use subspace_server::store::SqliteStore;
use subspace_server::{WorkerConfig, WorkerEvent, WorkerManager};

/// A fresh data dir, in-memory store, and homes rooted in that dir.
async fn setup() -> (PathBuf, Arc<SqliteStore>, Homes) {
    let data = std::env::temp_dir().join(format!("subspace_persist_test_{}", new_id()));
    fs::create_dir_all(&data).unwrap();
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
    let homes = Homes::open(&data).unwrap();
    (data, store, homes)
}

fn provider() -> Arc<dyn Provider> {
    Arc::new(MockProvider::with_text("Hi there"))
}

#[tokio::test]
async fn history_is_persisted_and_worker_restored() {
    let (data, store, homes) = setup().await;

    let manager = WorkerManager::with_persistence(provider(), store.clone(), homes.clone());
    let mut events = manager.subscribe();

    let id = manager
        .spawn(WorkerConfig::new("Secretary", "role"))
        .await
        .unwrap();
    assert!(homes.exists(&id).unwrap(), "spawn creates the home");
    assert_eq!(store.list_index().await.unwrap().len(), 1);

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

    // A fresh manager sharing the store + homes should restore the worker on boot.
    let manager2 = WorkerManager::with_persistence(provider(), store.clone(), homes);
    let restored = manager2.boot().await.unwrap();
    assert_eq!(restored, 1);
    assert_eq!(manager2.list().len(), 1);
    assert_eq!(manager2.list()[0].id, id);

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn boot_reads_identity_from_homes_and_refreshes_index() {
    let (data, store, homes) = setup().await;
    let cfg = WorkerConfig::new("Secretary", "role");

    // A home created out-of-band (e.g. copied in by hand) with no index entry.
    let dir = homes.create(&cfg).unwrap();
    let manifest = fs::read_to_string(dir.join(MANIFEST_FILE))
        .unwrap()
        .replace("Secretary", "Hand Edited");
    fs::write(dir.join(MANIFEST_FILE), manifest).unwrap();
    fs::write(dir.join(ROLE_FILE), "new persona").unwrap();

    let manager = WorkerManager::with_persistence(provider(), store.clone(), homes);
    assert_eq!(manager.boot().await.unwrap(), 1);
    assert_eq!(manager.list()[0].name, "Hand Edited");

    let index = store.list_index().await.unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].name, "Hand Edited");
    assert_eq!(index[0].home_path, dir.to_string_lossy());

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn boot_is_idempotent() {
    let (data, store, homes) = setup().await;
    let manager = WorkerManager::with_persistence(provider(), store, homes);
    manager
        .spawn(WorkerConfig::new("Secretary", "role"))
        .await
        .unwrap();

    assert_eq!(manager.boot().await.unwrap(), 0, "already running");
    assert_eq!(manager.list().len(), 1);

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn boot_prunes_index_entries_without_a_home_but_keeps_history() {
    let (data, store, homes) = setup().await;
    let cfg = WorkerConfig::new("Ghost", "role");
    store
        .upsert_index(&cfg, &homes.path(&cfg.id).unwrap())
        .await
        .unwrap();
    store
        .append_messages(&cfg.id, &[Message::user("still here")])
        .await
        .unwrap();

    let manager = WorkerManager::with_persistence(provider(), store.clone(), homes);
    assert_eq!(manager.boot().await.unwrap(), 0);
    assert!(store.list_index().await.unwrap().is_empty());
    assert_eq!(store.load_history(&cfg.id).await.unwrap().len(), 1);

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn boot_skips_broken_homes() {
    let (data, store, homes) = setup().await;
    let good = WorkerConfig::new("Good", "role");
    let bad = WorkerConfig::new("Bad", "role");
    homes.create(&good).unwrap();
    let bad_dir = homes.create(&bad).unwrap();
    store.upsert_index(&bad, &bad_dir).await.unwrap();
    fs::write(bad_dir.join(MANIFEST_FILE), "not = [valid").unwrap();

    let manager = WorkerManager::with_persistence(provider(), store.clone(), homes);
    assert_eq!(manager.boot().await.unwrap(), 1);
    assert_eq!(manager.list()[0].id, good.id);
    // A broken home still exists on disk, so its index entry is not pruned.
    let ids: Vec<_> = store
        .list_index()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert!(ids.contains(&bad.id));

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn spawn_refuses_an_existing_home() {
    let (data, store, homes) = setup().await;
    let cfg = WorkerConfig::new("Secretary", "role");
    homes.create(&cfg).unwrap();

    let manager = WorkerManager::with_persistence(provider(), store.clone(), homes.clone());
    assert!(manager.spawn(cfg.clone()).await.is_err());
    assert!(manager.list().is_empty());
    assert!(store.list_index().await.unwrap().is_empty());
    assert!(homes.exists(&cfg.id).unwrap(), "existing home untouched");

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn remove_deletes_index_history_and_home() {
    let (data, store, homes) = setup().await;
    let manager = WorkerManager::with_persistence(provider(), store.clone(), homes.clone());
    let id = manager
        .spawn(WorkerConfig::new("Secretary", "role"))
        .await
        .unwrap();
    store
        .append_messages(&id, &[Message::user("hello")])
        .await
        .unwrap();

    manager.remove(&id).await.unwrap();
    assert!(manager.list().is_empty());
    assert!(store.list_index().await.unwrap().is_empty());
    assert!(store.load_history(&id).await.unwrap().is_empty());
    assert!(!homes.exists(&id).unwrap());

    assert!(manager.remove(&id).await.is_err(), "already gone");

    let _ = fs::remove_dir_all(&data);
}
