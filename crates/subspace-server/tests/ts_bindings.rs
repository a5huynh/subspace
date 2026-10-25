//! Fails when the committed TypeScript bindings don't match the Rust wire
//! types. Regenerate with:
//!
//!     UPDATE_TS_BINDINGS=1 cargo test -p subspace-server --test ts_bindings

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use subspace::types::new_id;
use subspace_server::bindings::{CONSOLE_BINDINGS_DIR, export_ts_bindings};

fn committed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(CONSOLE_BINDINGS_DIR)
}

/// Relative path → contents for every `.ts` file under `dir`.
fn read_tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().is_some_and(|e| e == "ts") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn ts_bindings_are_up_to_date() {
    let fresh_dir = std::env::temp_dir().join(format!("subspace_ts_bindings_{}", new_id()));
    export_ts_bindings(&fresh_dir).unwrap();
    let fresh = read_tree(&fresh_dir);
    let _ = fs::remove_dir_all(&fresh_dir);
    assert!(!fresh.is_empty(), "no bindings were generated");

    let committed = committed_dir();
    if std::env::var_os("UPDATE_TS_BINDINGS").is_some() {
        let _ = fs::remove_dir_all(&committed);
        export_ts_bindings(&committed).unwrap();
        return;
    }

    let current = read_tree(&committed);
    if current != fresh {
        let mut diff: Vec<String> = Vec::new();
        for (path, contents) in &fresh {
            match current.get(path) {
                None => diff.push(format!("missing: {path}")),
                Some(c) if c != contents => diff.push(format!("changed: {path}")),
                _ => {}
            }
        }
        for path in current.keys().filter(|p| !fresh.contains_key(*p)) {
            diff.push(format!("stale: {path}"));
        }
        panic!(
            "TypeScript bindings in {CONSOLE_BINDINGS_DIR} are out of date:\n  {}\n\
             Regenerate with: UPDATE_TS_BINDINGS=1 cargo test -p subspace-server --test ts_bindings",
            diff.join("\n  ")
        );
    }
}
