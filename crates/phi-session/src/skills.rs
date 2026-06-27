//! Skill discovery, mirroring pi's [Agent Skills standard](https://agentskills.io)
//! implementation.
//!
//! Skills are self-contained capability packages discovered from `SKILL.md`
//! files. Each skill's frontmatter provides a `name` and `description`,
//! injected as an `<available_skills>` block in the system prompt. The model
//! reads the full `SKILL.md` on-demand when a task matches (progressive
//! disclosure).
//!
//! # Discovery locations
//!
//! Scanned by default (all may exist; missing dirs are skipped):
//!   - `~/.config/phi/skills/`  — global user skills
//!   - `<cwd>/.phi/skills/`     — project-level skills
//!   - Phi's own data dir       — bundled/installed skills
//!
//! Additional paths from [`Config::skills`](crate::config::Config::skills) are
//! scanned after the defaults. Paths support `~` expansion and are resolved
//! relative to `cwd`.
//!
//! # Discovery rules
//!
//! - Directories containing `SKILL.md` are treated as a skill root (no further
//!   recursion into that directory).
//! - Direct root `.md` files in skill directories are loaded as individual
//!   skills.
//! - Subdirectories without `SKILL.md` are recursed into.
//! - Hidden entries (starting with `.`) are skipped.
//!
//! # Frontmatter
//!
//! `SKILL.md` files begin with YAML frontmatter delimited by `---`:
//! ```markdown
//! ---
//! name: my-skill
//! description: What this skill does and when to use it.
//! ---
//! ```
//! `name` falls back to the parent directory name if omitted. `description`
//! is required — skills without it are skipped.

use std::path::{Path, PathBuf};

use phi_rlm::Skill;

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// Discover skills from default locations and user-configured paths.
///
/// Default locations (all optional, missing dirs are skipped):
///   - `~/.config/phi/skills/`  (global)
///   - `<cwd>/.phi/skills/`     (project)
///   - Phi data dir `/skills/`  (bundled/installed)
///
/// `extra_paths` are additional dirs/files from the user's `config.lua`:
/// `skills = { "~/.claude/skills", ".forge/skills" }`. They are scanned after
/// the defaults. `~` is expanded to the home directory; relative paths are
/// resolved against `cwd`.
pub fn discover_skills(cwd: &Path, extra_paths: &[String]) -> Vec<Skill> {
    let mut skills = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Default locations.
    let default_dirs: Vec<PathBuf> = {
        let mut dirs = Vec::new();

        // Global: ~/.config/phi/skills/
        if let Some(config_dir) = dirs::config_dir() {
            dirs.push(config_dir.join("phi").join("skills"));
        }

        // Project: <cwd>/.phi/skills/
        dirs.push(cwd.join(".phi").join("skills"));

        // Phi's own data dir (wherever Phi is stored).
        if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "phi") {
            dirs.push(proj_dirs.data_dir().join("skills"));
        }

        dirs
    };

    for dir in &default_dirs {
        load_skills_from_dir(dir, true, &mut skills, &mut seen_names);
    }

    // User-configured extra paths.
    for raw in extra_paths {
        let expanded = expand_path(raw, cwd);
        let path = Path::new(&expanded);
        if path.is_dir() {
            load_skills_from_dir(path, true, &mut skills, &mut seen_names);
        } else if path.is_file() && path.extension().is_some_and(|e| e == "md") {
            load_skill_from_file(path, &mut skills, &mut seen_names);
        }
    }

    skills
}

/// Recursively load skills from a directory.
///
/// - If the directory contains `SKILL.md`, load it and return (no recursion).
/// - Otherwise, load direct `.md` children (if `include_root_files`) and
///   recurse into subdirectories.
fn load_skills_from_dir(
    dir: &Path,
    include_root_files: bool,
    skills: &mut Vec<Skill>,
    seen_names: &mut std::collections::HashSet<String>,
) {
    // Canonicalize to resolve symlinks and verify existence.
    let dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(_) => return, // dir doesn't exist
    };

    load_skills_from_dir_internal(&dir, include_root_files, skills, seen_names);
}

