use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use subspace::agent::AgentEvent;
use subspace::provider::mock::MockProvider;
use subspace::provider::{CompletionRequest, CompletionStream, Provider};
use subspace::types::new_id;
use subspace_server::home::{GLOBAL_SKILLS_DIR, Homes};
use subspace_server::skills::{
    self, Frontmatter, SKILL_FILE, SkillSelection, SkillSource, compose_system_prompt,
    format_skills, parse_frontmatter,
};
use subspace_server::store::SqliteStore;
use subspace_server::{WorkerConfig, WorkerEvent, WorkerManager};

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("subspace_skills_test_{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_skill(dir: &Path, folder: &str, frontmatter: &str) -> PathBuf {
    let skill_dir = dir.join(folder);
    fs::create_dir_all(&skill_dir).unwrap();
    let path = skill_dir.join(SKILL_FILE);
    fs::write(
        &path,
        format!("---\n{frontmatter}\n---\n\n# Body\nDo the thing.\n"),
    )
    .unwrap();
    path
}

// --- frontmatter -----------------------------------------------------------

#[test]
fn parses_basic_frontmatter() {
    let fm = parse_frontmatter("---\nname: web-search\ndescription: Search the web.\n---\nbody")
        .unwrap();
    assert_eq!(
        fm,
        Frontmatter {
            name: Some("web-search".into()),
            description: "Search the web.".into()
        }
    );
}

#[test]
fn parses_quoted_folded_and_extra_keys() {
    let fm = parse_frontmatter(
        "---\n\
         name: \"pdf\"\n\
         license: MIT\n\
         description: >\n  Read and write PDFs.\n  Use for any .pdf task.\n\
         ---\n",
    )
    .unwrap();
    assert_eq!(fm.name.as_deref(), Some("pdf"));
    assert_eq!(
        fm.description,
        "Read and write PDFs. Use for any .pdf task."
    );

    let fm = parse_frontmatter("---\ndescription: 'Has: a colon'\n---\n").unwrap();
    assert_eq!(fm.name, None, "name is optional");
    assert_eq!(fm.description, "Has: a colon");
}

#[test]
fn rejects_bad_frontmatter() {
    for bad in [
        "no frontmatter at all",
        "---\nname: x\ndescription: never closed\n",
        "---\nname: x\n---\n",
        "---\nname: x\ndescription:\n---\n",
        "---\njust some words\n---\n",
    ] {
        assert!(parse_frontmatter(bad).is_err(), "should reject {bad:?}");
    }
}

// --- discovery & resolution ------------------------------------------------

