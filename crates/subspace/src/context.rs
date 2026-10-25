//! Context file discovery and loading.
//!
//! Walks up from `cwd` to the filesystem root, collecting `AGENTS.md` (or `CLAUDE.md`)
//! files. Also checks `~/.pi/agent/AGENTS.md` for global context. All found files
//! are concatenated and appended to the system prompt.

use std::path::{Path, PathBuf};

/// Names to look for, in priority order (first match per directory wins).
const CONTEXT_FILE_NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// A discovered context file.
#[derive(Debug, Clone)]
pub struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

/// Discover and load context files.
///
/// Search order (all are concatenated):
/// 1. Global: `~/.pi/agent/AGENTS.md`
/// 2. Ancestors: walk from filesystem root down to `cwd` (so higher dirs come first)
/// 3. Current directory is included in the ancestor walk
pub fn load_context_files(cwd: &Path) -> Vec<ContextFile> {
    let mut files = Vec::new();

    // 1. Global context
    if let Some(home) = dirs::home_dir() {
        let global_dir = home.join(".pi").join("agent");
        if let Some(cf) = find_context_file(&global_dir) {
            files.push(cf);
        }
    }

    // 2. Walk from cwd up to root, collect paths, then reverse so root-most comes first
    let mut ancestor_files = Vec::new();
    let mut dir = cwd.to_path_buf();
    loop {
        if let Some(cf) = find_context_file(&dir) {
            ancestor_files.push(cf);
        }

        if !dir.pop() {
            break;
        }
    }
    ancestor_files.reverse(); // root-most first
    files.extend(ancestor_files);

    files
}

/// Look for a context file in a single directory.
fn find_context_file(dir: &Path) -> Option<ContextFile> {
    for name in CONTEXT_FILE_NAMES {
        let path = dir.join(name);
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                return Some(ContextFile { path, content });
            }
        }
    }
    None
}

/// Format loaded context files into a string suitable for appending to a system prompt.
pub fn format_context(files: &[ContextFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    for cf in files {
        let path_display = cf.path.display();
        parts.push(format!(
            "--- Context from {} ---\n{}",
            path_display, cf.content
        ));
    }

    Some(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_find_context_file_agents_md() {
        let dir = std::env::temp_dir().join("subspace_ctx_test_1");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("AGENTS.md"), "# Project rules\nBe concise.").unwrap();

        let cf = find_context_file(&dir).unwrap();
        assert!(cf.content.contains("Be concise"));
        assert!(cf.path.ends_with("AGENTS.md"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_context_file_claude_md_fallback() {
        let dir = std::env::temp_dir().join("subspace_ctx_test_2");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("CLAUDE.md"), "# Claude rules").unwrap();

        let cf = find_context_file(&dir).unwrap();
        assert!(cf.content.contains("Claude rules"));
        assert!(cf.path.ends_with("CLAUDE.md"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_agents_md_takes_priority_over_claude_md() {
        let dir = std::env::temp_dir().join("subspace_ctx_test_3");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("AGENTS.md"), "agents content").unwrap();
        fs::write(dir.join("CLAUDE.md"), "claude content").unwrap();

        let cf = find_context_file(&dir).unwrap();
        assert!(cf.content.contains("agents content"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_no_context_file() {
        let dir = std::env::temp_dir().join("subspace_ctx_test_4");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert!(find_context_file(&dir).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_walks_ancestors() {
        let base = std::env::temp_dir().join("subspace_ctx_test_5");
        let child = base.join("sub").join("deep");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&child).unwrap();

        fs::write(base.join("AGENTS.md"), "root rules").unwrap();
        fs::write(child.join("AGENTS.md"), "deep rules").unwrap();

        let files = load_context_files(&child);
        // Should find at least the two we created (may also find global)
        let our_files: Vec<_> = files.iter().filter(|f| f.path.starts_with(&base)).collect();
        assert_eq!(our_files.len(), 2);
        // Root-most should come first
        assert!(our_files[0].content.contains("root rules"));
        assert!(our_files[1].content.contains("deep rules"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_format_context() {
        assert!(format_context(&[]).is_none());

        let files = vec![
            ContextFile {
                path: PathBuf::from("/a/AGENTS.md"),
                content: "rule 1".into(),
            },
            ContextFile {
                path: PathBuf::from("/a/b/AGENTS.md"),
                content: "rule 2".into(),
            },
        ];
        let formatted = format_context(&files).unwrap();
        assert!(formatted.contains("rule 1"));
        assert!(formatted.contains("rule 2"));
        assert!(formatted.contains("/a/AGENTS.md"));
    }
}