fn load_skills_from_dir_internal(
    dir: &Path,
    include_root_files: bool,
    skills: &mut Vec<Skill>,
    seen_names: &mut std::collections::HashSet<String>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut entry_vec: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        entry_vec.push(entry.path());
    }
    entry_vec.sort();

    // First pass: look for SKILL.md. If found, load it and return.
    for path in &entry_vec {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if file_name == "SKILL.md" && path.is_file() {
            load_skill_from_file(path, skills, seen_names);
            return;
        }
    }

    // Second pass: process subdirectories and root .md files.
    for path in &entry_vec {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Skip hidden entries.
        if file_name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            load_skills_from_dir_internal(path, false, skills, seen_names);
        } else if path.is_file() && include_root_files && file_name.ends_with(".md") {
            load_skill_from_file(path, skills, seen_names);
        }
    }
}

/// Load a single skill from a `SKILL.md` (or root `.md`) file.
fn load_skill_from_file(
    path: &Path,
    skills: &mut Vec<Skill>,
    seen_names: &mut std::collections::HashSet<String>,
) {
    let raw = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return,
    };

    let fm = match parse_frontmatter(&raw) {
        Some(f) => f,
        None => return, // no frontmatter, skip
    };

    // description is required.
    let description = match fm.description {
        Some(d) if !d.trim().is_empty() => d.trim().to_owned(),
        _ => return, // missing description → skip
    };

    // name: frontmatter or parent dir name.
    let parent_dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed");
    let name = fm.name.unwrap_or_else(|| parent_dir_name.to_owned());

    // Validate name (warn but still load, like pi).
    if name.len() > MAX_NAME_LENGTH {
        eprintln!(
            "warning: skill name '{}' exceeds {} characters (loaded anyway)",
            name, MAX_NAME_LENGTH
        );
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        eprintln!(
            "warning: skill name '{}' contains invalid characters (loaded anyway)",
            name
        );
    }

    // Description length warning.
    if description.len() > MAX_DESCRIPTION_LENGTH {
        eprintln!(
            "warning: skill '{}' description exceeds {} characters (loaded anyway)",
            name, MAX_DESCRIPTION_LENGTH
        );
    }

    // Name collision: keep first found, warn.
    if !seen_names.insert(name.clone()) {
        eprintln!(
            "warning: skill name '{}' collision, keeping first found (at {})",
            name,
            path.display()
        );
        return;
    }

    skills.push(Skill {
        name,
        description,
        path: Some(path.display().to_string()),
    });
}

/// Parsed frontmatter from a `SKILL.md` file.
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

/// Parse YAML frontmatter from markdown content.
///
/// Frontmatter is delimited by `---` at the start of the file:
/// ```text
/// ---
/// name: my-skill
/// description: What this skill does.
/// ---
/// ```
///
/// Only `name` and `description` are extracted. Values may be single-line or
/// multi-line (folded with continuation indentation). This is a minimal parser
/// that handles the common cases without a full YAML dependency.
fn parse_frontmatter(content: &str) -> Option<SkillFrontmatter> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }

    // Find the closing `---`.
    let after_open = &content[3..];
    let rest = after_open.trim_start_matches('\n');
    let end = rest.find("\n---")?;
    let body = &rest[..end];

    let mut name = None;
    let mut description = None;

    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Split on first ':'.
        let colon = match trimmed.find(':') {
            Some(c) => c,
            None => continue,
        };
        let key = trimmed[..colon].trim();
        let mut value = trimmed[colon + 1..].trim().to_owned();

        // Handle multi-line values: if value is empty, look for indented continuation.
        if value.is_empty() {
            let mut parts = Vec::new();
            while let Some(&next) = lines.peek() {
                if next.starts_with(' ') || next.starts_with('\t') {
                    parts.push(next.trim().to_owned());
                    lines.next();
                } else {
                    break;
                }
            }
            value = parts.join(" ");
        } else if value.starts_with('>') {
            // Folded scalar: collect continuation lines.
            let first = value.trim_start_matches('>').trim().to_owned();
            let mut parts = vec![first];
            while let Some(&next) = lines.peek() {
                if next.starts_with(' ') || next.starts_with('\t') {
                    parts.push(next.trim().to_owned());
                    lines.next();
                } else {
                    break;
                }
            }
            value = parts.join(" ");
        }

        match key {
            "name" => name = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }

    Some(SkillFrontmatter { name, description })
}

