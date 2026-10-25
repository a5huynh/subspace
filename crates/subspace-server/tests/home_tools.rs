mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{CapturingProvider, prompt_and_wait};
use serde_json::json;
use subspace::provider::mock::MockResponse;
use subspace::types::new_id;
use subspace_server::home::{Homes, MANIFEST_FILE, ROLE_FILE};
use subspace_server::home_tools::{Access, HomeScope, home_tools};
use subspace_server::store::SqliteStore;
use subspace_server::{WorkerConfig, WorkerManager};

struct Fixture {
    data: PathBuf,
    homes: Homes,
    cfg: WorkerConfig,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.data);
    }
}

fn fixture() -> Fixture {
    let data = std::env::temp_dir().join(format!("subspace_home_tools_test_{}", new_id()));
    fs::create_dir_all(&data).unwrap();
    let homes = Homes::open(&data).unwrap();
    let cfg = WorkerConfig::new("Secretary", "role");
    let home = homes.create(&cfg).unwrap();
    Fixture {
        data,
        homes,
        cfg,
        home,
    }
}

fn write_skill(dir: &Path, name: &str) -> PathBuf {
    let skill = dir.join(name);
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: d\n---\nbody of {name}\n"),
    )
    .unwrap();
    skill
}

fn scope(f: &Fixture, skill_dirs: Vec<PathBuf>) -> HomeScope {
    HomeScope::new(&f.home, f.homes.global_skills_dir(), skill_dirs).unwrap()
}

fn noop(_: String) {}

// --- path policy -----------------------------------------------------------

#[test]
fn reads_allowed_in_home_and_global_skills() {
    let f = fixture();
    let global = write_skill(f.homes.global_skills_dir(), "shared");
    let s = scope(&f, vec![]);

    for p in [MANIFEST_FILE, ROLE_FILE, "memory/missing.md", "skills"] {
        assert!(s.resolve(p, Access::Read).is_ok(), "{p} should be readable");
    }
    let abs = global.join("SKILL.md");
    assert!(s.resolve(abs.to_str().unwrap(), Access::Read).is_ok());
}

#[test]
fn writes_limited_to_skills_and_memory() {
    let f = fixture();
    let global = write_skill(f.homes.global_skills_dir(), "shared");
    let s = scope(&f, vec![]);

    for p in [
        "memory/note.md",
        "skills/new-skill/SKILL.md",
        "memory/deep/er/x.md",
    ] {
        assert!(
            s.resolve(p, Access::Write).is_ok(),
            "{p} should be writable"
        );
    }
    for p in [
        MANIFEST_FILE,
        ROLE_FILE,
        "memory",
        "skills",
        "other/file.md",
        "notes.md",
    ] {
        let err = s.resolve(p, Access::Write).unwrap_err();
        assert!(err.contains("not writable"), "{p}: {err}");
    }
    let global_file = global.join("SKILL.md");
    assert!(
        s.resolve(global_file.to_str().unwrap(), Access::Write)
            .is_err(),
        "global skills are read-only"
    );
}

#[test]
fn parent_dir_components_are_rejected() {
    let f = fixture();
    let s = scope(&f, vec![]);
    for p in [
        "skills/../worker.toml",
        "../other/ROLE.md",
        "memory/../../escape.md",
    ] {
        for access in [Access::Read, Access::Write] {
            let err = s.resolve(p, access).unwrap_err();
            assert!(err.contains("'..'"), "{p}: {err}");
        }
    }
}

#[test]
fn paths_outside_the_scope_are_denied() {
    let f = fixture();
    let other = WorkerConfig::new("Other", "role");
    let other_home = f.homes.create(&other).unwrap();
    let s = scope(&f, vec![]);

    let other_role = other_home.join(ROLE_FILE);
    for p in ["/etc/passwd", other_role.to_str().unwrap()] {
        assert!(
            s.resolve(p, Access::Read).is_err(),
            "{p} must not be readable"
        );
        assert!(
            s.resolve(p, Access::Write).is_err(),
            "{p} must not be writable"
        );
    }
    let db = f.data.join("subspace.db");
    assert!(s.resolve(db.to_str().unwrap(), Access::Read).is_err());
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_escape_the_scope() {
    use std::os::unix::fs::symlink;

    let f = fixture();
    let outside = f.data.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "secret").unwrap();

    // A directory symlink inside memory/ pointing outside the home.
    symlink(&outside, f.home.join("memory/link")).unwrap();
    // A file symlink inside memory/ pointing back at the read-only manifest.
    symlink(
        f.home.join(MANIFEST_FILE),
        f.home.join("memory/manifest-alias"),
    )
    .unwrap();

    let s = scope(&f, vec![]);
    assert!(s.resolve("memory/link/secret.txt", Access::Read).is_err());
    assert!(s.resolve("memory/link/new.txt", Access::Write).is_err());
    assert!(
        s.resolve("memory/manifest-alias", Access::Write).is_err(),
        "a symlink must not make worker.toml writable"
    );
    assert!(s.resolve("memory/manifest-alias", Access::Read).is_ok());
}

