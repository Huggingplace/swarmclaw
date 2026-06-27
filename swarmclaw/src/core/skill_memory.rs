//! Self-improving "learned skills" (PR-9), a CONSERVATIVE v1 inspired by the
//! Hermes Agent. A *learned skill* is a MARKDOWN PROCEDURE NOTE — human/LLM
//! readable text distilled from a just-completed complex task. It is NEVER
//! executed; it is only stored on disk and (optionally) shown/retrieved as
//! context. This is intentionally a SEPARATE, ADDITIVE notes library and does
//! NOT touch the executable [`crate::skills::Skill`] trait / tool system.
//!
//! SAFETY / SCOPE:
//! - The whole feature is strictly OPT-IN and DEFAULT-OFF (env
//!   `SWARMCLAW_SELF_IMPROVE`). With it off the agent never authors anything and
//!   behavior is byte-for-byte unchanged. See [`crate::core::agent::Agent`].
//! - Writes go ONLY under `<workspace_root>/.swarmclaw/skills/`. If no workspace
//!   is configured the feature is a no-op.
//! - A learned skill is plain markdown text, so it can never run.
//!
//! This module hosts the PURE, unit-tested predicates ([`slugify`],
//! [`should_author_skill`]) and the file-backed store ([`save_skill`],
//! [`list_skills`], [`get_skill`]). The (non-testable) authoring LLM call lives
//! in `agent.rs` and stays thin + gated.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Author-on-complex-turn threshold: a turn that executes at least this many
/// tool calls is considered "complex" and (when the feature is enabled) triggers
/// authoring of a learned skill. Matches Hermes' "5+ tool calls" heuristic.
pub const DEFAULT_SKILL_TOOL_THRESHOLD: usize = 5;

/// Upper bound on a generated slug's length, so a long title can't produce an
/// unwieldy filename.
const MAX_SLUG_LEN: usize = 64;

/// A learned skill: a titled markdown procedure note. `name` is the
/// human-readable title, `slug` is the filesystem-safe identifier (also the
/// `<slug>.md` filename stem), and `content` is the markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnedSkill {
    pub name: String,
    pub slug: String,
    pub content: String,
}

/// Turn an arbitrary title into a filesystem-safe slug: lowercase, keep only
/// ASCII alphanumerics (everything else becomes a separator), collapse runs of
/// separators into a single dash, trim leading/trailing dashes, bound the
/// length, and fall back to a non-empty default when nothing usable remains.
///
/// Pure (no I/O), so it is unit-tested.
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len().min(MAX_SLUG_LEN));
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            // Any non-alphanumeric char collapses into a single dash.
            slug.push('-');
            prev_dash = true;
        }
    }
    // Trim leading/trailing dashes.
    let trimmed = slug.trim_matches('-');
    // Bound the length, then re-trim so we never end on a dash.
    let bounded: String = trimmed.chars().take(MAX_SLUG_LEN).collect();
    let bounded = bounded.trim_matches('-').to_string();
    if bounded.is_empty() {
        "learned-skill".to_string()
    } else {
        bounded
    }
}

/// Whether a turn that executed `tool_calls_in_turn` tool calls is "complex"
/// enough to author a learned skill: true when `tool_calls_in_turn >= threshold`.
///
/// Pure, so it is unit-tested.
pub fn should_author_skill(tool_calls_in_turn: usize, threshold: usize) -> bool {
    tool_calls_in_turn >= threshold
}

/// The on-disk directory where learned skills live, given a workspace root:
/// `<workspace_root>/.swarmclaw/skills/`.
pub fn skills_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".swarmclaw").join("skills")
}

/// Front-matter marker line prefix used to persist the human-readable name.
const NAME_PREFIX: &str = "name: ";