/// Expand `~` to home and resolve relative paths against `cwd`.
fn expand_path(raw: &str, cwd: &Path) -> String {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).display().to_string();
    }
    if raw == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home.display().to_string();
    }
    if !Path::new(raw).is_absolute() {
        return cwd.join(raw).display().to_string();
    }
    raw.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn no_skills_dir_returns_empty() {
        let dir = tempdir().unwrap();
        let skills = discover_skills(dir.path(), &[]);
        assert!(skills.is_empty());
    }

    #[test]
    fn discovers_skill_from_skill_md() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join(".phi").join("skills");
        let my_skill = skills_dir.join("my-skill");
        fs::create_dir_all(&my_skill).unwrap();
        fs::write(
            my_skill.join("SKILL.md"),
            "---\nname: my-skill\ndescription: Does cool things.\n---\n# My Skill\n",
        )
        .unwrap();

        let skills = discover_skills(dir.path(), &[]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "Does cool things.");
    }

    #[test]
    fn name_falls_back_to_parent_dir() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join(".phi").join("skills");
        let pdf = skills_dir.join("pdf-tools");
        fs::create_dir_all(&pdf).unwrap();
        fs::write(
            pdf.join("SKILL.md"),
            "---\ndescription: PDF extraction.\n---\n",
        )
        .unwrap();

        let skills = discover_skills(dir.path(), &[]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf-tools");
        assert_eq!(skills[0].description, "PDF extraction.");
    }

    #[test]
    fn missing_description_skips_skill() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join(".phi").join("skills");
        let bad = skills_dir.join("bad-skill");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("SKILL.md"), "---\nname: bad-skill\n---\n").unwrap();

        let skills = discover_skills(dir.path(), &[]);
        assert!(skills.is_empty());
    }

    #[test]
    fn name_collision_keeps_first() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join(".phi").join("skills");
        let first = skills_dir.join("alpha").join("calendar");
        let second = skills_dir.join("beta").join("calendar");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(
            first.join("SKILL.md"),
            "---\nname: calendar\ndescription: First calendar.\n---\n",
        )
        .unwrap();
        fs::write(
            second.join("SKILL.md"),
            "---\nname: calendar\ndescription: Second calendar.\n---\n",
        )
        .unwrap();

        let skills = discover_skills(dir.path(), &[]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "First calendar.");
    }

    #[test]
    fn discovers_from_extra_paths() {
        let dir = tempdir().unwrap();
        let extra = dir.path().join("custom-skills");
        let skill = extra.join("my-tool");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: my-tool\ndescription: A custom tool.\n---\n",
        )
        .unwrap();

        let skills = discover_skills(dir.path(), &[extra.display().to_string()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-tool");
    }

    #[test]
    fn discovers_from_extra_md_file() {
        let dir = tempdir().unwrap();
        let extra = dir.path().join("standalone.md");
        fs::write(
            &extra,
            "---\nname: standalone\ndescription: A single-file skill.\n---\n",
        )
        .unwrap();

        let skills = discover_skills(dir.path(), &[extra.display().to_string()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "standalone");
    }

    #[test]
    fn parses_multiline_description() {
        let content = "---\nname: test\ndescription:\n  First part.\n  Second part.\n---\n";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.name.as_deref(), Some("test"));
        assert_eq!(
            fm.description.as_deref(),
            Some("First part. Second part.")
        );
    }

    #[test]
    fn no_frontmatter_returns_none() {
        let content = "# Just a markdown file\nNo frontmatter here.";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn nested_skill_directories() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join(".phi").join("skills");
        let nested = skills_dir.join("category").join("deep-skill");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("SKILL.md"),
            "---\nname: deep-skill\ndescription: A deeply nested skill.\n---\n",
        )
        .unwrap();

        let skills = discover_skills(dir.path(), &[]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "deep-skill");
    }

    #[test]
    fn expand_tilde() {
        let dir = tempdir().unwrap();
        let result = expand_path("~/foo", dir.path());
        assert!(result.ends_with("/foo"));
        assert!(!result.contains("~"));
    }

    #[test]
    fn expand_relative() {
        let dir = tempdir().unwrap();
        let result = expand_path("relative/path", dir.path());
        assert_eq!(
            result,
            dir.path().join("relative/path").display().to_string()
        );
    }
}
