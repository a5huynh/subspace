mod common;

use std::fs;
use std::sync::Arc;

use common::{CapturingProvider, prompt_and_wait};
use serde_json::json;
use subspace::agent::AgentEvent;
use subspace::types::{Message, new_id};
use subspace_server::event::{EVENT_BUFFER_CAPACITY, EventLog};
use subspace_server::store::SqliteStore;
use subspace_server::{
    Homes, ManagerError, Replay, ServerEvent, WorkerConfig, WorkerEvent, WorkerManager,
    WorkerStatus,
};
use tokio::sync::broadcast;

fn log() -> (EventLog, broadcast::Sender<ServerEvent>) {
    let (bus, _) = broadcast::channel(64);
    (EventLog::new("w1".into(), bus.clone()), bus)
}

fn seqs(replay: Replay) -> Vec<u64> {
    match replay {
        Replay::Events(evs) => evs.into_iter().map(|e| e.seq).collect(),
        Replay::Gap => panic!("unexpected gap"),
    }
}

// --- wire format -----------------------------------------------------------

#[test]
fn events_serialize_to_the_generated_typescript_shapes() {
    let (log, bus) = log();
    let mut rx = bus.subscribe();
    log.emit(WorkerEvent::Agent(AgentEvent::TextDelta("hi".into())));
    log.emit(WorkerEvent::StatusChanged(WorkerStatus::Error(
        "boom".into(),
    )));
    log.emit(WorkerEvent::StatusChanged(WorkerStatus::Idle));

    let ev = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
    assert_eq!(ev["worker_id"], "w1");
    assert_eq!(ev["seq"], 1);
    assert!(ev["ts"].is_string());
    assert_eq!(
        ev["inner"],
        json!({"type": "agent", "data": {"type": "text_delta", "data": "hi"}})
    );

    let ev = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
    assert_eq!(
        ev["inner"],
        json!({"type": "status_changed", "data": {"state": "error", "message": "boom"}})
    );
    let ev = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
    assert_eq!(
        ev["inner"],
        json!({"type": "status_changed", "data": {"state": "idle"}})
    );
}

// --- event log -------------------------------------------------------------

#[test]
fn seq_starts_at_one_and_replay_returns_events_after_since() {
    let (log, _bus) = log();
    assert_eq!(log.cursor(), 0);
    for i in 0..5 {
        log.emit(WorkerEvent::StimulusReceived(format!("{i}")));
    }
    assert_eq!(log.cursor(), 5);
    assert_eq!(seqs(log.subscribe(0).0), [1, 2, 3, 4, 5]);
    assert_eq!(seqs(log.subscribe(3).0), [4, 5]);
    assert!(seqs(log.subscribe(5).0).is_empty());
}

#[test]
fn subscribe_then_emit_delivers_live_without_duplicates() {
    let (log, _bus) = log();
    log.emit(WorkerEvent::StimulusReceived("before".into()));
    let (replay, mut rx) = log.subscribe(0);
    assert_eq!(seqs(replay), [1]);
    log.emit(WorkerEvent::StimulusReceived("after".into()));
    assert_eq!(rx.try_recv().unwrap().seq, 2);
    assert!(rx.try_recv().is_err());
}

#[test]
fn evicted_or_future_cursors_are_gaps() {
    let (log, _bus) = log();
    let total = EVENT_BUFFER_CAPACITY as u64 + 10;
    for _ in 0..total {
        log.emit(WorkerEvent::StimulusReceived(String::new()));
    }
    // Oldest buffered seq is 11, so resuming from 10 is fine but 9 is not.
    assert_eq!(seqs(log.subscribe(10).0).len(), EVENT_BUFFER_CAPACITY);
    assert!(matches!(log.subscribe(9).0, Replay::Gap));
    assert!(matches!(log.subscribe(total + 1).0, Replay::Gap));
}

