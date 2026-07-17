use subspace::types::Message;
use subspace_server::WorkerConfig;
use subspace_server::store::SqliteStore;

#[tokio::test]
async fn worker_and_history_roundtrip() {
    let store = SqliteStore::connect("sqlite::memory:").await.unwrap();

    let cfg = WorkerConfig::new("Secretary", "role text");
    store.save_worker(&cfg).await.unwrap();

    let workers = store.list_workers().await.unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].id, cfg.id);
    assert_eq!(workers[0].name, "Secretary");
    assert_eq!(workers[0].thinking, cfg.thinking);

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
    assert!(store.list_workers().await.unwrap().is_empty());
    assert!(store.load_history(&cfg.id).await.unwrap().is_empty());
}
