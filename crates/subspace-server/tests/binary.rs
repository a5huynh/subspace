//! Smoke test: the real `subspace-server` binary boots, serves the API, and
//! uses the data dir it was given.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use subspace::types::new_id;

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn binary_boots_and_serves_health() {
    let data = std::env::temp_dir().join(format!("subspace_bin_test_{}", new_id()));
    let mut child = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_subspace-server"))
            .args(["--bind", "127.0.0.1:0", "--data-dir"])
            .arg(&data)
            .env("ANTHROPIC_API_KEY", "test-key-not-used")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let stdout = child.0.stdout.take().unwrap();
    let line = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        line
    });
    let line = tokio::time::timeout(Duration::from_secs(30), line)
        .await
        .expect("server never printed its address")
        .unwrap();
    let base = line
        .trim()
        .strip_prefix("subspace-server listening on ")
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .to_string();

    let health: serde_json::Value = reqwest::get(format!("{base}/api/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], "ok");

    assert!(data.join("subspace.db").is_file());
    assert!(data.join("workers").is_dir());
    assert!(data.join("skills").is_dir());

    drop(child);
    let _ = std::fs::remove_dir_all(&data);
}