#[test]
fn cursor_points_before_an_in_progress_run() {
    let (log, _bus) = log();
    log.emit(WorkerEvent::StimulusReceived("old".into()));
    log.emit(WorkerEvent::StatusChanged(WorkerStatus::Running));
    log.emit(WorkerEvent::Agent(AgentEvent::TextDelta("partial".into())));
    assert_eq!(log.cursor(), 1, "mid-run: replay the whole current run");
    log.emit(WorkerEvent::StatusChanged(WorkerStatus::Idle));
    assert_eq!(log.cursor(), 4);
}

// --- manager ---------------------------------------------------------------

async fn persistent_manager(
    provider: Arc<CapturingProvider>,
) -> (WorkerManager, std::path::PathBuf) {
    let data = std::env::temp_dir().join(format!("subspace_events_test_{}", new_id()));
    fs::create_dir_all(&data).unwrap();
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
    let manager = WorkerManager::with_persistence(provider, store, Homes::open(&data).unwrap());
    (manager, data)
}

#[tokio::test]
async fn history_cursor_and_user_message_ids_line_up() {
    let provider = Arc::new(CapturingProvider::with_text("hello back"));
    let (manager, data) = persistent_manager(provider).await;
    let id = manager
        .spawn(WorkerConfig::new("Secretary", "role"))
        .await
        .unwrap();

    let before = manager.history(&id).await.unwrap();
    assert_eq!(before.epoch, manager.epoch());
    assert_eq!(before.cursor, 0);
    assert!(before.messages.is_empty());

    prompt_and_wait(&manager, &id, "hello").await;

    let after = manager.history(&id).await.unwrap();
    let Replay::Events(events) = manager.subscribe_worker(&id, before.cursor).unwrap().0 else {
        panic!("gap");
    };
    assert_eq!(after.cursor, events.last().unwrap().seq);

    let user_event: Vec<&Message> = events
        .iter()
        .filter_map(|e| match &e.inner {
            WorkerEvent::UserMessage(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(user_event.len(), 1);
    assert_eq!(
        user_event[0].id, after.messages[0].id,
        "event and persisted user message share an id"
    );

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn update_restarts_with_new_config_and_keeps_seq_increasing() {
    let provider = Arc::new(CapturingProvider::new(vec![
        subspace::provider::mock::MockResponse::text("one"),
        subspace::provider::mock::MockResponse::text("two"),
    ]));
    let (manager, data) = persistent_manager(provider.clone()).await;
    let mut cfg = WorkerConfig::new("Secretary", "old role");
    let id = manager.spawn(cfg.clone()).await.unwrap();

    prompt_and_wait(&manager, &id, "first").await;
    let cursor_before = manager.history(&id).await.unwrap().cursor;

    cfg.name = "Chief of Staff".into();
    cfg.role = "new role".into();
    manager.update(cfg.clone()).await.unwrap();
    assert_eq!(manager.get(&id).unwrap().name, "Chief of Staff");

    prompt_and_wait(&manager, &id, "second").await;
    let requests = provider.requests();
    assert_eq!(
        requests[1]
            .system_prompt
            .as_deref()
            .map(|p| p.starts_with("new role")),
        Some(true)
    );
    assert!(
        requests[1].messages.len() > 1,
        "history survives the restart"
    );

    let Replay::Events(events) = manager.subscribe_worker(&id, cursor_before).unwrap().0 else {
        panic!("gap");
    };
    assert!(
        events.first().unwrap().seq > cursor_before,
        "seq did not reset"
    );

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn update_errors_are_typed() {
    let provider = Arc::new(CapturingProvider::with_text("ok"));
    let manager = WorkerManager::new(provider);
    let err = manager
        .update(WorkerConfig::new("Ghost", "role"))
        .await
        .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<ManagerError>(),
        Some(ManagerError::NotFound(_))
    ));

    let cfg = WorkerConfig::new("A", "role");
    manager.spawn(cfg.clone()).await.unwrap();
    let err = manager.spawn(cfg).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<ManagerError>(),
        Some(ManagerError::AlreadyExists(_))
    ));
}
