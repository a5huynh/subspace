//! Upgrading an A3 database (worker config in SQLite) to A4 homes.

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use subspace::provider::Provider;
use subspace::provider::mock::MockProvider;
use subspace::types::{ThinkingLevel, new_id};
use subspace_server::home::{Homes, MANIFEST_FILE};
use subspace_server::store::SqliteStore;
use subspace_server::{WorkerConfig, WorkerManager};

struct LegacyRow<'a> {
    id: &'a str,
    name: &'a str,
    role: &'a str,
    model: &'a str,
    thinking: &'a str,
    created_at: &'a str,
}

fn data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("subspace_migrate_test_{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn db_url(data: &Path) -> String {
    format!("sqlite://{}", data.join("subspace.db").display())
}

/// Build a database exactly as A3 left it: only the 0001 schema, with rows.
async fn a3_database(data: &Path, rows: &[LegacyRow<'_>], history_for: &[&str]) {
    let opts = SqliteConnectOptions::from_str(&db_url(data))
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(opts).await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
        .execute(&pool)
        .await
        .unwrap();
    for r in rows {
        sqlx::query("INSERT INTO workers (id, name, role, model, thinking, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(r.id)
            .bind(r.name)
            .bind(r.role)
            .bind(r.model)
            .bind(r.thinking)
            .bind(r.created_at)
            .execute(&pool)
            .await
            .unwrap();
    }
    for id in history_for {
        sqlx::query("INSERT INTO messages (worker_id, data, created_at) VALUES (?, ?, ?)")
            .bind(id)
            .bind(serde_json::to_string(&subspace::types::Message::user("from A3")).unwrap())
            .bind("2026-01-01T00:00:00Z")
            .execute(&pool)
            .await
            .unwrap();
    }
    pool.close().await;
}

async fn open(data: &Path) -> (Arc<SqliteStore>, Homes, WorkerManager) {
    let store = Arc::new(SqliteStore::connect(&db_url(data)).await.unwrap());
    let homes = Homes::open(data).unwrap();
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::with_text("ok"));
    let manager = WorkerManager::with_persistence(provider, store.clone(), homes.clone());
    (store, homes, manager)
}

#[tokio::test]
async fn a3_workers_are_migrated_into_homes() {
    let data = data_dir();
    let (a, b) = (new_id(), new_id());
    a3_database(
        &data,
        &[
            LegacyRow {
                id: &a,
                name: "Secretary",
                role: "You manage the inbox.",
                model: "model-a",
                thinking: "\"high\"",
                created_at: "2026-01-01T00:00:00Z",
            },
            LegacyRow {
                id: &b,
                name: "Researcher",
                role: "You dig.",
                model: "model-b",
                thinking: "\"off\"",
                created_at: "2026-01-02T00:00:00Z",
            },
        ],
        &[&a],
    )
    .await;

    let (store, homes, manager) = open(&data).await;
    assert_eq!(manager.boot().await.unwrap(), 2);

    let sec = homes.load(&a).unwrap();
    assert_eq!(sec.name, "Secretary");
    assert_eq!(sec.role, "You manage the inbox.");
    assert_eq!(sec.model, "model-a");
    assert_eq!(sec.thinking, ThinkingLevel::High);
    assert_eq!(homes.load(&b).unwrap().thinking, ThinkingLevel::Off);

    let index = store.list_index().await.unwrap();
    assert_eq!(index.len(), 2);
    assert_eq!(index[0].id, a, "original created_at order is kept");
    assert_eq!(index[0].created_at, "2026-01-01T00:00:00Z");

    assert!(store.list_legacy_workers().await.unwrap().is_empty());
    assert_eq!(
        store.load_history(&a).await.unwrap()[0].text(),
        "from A3",
        "history is untouched"
    );

    // A later boot (new process) has nothing left to migrate.
    drop(manager);
    let (store, _, manager) = open(&data).await;
    assert_eq!(manager.boot().await.unwrap(), 2);
    assert_eq!(store.list_index().await.unwrap().len(), 2);

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn interrupted_migration_keeps_the_existing_home() {
    let data = data_dir();
    let id = new_id();
    a3_database(
        &data,
        &[LegacyRow {
            id: &id,
            name: "Old Name",
            role: "old role",
            model: "m",
            thinking: "\"low\"",
            created_at: "2026-01-01T00:00:00Z",
        }],
        &[],
    )
    .await;

    // Simulate a crash after the home was created but before the DB step,
    // followed by a hand edit to the home.
    let homes = Homes::open(&data).unwrap();
    let mut cfg = WorkerConfig::new("New Name", "new role");
    cfg.id = id.clone();
    homes.create(&cfg).unwrap();

    let (store, homes, manager) = open(&data).await;
    assert_eq!(manager.boot().await.unwrap(), 1);

    let loaded = homes.load(&id).unwrap();
    assert_eq!(loaded.name, "New Name", "home is not overwritten");
    assert_eq!(loaded.role, "new role");
    assert!(store.list_legacy_workers().await.unwrap().is_empty());
    let index = store.list_index().await.unwrap();
    assert_eq!(index[0].name, "New Name");
    assert_eq!(index[0].created_at, "2026-01-01T00:00:00Z");

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn unconvertible_rows_are_left_in_place() {
    let data = data_dir();
    let good = new_id();
    let bad_thinking = new_id();
    a3_database(
        &data,
        &[
            LegacyRow {
                id: &good,
                name: "Good",
                role: "r",
                model: "m",
                thinking: "\"low\"",
                created_at: "2026-01-01T00:00:00Z",
            },
            LegacyRow {
                id: &bad_thinking,
                name: "Bad thinking",
                role: "r",
                model: "m",
                thinking: "not json",
                created_at: "2026-01-02T00:00:00Z",
            },
            LegacyRow {
                id: "../escape",
                name: "Bad id",
                role: "r",
                model: "m",
                thinking: "\"low\"",
                created_at: "2026-01-03T00:00:00Z",
            },
        ],
        &[],
    )
    .await;

    let (store, homes, manager) = open(&data).await;
    assert_eq!(
        manager.boot().await.unwrap(),
        1,
        "only the good row migrates"
    );
    assert!(homes.exists(&good).unwrap());
    assert!(!homes.exists(&bad_thinking).unwrap());
    assert!(!data.join("escape").exists());
    assert!(!data.join("workers").join(MANIFEST_FILE).exists());

    let mut left: Vec<_> = store
        .list_legacy_workers()
        .await
        .unwrap()
        .into_iter()
        .map(|w| w.id)
        .collect();
    left.sort();
    let mut expected = vec![bad_thinking.clone(), "../escape".to_string()];
    expected.sort();
    assert_eq!(left, expected);

    let _ = fs::remove_dir_all(&data);
}
