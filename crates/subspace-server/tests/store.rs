use std::path::Path;

use subspace::types::Message;
use subspace_server::WorkerConfig;
use subspace_server::store::SqliteStore;

#[tokio::test]
async fn index_and_history_roundtrip() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();

    let mut cfg = WorkerConfig::new("Secretary", "role text");
    cfg.description = "Triages email".into();
    store
        .upsert_index(&cfg, Path::new("/data/workers/x"))
        .await
        .unwrap();

    let index = store.list_index().await.unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].id, cfg.id);
    assert_eq!(index[0].name, "Secretary");
    assert_eq!(index[0].description, "Triages email");
    assert_eq!(index[0].home_path, "/data/workers/x");

    store
        .append_messages(
            &cfg.id,
            &[Message::user("hello"), Message::assistant(vec![])],
        )
        .await
        .unwrap();

    let history = store.load_history(&cfg.id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].text(), "hello");

    store.delete_worker(&cfg.id).await.unwrap();
    assert!(store.list_index().await.unwrap().is_empty());
    assert!(store.load_history(&cfg.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn upsert_refreshes_fields_but_keeps_created_at() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let mut cfg = WorkerConfig::new("Secretary", "role");
    store.upsert_index(&cfg, Path::new("/a")).await.unwrap();
    let first = store.list_index().await.unwrap().remove(0);

    cfg.name = "Chief of Staff".into();
    store.upsert_index(&cfg, Path::new("/b")).await.unwrap();
    let index = store.list_index().await.unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].name, "Chief of Staff");
    assert_eq!(index[0].home_path, "/b");
    assert_eq!(index[0].created_at, first.created_at);
}

#[tokio::test]
async fn remove_index_keeps_history() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();
    let cfg = WorkerConfig::new("Secretary", "role");
    store.upsert_index(&cfg, Path::new("/a")).await.unwrap();
    store
        .append_messages(&cfg.id, &[Message::user("hello")])
        .await
        .unwrap();

    store.remove_index(&cfg.id).await.unwrap();
    assert!(store.list_index().await.unwrap().is_empty());
    assert_eq!(store.load_history(&cfg.id).await.unwrap().len(), 1);
}
