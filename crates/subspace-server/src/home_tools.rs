//! Home-scoped file tools: `home_read`, `home_write`, `home_edit`.
//!
//! These wrap the engine's read/write/edit tools with a path guard, so a
//! worker can manage its skills and memory even when unrestricted core tools
//! are off. Reads are allowed within the worker's home, the global skills dir,
//! and the (real) directories of skills offered to the worker. Writes are
//! allowed only under the home's `skills/` and `memory/`, so `worker.toml` and
//! `ROLE.md` stay read-only to the worker.

use std::fs;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use subspace::tools::edit::EditTool;
use subspace::tools::read::ReadTool;
use subspace::tools::write::WriteTool;
use subspace::tools::{OnUpdate, Tool, ToolDefinition, ToolResult};

use crate::home::{MEMORY_DIR, SKILLS_DIR};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The filesystem areas a worker may read and write. All roots are
/// canonicalized, so comparisons see through symlinks.
#[derive(Debug)]
pub struct HomeScope {
    home: PathBuf,
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
}

impl HomeScope {
    /// `skill_dirs` are the directories of skills offered to this worker; they
    /// may live outside the home or global dir when skills are symlinked.
    pub fn new(
        home: &Path,
        global_skills: &Path,
        skill_dirs: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        let canon =
            |p: &Path| fs::canonicalize(p).with_context(|| format!("resolving {}", p.display()));
        let home = canon(home)?;
        let mut write_roots = Vec::new();
        for sub in [SKILLS_DIR, MEMORY_DIR] {
            let dir = home.join(sub);
            fs::create_dir_all(&dir)?;
            write_roots.push(canon(&dir)?);
        }
        let mut read_roots = vec![home.clone()];
        if global_skills.exists() {
            read_roots.push(canon(global_skills)?);
        }
        for dir in skill_dirs {
            if let Ok(dir) = fs::canonicalize(&dir) {
                read_roots.push(dir);
            }
        }
        Ok(Self {
            home,
            read_roots,
            write_roots,
        })
    }

    /// The worker's (canonical) home directory.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Resolve `path` (relative to the home, or absolute) and check it against
    /// the roots for `access`. Returns the real absolute path, or a message
    /// suitable for the model.
    pub fn resolve(&self, path: &str, access: Access) -> std::result::Result<PathBuf, String> {
        let p = Path::new(path);
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!("'{path}': '..' is not allowed in paths"));
        }
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.home.join(p)
        };
        let real = real_path(&abs).map_err(|e| format!("'{path}': {e}"))?;

        let allowed = match access {
            Access::Read => self.read_roots.iter().any(|r| real.starts_with(r)),
            // Writes must target something *inside* a write root, not the root itself.
            Access::Write => self
                .write_roots
                .iter()
                .any(|r| real.starts_with(r) && real != *r),
        };
        if allowed {
            Ok(real)
        } else {
            Err(match access {
                Access::Read => {
                    format!("'{path}' is outside your home directory and available skills")
                }
                Access::Write => format!(
                    "'{path}' is not writable: you may only write under {SKILLS_DIR}/ and \
                     {MEMORY_DIR}/ in your home directory"
                ),
            })
        }
    }
}

/// Canonicalize the deepest existing ancestor of `path` (following symlinks)
/// and re-append the components that don't exist yet.
fn real_path(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path;
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => break,
        }
    }
    let mut real = fs::canonicalize(existing)?;
    for name in rest.into_iter().rev() {
        real.push(name);
    }
    Ok(real)
}

/// The three home-scoped tools for a worker.
pub fn home_tools(scope: Arc<HomeScope>) -> Vec<Arc<dyn Tool>> {
    let home = scope.home().to_path_buf();
    vec![
        Arc::new(ScopedTool {
            name: "home_read",
            description: "Read a file from your home directory or one of your skills. \
                Paths are relative to your home directory; skill locations from the \
                <skills> list may be given as absolute paths. Output is truncated to \
                2000 lines or 50KB; use offset/limit for large files.",
            access: Access::Read,
            inner: Arc::new(ReadTool::new(home.clone())),
            scope: scope.clone(),
        }),
        Arc::new(ScopedTool {
            name: "home_write",
            description: "Write a file in your home directory, creating parent directories. \
                Only paths under skills/ (your skills, as <name>/SKILL.md) and memory/ \
                (your private notes) are writable. Overwrites existing files.",
            access: Access::Write,
            inner: Arc::new(WriteTool::new(home.clone())),
            scope: scope.clone(),
        }),
        Arc::new(ScopedTool {
            name: "home_edit",
            description: "Edit a file in your home directory by replacing exact text. \
                Only paths under skills/ and memory/ are editable. oldText must match \
                exactly once.",
            access: Access::Write,
            inner: Arc::new(EditTool::new(home)),
            scope,
        }),
    ]
}

/// An engine tool whose `path` argument is confined by a [`HomeScope`].
struct ScopedTool {
    name: &'static str,
    description: &'static str,
    access: Access,
    inner: Arc<dyn Tool>,
    scope: Arc<HomeScope>,
}

// `Tool` is an `#[async_trait]`; this is its desugared form, which avoids a
// dependency on `async-trait` in this crate.
impl Tool for ScopedTool {
    fn definition(&self) -> ToolDefinition {
        let mut def = self.inner.definition();
        def.name = self.name.to_string();
        def.description = self.description.to_string();
        if let Some(path) = def.parameters.pointer_mut("/properties/path/description") {
            *path = serde_json::Value::String(
                "Path relative to your home directory (or an absolute skill location)".into(),
            );
        }
        def
    }

    fn execute<'a, 'b, 'c>(
        &'a self,
        mut arguments: serde_json::Value,
        on_update: &'b OnUpdate,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'c>>
    where
        'a: 'c,
        'b: 'c,
        Self: 'c,
    {
        Box::pin(async move {
            let Some(path) = arguments.get("path").and_then(|p| p.as_str()) else {
                return Ok(ToolResult::error(
                    "Invalid arguments: missing string `path`",
                ));
            };
            match self.scope.resolve(path, self.access) {
                Ok(real) => {
                    arguments["path"] = serde_json::Value::String(real.to_string_lossy().into());
                    self.inner.execute(arguments, on_update).await
                }
                Err(msg) => Ok(ToolResult::error(msg)),
            }
        })
    }
}
