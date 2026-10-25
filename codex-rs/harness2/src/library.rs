//! The per-domain experience library R: adopted harnesses plus the lesson that earned them.
//!
//! Layout mirrors google-research/harness2:
//!
//! ```text
//! <root>/<domain>/INDEX.md
//! <root>/<domain>/harnesses/<task>/harness/<rel>
//! <root>/<domain>/harnesses/<task>/EXPERIENCE.md
//! ```
//!
//! Retrieval is by construction: a domain directory only holds same-domain entries.

use crate::surface;
use crate::surface::Tree;
use anyhow::Context;
use anyhow::Result;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct Experience {
    pub cue: String,
    pub lesson: String,
    pub evidence: String,
    pub confidence: String,
    pub edits: String,
}

impl Experience {
    pub fn has_lesson(&self) -> bool {
        !self.cue.trim().is_empty()
            && !self.lesson.trim().is_empty()
            && !self.evidence.trim().is_empty()
    }
}

pub fn slug(task_id: &str) -> String {
    task_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub fn domain_dir(domain: &str) -> String {
    let collapsed = domain
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("__");
    if collapsed.is_empty() {
        "general".to_string()
    } else {
        collapsed
    }
}

#[derive(Debug, Clone)]
pub struct Library {
    pub root: PathBuf,
    pub domain: String,
}

impl Library {
    pub fn new(root: PathBuf, domain: impl Into<String>) -> Self {
        Self {
            root,
            domain: domain.into(),
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join(domain_dir(&self.domain))
    }

    pub fn index(&self) -> PathBuf {
        self.dir().join("INDEX.md")
    }

    pub fn entry_dir(&self, task_id: &str) -> PathBuf {
        self.dir().join("harnesses").join(slug(task_id))
    }

    pub fn exists(&self) -> bool {
        self.index().is_file()
    }

    pub fn context_dir(&self) -> Option<PathBuf> {
        let dir = self.dir();
        if !dir.is_dir() {
            return None;
        }
        let has_experience = walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name() == "EXPERIENCE.md");
        has_experience.then_some(dir)
    }

    pub fn entries(&self) -> Vec<String> {
        let dir = self.dir().join("harnesses");
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut names = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    pub fn add(
        &self,
        task_id: &str,
        harness: &Tree,
        adopted_label: &str,
        run_id: &str,
        why: &str,
        experience: &Experience,
    ) -> Result<Option<PathBuf>> {
        if !experience.has_lesson() {
            return Ok(None);
        }
        let entry = self.entry_dir(task_id);
        let harness_dir = entry.join("harness");
        if harness_dir.exists() {
            fs::remove_dir_all(&harness_dir)?;
        }
        fs::create_dir_all(&harness_dir)?;
        surface::write_tree(&harness_dir, harness)?;

        let mut lines = vec![
            format!("# {task_id}"),
            String::new(),
            format!("- adopted: `{adopted_label}`  (run `{run_id}`)"),
            format!("- components: {}", surface::inventory_line(harness)),
            String::new(),
        ];
        if !why.trim().is_empty() {
            lines.push("## Why this was chosen".to_string());
            lines.push(why.trim().to_string());
            lines.push(String::new());
        }
        if !experience.edits.trim().is_empty() {
            lines.push("## What each edit did".to_string());
            lines.push(experience.edits.trim().to_string());
            lines.push(String::new());
        }
        lines.push("## Transferable lesson".to_string());
        for (key, value) in [
            ("cue", &experience.cue),
            ("lesson", &experience.lesson),
            ("evidence", &experience.evidence),
            ("confidence", &experience.confidence),
        ] {
            if !value.trim().is_empty() {
                lines.push(format!("- **{key}**: {}", value.trim()));
            }
        }
        lines.push(String::new());
        fs::write(entry.join("EXPERIENCE.md"), lines.join("\n"))?;
        self.rebuild_index()?;
        Ok(Some(entry))
    }

    pub fn rebuild_index(&self) -> Result<PathBuf> {
        fs::create_dir_all(self.dir())?;
        let mut rows = vec![
            format!("# Library — {}", self.domain),
            String::new(),
            "Each entry is an earlier task in THIS domain whose harness was chosen, stored \
with its full component tree and why it was picked. Browse this index, then open the entries \
that look relevant to the matter in front of you."
                .to_string(),
            String::new(),
        ];
        let names = self.entries();
        if names.is_empty() {
            rows.push("_(empty — this is the first task in this domain)_".to_string());
        }
        for name in names {
            let entry = self.dir().join("harnesses").join(&name);
            let tree =
                surface::read_tree(&entry.join("harness"), &mut Vec::new()).unwrap_or_default();
            rows.push(format!("## `harnesses/{name}/`"));
            rows.push(format!(
                "- harness: `harnesses/{name}/harness/` — {}",
                surface::inventory_line(&tree)
            ));
            rows.push(format!("- why:     `harnesses/{name}/EXPERIENCE.md`"));
            if let Some(cue) = field(&entry.join("EXPERIENCE.md"), "cue") {
                rows.push(format!("- cue:     {cue}"));
            }
            rows.push(String::new());
        }
        let path = self.index();
        fs::write(&path, rows.join("\n"))?;
        Ok(path)
    }
}

fn field(path: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let prefix = format!("- **{name}**:");
    text.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// Parse `<experience>` and `<edits>` out of an Experience Reflector reply.
pub fn parse_reflection(reply: &str) -> Experience {
    let mut experience = Experience::default();
    if let Some(start) = reply.find("<edits>")
        && let Some(end) = reply[start..].find("</edits>")
    {
        experience.edits = reply[start + "<edits>".len()..start + end]
            .trim()
            .to_string();
    }
    let Some(start) = reply.find("<experience>") else {
        return experience;
    };
    let Some(end) = reply[start..].find("</experience>") else {
        return experience;
    };
    let body = &reply[start + "<experience>".len()..start + end];
    for line in body.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "cue" => experience.cue = value.to_string(),
            "lesson" => experience.lesson = value.to_string(),
            "evidence" => experience.evidence = value.to_string(),
            "confidence" => experience.confidence = value.to_string(),
            _ => {}
        }
    }
    experience
}

/// Extract the single candidate id from a selector reply's ```choice fence.
pub fn parse_choice(reply: &str) -> Option<String> {
    let start = reply.rfind("```choice")?;
    let rest = &reply[start + "```choice".len()..];
    let end = rest.find("```")?;
    let choice = rest[..end].trim();
    if choice.is_empty() {
        None
    } else {
        Some(choice.to_string())
    }
}

pub fn list_text(library: &Library) -> Result<String> {
    if library.index().is_file() {
        fs::read_to_string(library.index()).context("read library index")
    } else {
        Ok(format!(
            "Library {} for domain {:?} is empty. Run `codex harness2 run` first.",
            library.root.display(),
            library.domain
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reflection_blocks() {
        let reply = "<edits>\ncomponent: AGENTS.md\nverdict: helped\n</edits>\n\
<experience>\ncue: when a table is built\nlesson: add a script\n\
evidence: run base1\nconfidence: medium\n</experience>";
        let experience = parse_reflection(reply);
        assert_eq!(experience.cue, "when a table is built");
        assert_eq!(experience.lesson, "add a script");
        assert_eq!(experience.evidence, "run base1");
        assert!(experience.edits.contains("AGENTS.md"));
        assert!(experience.has_lesson());
    }

    #[test]
    fn parses_the_last_choice_fence() {
        let reply = "thinking\n```choice\ncand02\n```\n";
        assert_eq!(parse_choice(reply).as_deref(), Some("cand02"));
        assert_eq!(parse_choice("no choice"), None);
    }

    #[test]
    fn adding_an_entry_rebuilds_the_index() {
        let temp = tempfile::tempdir().unwrap();
        let library = Library::new(temp.path().to_path_buf(), "general");
        let tree = surface::base_harness();
        let experience = Experience {
            cue: "when a task needs a report".to_string(),
            lesson: "add a report skill".to_string(),
            evidence: "base1 missed the section".to_string(),
            confidence: "medium".to_string(),
            edits: "component: AGENTS.md".to_string(),
        };
        let entry = library
            .add(
                "task-1",
                &tree,
                "agg1",
                "run-1",
                "blind selection",
                &experience,
            )
            .unwrap()
            .expect("entry");
        assert!(entry.join("EXPERIENCE.md").is_file());
        let index = fs::read_to_string(library.index()).unwrap();
        assert!(index.contains("task-1"));
        assert!(index.contains("when a task needs a report"));
        assert_eq!(library.entries(), vec!["task-1"]);
    }

    #[test]
    fn a_reflection_without_a_lesson_does_not_grow_the_library() {
        let temp = tempfile::tempdir().unwrap();
        let library = Library::new(temp.path().to_path_buf(), "general");
        let tree = surface::base_harness();
        let experience = Experience {
            cue: String::new(),
            lesson: String::new(),
            evidence: String::new(),
            ..Experience::default()
        };
        assert!(
            library
                .add("task-2", &tree, "agg1", "run-1", "", &experience)
                .unwrap()
                .is_none()
        );
    }
}