/// Save (or overwrite) a learned skill as `<slug>.md` under `dir`, creating the
/// directory as needed. The file is a tiny front-matter line carrying the name
/// followed by a blank line and the markdown content. Writing the same slug
/// overwrites the previous note. Returns the path written.
pub fn save_skill(dir: &Path, skill: &LearnedSkill) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("creating learned-skills dir {}", dir.display()))?;
    let path = dir.join(format!("{}.md", skill.slug));
    // A minimal front-matter line (the name) then the markdown body. We sanitize
    // the name onto a single line so the parser round-trips cleanly.
    let name_line = skill.name.replace(['\r', '\n'], " ");
    let body = format!("{NAME_PREFIX}{name_line}\n\n{}", skill.content);
    fs::write(&path, body)
        .with_context(|| format!("writing learned skill {}", path.display()))?;
    Ok(path)
}

/// Parse a single learned-skill file's contents into a [`LearnedSkill`], using
/// `slug` (the filename stem) as the slug. The first line, when it starts with
/// the `name:` front-matter prefix, supplies the name (and a following blank
/// line is dropped from the content); otherwise the name falls back to the slug
/// and the whole file is the content.
fn parse_skill(slug: &str, raw: &str) -> LearnedSkill {
    if let Some(rest) = raw.strip_prefix(NAME_PREFIX) {
        // First line is the name; the remainder (after the newline) is content.
        let (name, content) = match rest.split_once('\n') {
            Some((name, after)) => {
                // Drop a single leading blank line between front-matter and body.
                let content = after.strip_prefix('\n').unwrap_or(after);
                (name.trim().to_string(), content.to_string())
            }
            None => (rest.trim().to_string(), String::new()),
        };
        let name = if name.is_empty() {
            slug.to_string()
        } else {
            name
        };
        LearnedSkill {
            name,
            slug: slug.to_string(),
            content,
        }
    } else {
        LearnedSkill {
            name: slug.to_string(),
            slug: slug.to_string(),
            content: raw.to_string(),
        }
    }
}

