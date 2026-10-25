//! Skills: directories containing a `SKILL.md` with `name` / `description`
//! frontmatter followed by instructions.
//!
//! Only the name, description, and path are injected into the system prompt;
//! the worker reads the body on demand. Skills resolve by layered lookup —
//! worker `skills/` first, then the global `<data_dir>/skills/` — and the
//! first match by name wins, so a worker can override a shared skill.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

pub const SKILL_FILE: &str = "SKILL.md";

/// Per-worker skill settings, stored under `[skills]` in `worker.toml`.
/// Every discovered skill is enabled unless listed in `disabled`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSelection {
    #[serde(default)]
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    Worker,
    Global,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub path: PathBuf,
    pub source: SkillSource,
}

/// Result of scanning a skills directory.
#[derive(Debug, Default)]
pub struct SkillListing {
    pub skills: Vec<Skill>,
    /// Skill directories whose `SKILL.md` couldn't be read or parsed.
    pub invalid: Vec<(PathBuf, anyhow::Error)>,
}

/// Scan `dir` for `<name>/SKILL.md`. A missing `dir` yields an empty listing.
pub fn discover(dir: &Path, source: SkillSource) -> Result<SkillListing> {
    let mut listing = SkillListing::default();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        if dir_name.starts_with('.') {
            continue;
        }
        // `is_file` follows symlinks, so symlinked skill dirs are found too.
        let path = entry.path().join(SKILL_FILE);
        if !path.is_file() {
            continue;
        }
        let loaded = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))
            .and_then(|text| parse_frontmatter(&text));
        match loaded {
            Ok(fm) => listing.skills.push(Skill {
                name: fm.name.unwrap_or(dir_name),
                description: fm.description,
                path: fs::canonicalize(&path).unwrap_or(path),
                source,
            }),
            Err(e) => listing.invalid.push((path, e)),
        }
    }
    listing.skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(listing)
}

/// Resolve the skills a worker sees: worker skills override global skills of
/// the same name, then `selection.disabled` is applied. Sorted by name.
pub fn resolve(
    worker: SkillListing,
    global: SkillListing,
    selection: &SkillSelection,
) -> Vec<Skill> {
    let disabled: HashSet<&str> = selection.disabled.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut out: Vec<Skill> = worker
        .skills
        .into_iter()
        .chain(global.skills)
        .filter(|s| seen.insert(s.name.clone()))
        .filter(|s| !disabled.contains(s.name.as_str()))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The `<skills>` block appended to a worker's system prompt, or `None` if
/// there are no skills.
pub fn format_skills(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut out = String::from(
        "<skills>\n\
         The following skills provide specialized instructions for specific tasks.\n\
         When a task matches a skill's description, read its SKILL.md at the given \
         location before proceeding.\n",
    );
    for s in skills {
        out.push_str(&format!(
            "<skill>\n<name>{}</name>\n<description>{}</description>\n<location>{}</location>\n</skill>\n",
            escape(&s.name),
            escape(&s.description),
            escape(&s.path.to_string_lossy()),
        ));
    }
    out.push_str("</skills>");
    Some(out)
}

/// A worker's full system prompt: its role followed by the skills block.
pub fn compose_system_prompt(role: &str, skills: &[Skill]) -> String {
    match format_skills(skills) {
        None => role.to_string(),
        Some(block) if role.trim().is_empty() => block,
        Some(block) => format!("{}\n\n{block}", role.trim_end()),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: String,
}

/// Parse the `---`-delimited frontmatter of a `SKILL.md`.
///
/// Supports single-line `key: value` pairs (optionally quoted) and values
/// continued on indented lines (including YAML `>` / `|` block markers, which
/// are folded into one line). Unknown keys are ignored.
pub fn parse_frontmatter(text: &str) -> Result<Frontmatter> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        bail!("missing frontmatter (SKILL.md must start with `---`)");
    }

    let mut fields: Vec<(String, String)> = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = fields.last_mut() {
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim());
            }
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid frontmatter line: {line:?}"))?;
        let value = value.trim();
        let value = if matches!(value, ">" | "|" | ">-" | "|-") {
            ""
        } else {
            value
        };
        fields.push((key.trim().to_string(), unquote(value).to_string()));
    }
    if !closed {
        bail!("unterminated frontmatter (missing closing `---`)");
    }

    let get = |k: &str| {
        fields
            .iter()
            .rev()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    let description = get("description").ok_or_else(|| anyhow!("missing `description`"))?;
    Ok(Frontmatter {
        name: get("name"),
        description,
    })
}

fn unquote(s: &str) -> &str {
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