#[cfg(unix)]
#[test]
fn offered_symlinked_skills_are_readable_but_their_neighbours_are_not() {
    let f = fixture();
    let elsewhere = f.data.join("dotfiles");
    let real_skill = write_skill(&elsewhere, "linked");
    fs::write(elsewhere.join("private.txt"), "nope").unwrap();
    std::os::unix::fs::symlink(&real_skill, f.homes.global_skills_dir().join("linked")).unwrap();

    let canonical_skill = fs::canonicalize(&real_skill).unwrap();
    let s = scope(&f, vec![canonical_skill.clone()]);
    let skill_md = canonical_skill.join("SKILL.md");
    assert!(s.resolve(skill_md.to_str().unwrap(), Access::Read).is_ok());
    let neighbour = elsewhere.join("private.txt");
    assert!(
        s.resolve(neighbour.to_str().unwrap(), Access::Read)
            .is_err()
    );
    assert!(
        s.resolve(skill_md.to_str().unwrap(), Access::Write)
            .is_err()
    );
}

// --- tools -----------------------------------------------------------------

#[tokio::test]
async fn tools_write_read_and_edit_within_the_home() {
    let f = fixture();
    let tools = home_tools(Arc::new(scope(&f, vec![])));
    let [read, write, edit] = [0, 1, 2].map(|i| tools[i].clone());
    assert_eq!(read.definition().name, "home_read");
    assert_eq!(write.definition().name, "home_write");
    assert_eq!(edit.definition().name, "home_edit");

    let r = write
        .execute(
            json!({"path": "memory/people.md", "content": "Alice: CFO\n"}),
            &noop,
        )
        .await
        .unwrap();
    assert!(!r.is_error);
    assert_eq!(
        fs::read_to_string(f.home.join("memory/people.md")).unwrap(),
        "Alice: CFO\n"
    );

    let r = edit
        .execute(
            json!({"path": "memory/people.md", "oldText": "CFO", "newText": "CEO"}),
            &noop,
        )
        .await
        .unwrap();
    assert!(!r.is_error);

    let r = read
        .execute(json!({"path": "memory/people.md"}), &noop)
        .await
        .unwrap();
    assert!(!r.is_error);
    let text = format!("{:?}", r.content);
    assert!(text.contains("Alice: CEO"), "{text}");
}

#[tokio::test]
async fn denied_operations_are_tool_errors_and_change_nothing() {
    let f = fixture();
    let tools = home_tools(Arc::new(scope(&f, vec![])));
    let (write, edit) = (tools[1].clone(), tools[2].clone());
    let before = fs::read_to_string(f.home.join(MANIFEST_FILE)).unwrap();

    let r = write
        .execute(json!({"path": MANIFEST_FILE, "content": "pwned"}), &noop)
        .await
        .unwrap();
    assert!(r.is_error);
    let r = edit
        .execute(
            json!({"path": ROLE_FILE, "oldText": "role", "newText": "evil"}),
            &noop,
        )
        .await
        .unwrap();
    assert!(r.is_error);
    let r = write
        .execute(json!({"content": "no path"}), &noop)
        .await
        .unwrap();
    assert!(r.is_error);

    assert_eq!(
        fs::read_to_string(f.home.join(MANIFEST_FILE)).unwrap(),
        before
    );
    assert_eq!(fs::read_to_string(f.home.join(ROLE_FILE)).unwrap(), "role");
}

// --- manager integration ---------------------------------------------------

#[tokio::test]
async fn persistent_workers_get_home_tools_in_memory_workers_do_not() {
    let f = fixture();
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
    let provider = Arc::new(CapturingProvider::with_text("ok"));
    let manager = WorkerManager::with_persistence(provider.clone(), store, f.homes.clone());
    manager.boot().await.unwrap();
    prompt_and_wait(&manager, &f.cfg.id, "hi").await;

    let mut names: Vec<_> = provider.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    names.sort();
    assert_eq!(names, ["home_edit", "home_read", "home_write"]);

    let provider = Arc::new(CapturingProvider::with_text("ok"));
    let manager = WorkerManager::new(provider.clone());
    let id = manager
        .spawn(WorkerConfig::new("Scratch", "role"))
        .await
        .unwrap();
    prompt_and_wait(&manager, &id, "hi").await;
    assert!(provider.requests()[0].tools.is_empty());
}

#[tokio::test]
async fn a_worker_can_write_memory_but_not_its_own_config() {
    let f = fixture();
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
    let provider = Arc::new(CapturingProvider::new(vec![
        MockResponse::tool_call(
            "t1",
            "home_write",
            json!({"path": "memory/inbox.md", "content": "3 unread"}),
        ),
        MockResponse::tool_call(
            "t2",
            "home_write",
            json!({"path": MANIFEST_FILE, "content": "id = \"hijacked\""}),
        ),
        MockResponse::text("done"),
    ]));
    let manager = WorkerManager::with_persistence(provider.clone(), store, f.homes.clone());
    manager.boot().await.unwrap();
    prompt_and_wait(&manager, &f.cfg.id, "remember the inbox").await;

    assert_eq!(
        fs::read_to_string(f.home.join("memory/inbox.md")).unwrap(),
        "3 unread"
    );
    assert_eq!(
        f.homes.load(&f.cfg.id).unwrap().id,
        f.cfg.id,
        "worker.toml untouched"
    );
    // The denial was fed back to the model as a tool error.
    let last = provider.requests().pop().unwrap();
    let transcript = format!("{:?}", last.messages);
    assert!(transcript.contains("not writable"), "{transcript}");
}