/// List all learned skills stored in `dir` (every `*.md` file), parsed into
/// [`LearnedSkill`] values. A missing directory yields an empty list (never an
/// error), so callers don't have to special-case a fresh workspace. Results are
/// sorted by slug for stable output.
pub fn list_skills(dir: &Path) -> Result<Vec<LearnedSkill>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries =
        fs::read_dir(dir).with_context(|| format!("reading learned-skills dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("reading learned skill {}", path.display()))?;
        out.push(parse_skill(slug, &raw));
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Fetch a single learned skill by slug from `dir`, or `None` if no such
/// `<slug>.md` exists (a missing directory also yields `None`).
pub fn get_skill(dir: &Path, slug: &str) -> Result<Option<LearnedSkill>> {
    let path = dir.join(format!("{slug}.md"));
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("reading learned skill {}", path.display()))?;
    Ok(Some(parse_skill(slug, &raw)))
}

/// Delete a learned skill by slug from `dir`. Returns `true` if a file was
/// removed, `false` if there was nothing to remove. Used by the optional
/// `/forget` command.
pub fn forget_skill(dir: &Path, slug: &str) -> Result<bool> {
    let path = dir.join(format!("{slug}.md"));
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path)
        .with_context(|| format!("removing learned skill {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- slugify -----------------------------------------------------------

    #[test]
    fn slugify_basic_spaces_and_case() {
        assert_eq!(slugify("Deploy The App"), "deploy-the-app");
    }

    #[test]
    fn slugify_collapses_punctuation_and_repeats() {
        assert_eq!(slugify("Fix:  bug!! in   parser"), "fix-bug-in-parser");
    }

    #[test]
    fn slugify_trims_leading_trailing_separators() {
        assert_eq!(slugify("  ...hello world...  "), "hello-world");
    }

    #[test]
    fn slugify_empty_falls_back() {
        assert_eq!(slugify(""), "learned-skill");
        assert_eq!(slugify("   "), "learned-skill");
        assert_eq!(slugify("!!!"), "learned-skill");
    }

    #[test]
    fn slugify_bounds_length_and_no_trailing_dash() {
        let long = "a".repeat(200);
        let s = slugify(&long);
        assert_eq!(s.len(), MAX_SLUG_LEN);
        assert!(!s.ends_with('-'));
    }

    #[test]
    fn slugify_bounded_then_retrimmed() {
        // A title that would be cut exactly at a dash boundary must not end on a
        // dash after bounding.
        let title = format!("{}-tail", "x".repeat(MAX_SLUG_LEN - 1));
        let s = slugify(&title);
        assert!(!s.ends_with('-'));
        assert!(s.len() <= MAX_SLUG_LEN);
    }

    // --- should_author_skill ----------------------------------------------

    #[test]
    fn should_author_skill_below_threshold() {
        assert!(!should_author_skill(4, DEFAULT_SKILL_TOOL_THRESHOLD));
        assert!(!should_author_skill(0, DEFAULT_SKILL_TOOL_THRESHOLD));
    }

    #[test]
    fn should_author_skill_at_threshold() {
        assert!(should_author_skill(5, DEFAULT_SKILL_TOOL_THRESHOLD));
    }

    #[test]
    fn should_author_skill_above_threshold() {
        assert!(should_author_skill(9, DEFAULT_SKILL_TOOL_THRESHOLD));
    }

    // --- store round-trip --------------------------------------------------

    fn temp_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let unique = format!(
            "swarmclaw-skills-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        p.push(unique);
        p
    }

    #[test]
    fn save_then_list_and_get_round_trip() {
        let dir = temp_dir();
        let skill = LearnedSkill {
            name: "Deploy The App".to_string(),
            slug: slugify("Deploy The App"),
            content: "1. Build\n2. Ship\n".to_string(),
        };
        let path = save_skill(&dir, &skill).expect("save");
        assert!(path.exists());
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "deploy-the-app.md");

        let listed = list_skills(&dir).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], skill);

        let got = get_skill(&dir, "deploy-the-app").expect("get");
        assert_eq!(got, Some(skill));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_same_slug_overwrites() {
        let dir = temp_dir();
        let first = LearnedSkill {
            name: "Thing".to_string(),
            slug: "thing".to_string(),
            content: "old".to_string(),
        };
        let second = LearnedSkill {
            name: "Thing".to_string(),
            slug: "thing".to_string(),
            content: "new content".to_string(),
        };
        save_skill(&dir, &first).expect("save first");
        save_skill(&dir, &second).expect("save second");

        let listed = list_skills(&dir).expect("list");
        assert_eq!(listed.len(), 1, "same slug should overwrite, not duplicate");
        assert_eq!(listed[0].content, "new content");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_unknown_is_none() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(get_skill(&dir, "nope").expect("get"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_missing_or_empty_dir_is_empty() {
        // Missing directory.
        let missing = temp_dir();
        assert!(list_skills(&missing).expect("list missing").is_empty());

        // Empty (but existing) directory.
        let empty = temp_dir();
        fs::create_dir_all(&empty).unwrap();
        assert!(list_skills(&empty).expect("list empty").is_empty());
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn forget_removes_then_reports_missing() {
        let dir = temp_dir();
        let skill = LearnedSkill {
            name: "Temp".to_string(),
            slug: "temp".to_string(),
            content: "x".to_string(),
        };
        save_skill(&dir, &skill).expect("save");
        assert!(forget_skill(&dir, "temp").expect("forget"));
        assert!(!forget_skill(&dir, "temp").expect("forget again"));
        assert!(get_skill(&dir, "temp").expect("get").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_skill_without_front_matter_uses_slug_as_name() {
        let parsed = parse_skill("my-slug", "just body text");
        assert_eq!(parsed.name, "my-slug");
        assert_eq!(parsed.slug, "my-slug");
        assert_eq!(parsed.content, "just body text");
    }
}
