use std::fs;
use std::path::PathBuf;

use subspace::types::{ThinkingLevel, new_id};
use subspace_server::WorkerConfig;
use subspace_server::home::{Homes, MANIFEST_FILE, MEMORY_DIR, ROLE_FILE, SKILLS_DIR};

/// A fresh, empty data dir under the system temp dir.
fn data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("subspace_home_test_{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn sample() -> WorkerConfig {
    let mut cfg = WorkerConfig::new("Secretary", "You manage the inbox.\n\nBe brief.");
    cfg.description = "Triages email".into();
    cfg.thinking = ThinkingLevel::High;
    cfg
}

#[test]
fn create_and_load_roundtrip() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();
    let cfg = sample();

    let dir = homes.create(&cfg).unwrap();
    assert_eq!(dir, data.join("workers").join(&cfg.id));
    assert!(dir.join(MANIFEST_FILE).is_file());
    assert!(dir.join(SKILLS_DIR).is_dir());
    assert!(dir.join(MEMORY_DIR).is_dir());
    assert_eq!(
        fs::read_to_string(dir.join(ROLE_FILE)).unwrap(),
        cfg.role,
        "role is stored verbatim in ROLE.md"
    );

    let loaded = homes.load(&cfg.id).unwrap();
    assert_eq!(loaded.id, cfg.id);
    assert_eq!(loaded.name, cfg.name);
    assert_eq!(loaded.description, cfg.description);
    assert_eq!(loaded.role, cfg.role);
    assert_eq!(loaded.model, cfg.model);
    assert_eq!(loaded.thinking, cfg.thinking);

    let _ = fs::remove_dir_all(&data);
}

#[test]
fn manifest_is_hand_editable_toml() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();
    let cfg = sample();
    let dir = homes.create(&cfg).unwrap();

    // A human edits the file directly; optional fields may be omitted.
    fs::write(
        dir.join(MANIFEST_FILE),
        format!("id = \"{}\"\nname = \"Renamed\"\nmodel = \"m\"\n", cfg.id),
    )
    .unwrap();

    let loaded = homes.load(&cfg.id).unwrap();
    assert_eq!(loaded.name, "Renamed");
    assert_eq!(loaded.description, "");
    assert_eq!(loaded.thinking, ThinkingLevel::default());

    let _ = fs::remove_dir_all(&data);
}

#[test]
fn create_refuses_existing_home() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();
    let cfg = sample();
    homes.create(&cfg).unwrap();
    assert!(homes.create(&cfg).is_err());
    let _ = fs::remove_dir_all(&data);
}

#[test]
fn save_updates_existing_home_only() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();
    let mut cfg = sample();

    assert!(homes.save(&cfg).is_err(), "save must not create a home");

    homes.create(&cfg).unwrap();
    cfg.name = "Chief of Staff".into();
    cfg.role = "New persona".into();
    homes.save(&cfg).unwrap();

    let loaded = homes.load(&cfg.id).unwrap();
    assert_eq!(loaded.name, "Chief of Staff");
    assert_eq!(loaded.role, "New persona");

    let _ = fs::remove_dir_all(&data);
}

#[test]
fn load_rejects_id_mismatch() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();
    let cfg = sample();
    let dir = homes.create(&cfg).unwrap();

    fs::write(
        dir.join(MANIFEST_FILE),
        "id = \"someone-else\"\nname = \"x\"\nmodel = \"m\"\n",
    )
    .unwrap();
    assert!(homes.load(&cfg.id).is_err());

    let _ = fs::remove_dir_all(&data);
}

#[test]
fn list_skips_non_homes_and_reports_broken_ones() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();

    let a = sample();
    let b = WorkerConfig::new("Researcher", "Digs into things.");
    homes.create(&a).unwrap();
    homes.create(&b).unwrap();

    let workers = data.join("workers");
    // Not homes: no manifest, hidden staging dir, stray file.
    fs::create_dir_all(workers.join("leftover-logs")).unwrap();
    fs::create_dir_all(workers.join(".staging-abc")).unwrap();
    fs::write(workers.join("notes.txt"), "hi").unwrap();
    // Broken home: manifest that doesn't parse.
    fs::create_dir_all(workers.join("broken")).unwrap();
    fs::write(workers.join("broken").join(MANIFEST_FILE), "not = [valid").unwrap();

    let listing = homes.list().unwrap();
    let mut ids: Vec<_> = listing.workers.iter().map(|w| w.id.clone()).collect();
    let mut expected = vec![a.id.clone(), b.id.clone()];
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected);
    assert_eq!(listing.invalid.len(), 1);
    assert_eq!(listing.invalid[0].0, "broken");

    let _ = fs::remove_dir_all(&data);
}

#[test]
fn remove_deletes_home_and_is_idempotent() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();
    let cfg = sample();
    let dir = homes.create(&cfg).unwrap();

    homes.remove(&cfg.id).unwrap();
    assert!(!dir.exists());
    assert!(!homes.exists(&cfg.id).unwrap());
    homes.remove(&cfg.id).unwrap();

    let _ = fs::remove_dir_all(&data);
}

#[test]
fn ids_cannot_escape_the_workers_dir() {
    let data = data_dir();
    let homes = Homes::open(&data).unwrap();
    for bad in ["", "..", "../etc", "a/b", "a\\b", "with space"] {
        assert!(homes.path(bad).is_err(), "{bad:?} should be rejected");
    }
    let mut cfg = sample();
    cfg.id = "../escape".into();
    assert!(homes.create(&cfg).is_err());
    assert!(!data.join("escape").exists());

    let _ = fs::remove_dir_all(&data);
}