#[test]
fn discover_finds_skills_and_reports_broken_ones() {
    let dir = temp_dir();
    write_skill(&dir, "alpha", "name: alpha\ndescription: A");
    write_skill(&dir, "folder-name", "description: falls back to folder");
    write_skill(&dir, "broken", "name: broken");
    fs::create_dir_all(dir.join("no-skill-file")).unwrap();
    fs::create_dir_all(dir.join(".hidden")).unwrap();
    fs::write(dir.join("stray.md"), "x").unwrap();

    let listing = skills::discover(&dir, SkillSource::Global).unwrap();
    let names: Vec<_> = listing.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha", "folder-name"]);
    assert!(listing.skills.iter().all(|s| s.path.is_absolute()));
    assert_eq!(listing.invalid.len(), 1);
    assert!(listing.invalid[0].0.ends_with("broken/SKILL.md"));

    let missing = skills::discover(&dir.join("nope"), SkillSource::Global).unwrap();
    assert!(missing.skills.is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn discover_follows_symlinked_skill_dirs() {
    let root = temp_dir();
    let (real, skills_dir) = (root.join("elsewhere"), root.join("skills"));
    let target = write_skill(&real, "linked", "name: linked\ndescription: via symlink");
    fs::create_dir_all(&skills_dir).unwrap();
    std::os::unix::fs::symlink(real.join("linked"), skills_dir.join("linked")).unwrap();

    let listing = skills::discover(&skills_dir, SkillSource::Global).unwrap();
    assert_eq!(listing.skills.len(), 1);
    assert_eq!(listing.skills[0].name, "linked");
    assert_eq!(
        listing.skills[0].path,
        fs::canonicalize(target).unwrap(),
        "location points at the real file"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn worker_skills_override_global_and_disabled_are_dropped() {
    let root = temp_dir();
    let (wdir, gdir) = (root.join("worker"), root.join("global"));
    let override_path = write_skill(&wdir, "shared", "name: shared\ndescription: worker version");
    write_skill(&wdir, "mine", "name: mine\ndescription: only mine");
    write_skill(&gdir, "shared", "name: shared\ndescription: global version");
    write_skill(&gdir, "noisy", "name: noisy\ndescription: unwanted");
    write_skill(&gdir, "common", "name: common\ndescription: everyone");

    let worker = skills::discover(&wdir, SkillSource::Worker).unwrap();
    let global = skills::discover(&gdir, SkillSource::Global).unwrap();
    let selection = SkillSelection {
        disabled: vec!["noisy".into()],
    };
    let resolved = skills::resolve(worker, global, &selection);

    let names: Vec<_> = resolved.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["common", "mine", "shared"]);
    let shared = resolved.iter().find(|s| s.name == "shared").unwrap();
    assert_eq!(shared.description, "worker version");
    assert_eq!(shared.source, SkillSource::Worker);
    assert_eq!(shared.path, fs::canonicalize(override_path).unwrap());

    let _ = fs::remove_dir_all(&root);
}

// --- prompt formatting -----------------------------------------------------

#[test]
fn formats_and_escapes_skills_block() {
    assert_eq!(format_skills(&[]), None);
    assert_eq!(compose_system_prompt("Role.", &[]), "Role.");

    let dir = temp_dir();
    write_skill(
        &dir,
        "tricky",
        "name: tricky\ndescription: Use for <html> & \"stuff\"",
    );
    let listing = skills::discover(&dir, SkillSource::Global).unwrap();

    let prompt = compose_system_prompt("You are helpful.\n", &listing.skills);
    assert!(prompt.starts_with("You are helpful.\n\n<skills>\n"));
    assert!(prompt.ends_with("</skills>"));
    assert!(prompt.contains("<name>tricky</name>"));
    assert!(prompt.contains("<description>Use for &lt;html&gt; &amp; \"stuff\"</description>"));
    assert!(prompt.contains(&format!(
        "<location>{}</location>",
        listing.skills[0].path.display()
    )));

    assert!(compose_system_prompt("  ", &listing.skills).starts_with("<skills>"));

    let _ = fs::remove_dir_all(&dir);
}

// --- manager integration ---------------------------------------------------

/// Records the system prompt of every completion request, then delegates.
struct CapturingProvider {
    inner: MockProvider,
    prompts: Mutex<Vec<Option<String>>>,
}

// `Provider` is an `#[async_trait]`; implementing its desugared form avoids a
// test-only dependency on `async-trait`.
impl Provider for CapturingProvider {
    fn name(&self) -> &str {
        "capturing"
    }

    fn complete<'a, 'b>(
        &'a self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<CompletionStream>> + Send + 'b>>
    where
        'a: 'b,
        Self: 'b,
    {
        self.prompts
            .lock()
            .unwrap()
            .push(request.system_prompt.clone());
        self.inner.complete(request)
    }
}

async fn system_prompt_seen_by_model(manager: &WorkerManager, id: &str) {
    let mut events = manager.subscribe();
    manager.prompt(id, "hi").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(ev) = events.recv().await {
            if matches!(ev.inner, WorkerEvent::Agent(AgentEvent::AgentEnd { .. })) {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for AgentEnd");
}

#[tokio::test]
async fn workers_see_resolved_skills_in_their_system_prompt() {
    let data = temp_dir();
    let homes = Homes::open(&data).unwrap();
    let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());

    let global = data.join(GLOBAL_SKILLS_DIR);
    write_skill(
        &global,
        "shared",
        "name: shared\ndescription: global version",
    );
    write_skill(&global, "noisy", "name: noisy\ndescription: unwanted");

    let mut cfg = WorkerConfig::new("Secretary", "You manage the inbox.");
    cfg.skills.disabled = vec!["noisy".into()];
    homes.create(&cfg).unwrap();
    write_skill(
        &homes.skills_dir(&cfg.id).unwrap(),
        "shared",
        "name: shared\ndescription: worker version",
    );
    assert_eq!(
        homes.load(&cfg.id).unwrap().skills.disabled,
        ["noisy"],
        "[skills] round-trips through worker.toml"
    );

    let provider = Arc::new(CapturingProvider {
        inner: MockProvider::with_text("ok"),
        prompts: Mutex::new(Vec::new()),
    });
    let manager = WorkerManager::with_persistence(provider.clone(), store, homes);
    assert_eq!(manager.boot().await.unwrap(), 1);
    system_prompt_seen_by_model(&manager, &cfg.id).await;

    let prompts = provider.prompts.lock().unwrap();
    let prompt = prompts[0].as_deref().expect("a system prompt was sent");
    assert!(prompt.starts_with("You manage the inbox.\n\n<skills>"));
    assert!(prompt.contains("<description>worker version</description>"));
    assert!(!prompt.contains("global version"), "worker skill overrides");
    assert!(!prompt.contains("noisy"), "disabled skill is omitted");

    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn in_memory_workers_have_no_skills() {
    let provider = Arc::new(CapturingProvider {
        inner: MockProvider::with_text("ok"),
        prompts: Mutex::new(Vec::new()),
    });
    let manager = WorkerManager::new(provider.clone());
    let id = manager
        .spawn(WorkerConfig::new("Scratch", "Just a role."))
        .await
        .unwrap();
    system_prompt_seen_by_model(&manager, &id).await;
    assert_eq!(
        provider.prompts.lock().unwrap()[0].as_deref(),
        Some("Just a role.")
    );
}
