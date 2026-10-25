//! The editable Codex harness surface used by Harness².
//!
//! A harness is a small file tree rooted at the workspace: always-on `AGENTS.md`,
//! on-demand skills, scripts, delegated agent roles, and lifecycle hooks. This module
//! owns reading, writing, diffing, and validating that tree. It deliberately mirrors
//! the component contract documented by google-research/harness2 for the Codex
//! substrate so harnesses produced there remain portable.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use regex_lite::Regex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use walkdir::WalkDir;

pub type Tree = BTreeMap<String, String>;

pub const ALWAYS_ON: &[&str] = &["AGENTS.md"];
pub const ALWAYS_ON_MAX: usize = 8_000;
pub const SKILL_DIR: &str = ".agents/skills";
pub const HOOKS_FILE: &str = ".codex/hooks.json";
pub const RESERVED_SCRIPTS: &[&str] = &["read_doc.py", "pack.py", "unpack.py"];

pub const CAP_REASON: &str = "AGENTS.md is injected whole into every turn, and Codex \
additionally truncates it silently at 32,768 bytes";

/// Placeholder files that teach an editor which levers exist without entering the tree.
pub const STUBS: &[(&str, &str)] = &[
    (
        "scripts/README.md",
        "<!-- deterministic helpers, run as `python3 scripts/<name>.py`. Add one by \
emitting scripts/<name>.py. -->\n",
    ),
    (
        "sub_agents/README.md",
        "<!-- delegated Codex agent roles, each spawned in ITS OWN context. Add one as \
sub_agents/<name>/AGENT.md with frontmatter `description:`; the runner installs it \
as a named agent role. -->\n",
    ),
    (
        ".codex/README.md",
        "<!-- lifecycle hooks, the only lever that acts INSIDE the tool loop. Add ONE file, \
.codex/hooks.json, Claude-Code hook schema: {\"hooks\": {\"PostToolUse\": [{\"matcher\": \
\"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"...\"}]}]}}. A PostToolUse \
command that prints {\"decision\":\"block\",\"reason\":\"...\"} feeds the reason back to \
the model. -->\n",
    ),
    (
        ".agents/skills/README.md",
        "<!-- reusable workflows. Add one as .agents/skills/<name>/SKILL.md with YAML \
frontmatter: `description:` is REQUIRED (no frontmatter = skill silently dropped); \
`name:` is optional and must match the directory. Only name+description are advertised; \
the body is read on demand. -->\n",
    ),
];

fn name_pattern() -> &'static str {
    r"[a-z0-9][a-z0-9_\-]*"
}

struct Patterns {
    skill: Regex,
    skill_script: Regex,
    subagent: Regex,
    script: Regex,
    script_ref: Regex,
}

fn patterns() -> &'static Patterns {
    use std::sync::LazyLock;
    static PATTERNS: LazyLock<Patterns> = LazyLock::new(|| {
        let name = name_pattern();
        Patterns {
            skill: Regex::new(&format!(r"^\.agents/skills/({name})/SKILL\.md$"))
                .expect("valid skill regex"),
            skill_script: Regex::new(&format!(
                r"^\.agents/skills/{name}/scripts/[A-Za-z0-9_.\-]+$"
            ))
            .expect("valid skill script regex"),
            subagent: Regex::new(&format!(r"^sub_agents/({name})/AGENT\.md$"))
                .expect("valid subagent regex"),
            script: Regex::new(r"^scripts/[a-z0-9_][a-z0-9_]*\.py$").expect("valid script regex"),
            script_ref: Regex::new(
                r"(?:python3?\s+)((?:\.agents/skills/[\w\-]+/)?scripts/[\w.\-]+\.py)",
            )
            .expect("valid script reference regex"),
        }
    });
    &PATTERNS
}

/// Normalise a relative path, rejecting escapes before it is joined to a directory.
pub fn normalize_rel(rel: &str) -> Result<String> {
    let mut value = rel.trim().replace('\\', "/");
    if value.starts_with('/') || value.split('/').any(|part| part == "..") {
        bail!("path escapes the harness: {rel:?}");
    }
    while let Some(rest) = value.strip_prefix("./") {
        value = rest.to_string();
    }
    Ok(value.trim_matches('/').to_string())
}

pub fn is_allowed(rel: &str) -> bool {
    let Ok(rel) = normalize_rel(rel) else {
        return false;
    };
    if ALWAYS_ON.contains(&rel.as_str()) || rel == HOOKS_FILE {
        return true;
    }
    let p = patterns();
    p.skill.is_match(&rel)
        || p.skill_script.is_match(&rel)
        || p.subagent.is_match(&rel)
        || p.script.is_match(&rel)
}

pub fn skill_names(tree: &Tree) -> Vec<String> {
    let p = patterns();
    tree.keys()
        .filter_map(|rel| p.skill.captures(rel).map(|c| c[1].to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn subagent_names(tree: &Tree) -> Vec<String> {
    let p = patterns();
    tree.keys()
        .filter_map(|rel| p.subagent.captures(rel).map(|c| c[1].to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn is_stub(rel: &str) -> bool {
    STUBS.iter().any(|(path, _)| *path == rel)
}

/// Read every file on the editable surface; README stubs are excluded.
pub fn read_tree(root: &Path, rejected: &mut Vec<String>) -> Result<Tree> {
    let mut tree = Tree::new();
    if !root.is_dir() {
        return Ok(tree);
    }
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file()
            || entry
                .path()
                .components()
                .any(|c| c.as_os_str() == "__pycache__")
        {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .context("strip harness prefix")?
            .to_string_lossy()
            .replace('\\', "/");
        if is_stub(&rel) {
            continue;
        }
        if !is_allowed(&rel) {
            rejected.push(rel);
            continue;
        }
        let text = fs::read_to_string(entry.path())
            .with_context(|| format!("read harness file {}", entry.path().display()))?;
        tree.insert(rel, text);
    }
    Ok(tree)
}

/// Materialise an exact editable tree. Files outside the surface are preserved.
pub fn write_tree(root: &Path, tree: &Tree) -> Result<()> {
    fs::create_dir_all(root)?;
    let mut editable = Vec::new();
    if root.is_dir() {
        for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            if is_allowed(&rel) || is_stub(&rel) {
                editable.push(entry.path().to_path_buf());
            }
        }
    }
    for path in editable {
        fs::remove_file(path)?;
    }
    for rel in ALWAYS_ON {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !tree.contains_key(*rel) && !path.exists() {
            fs::write(&path, "")?;
        }
    }
    for (rel, content) in tree {
        let rel = normalize_rel(rel)?;
        let path = root.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content).with_context(|| format!("write {rel}"))?;
    }
    for (rel, body) in STUBS {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(path, body)?;
        }
    }
    Ok(())
}

pub fn always_on_text(tree: &Tree) -> String {
    ALWAYS_ON
        .iter()
        .map(|rel| tree.get(*rel).map(String::as_str).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Give every skill a trigger line in AGENTS.md; returns the names repaired.
pub fn autotrigger(tree: &mut Tree) -> Vec<String> {
    let mut fixed = Vec::new();
    let mut body = always_on_text(tree);
    for name in skill_names(tree) {
        if body.contains(&name) {
            continue;
        }
        let skill = tree
            .get(&format!("{SKILL_DIR}/{name}/SKILL.md"))
            .cloned()
            .unwrap_or_default();
        let description = frontmatter_value(head(&skill, 600), "description")
            .unwrap_or_else(|| format!("the {name} workflow applies"));
        let line = format!(
            "- When {}, read {SKILL_DIR}/{name}/SKILL.md and follow it.\n",
            description.trim_end_matches('.')
        );
        let current = tree.get("AGENTS.md").cloned().unwrap_or_default();
        if always_on_text(tree).len() + line.len() + 2 > ALWAYS_ON_MAX {
            continue;
        }
        let next = if current.trim().is_empty() {
            line
        } else {
            format!("{}\n{}", current.trim_end(), line)
        };
        tree.insert("AGENTS.md".to_string(), next);
        body = always_on_text(tree);
        fixed.push(name);
    }
    fixed
}

fn frontmatter_value(source: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    source.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix(prefix.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn head(source: &str, max: usize) -> &str {
    let mut end = source.len().min(max);
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }
    &source[..end]
}

fn check_hooks(source: &str, errors: &mut Vec<String>) {
    let doc: Value = match serde_json::from_str(source) {
        Ok(doc) => doc,
        Err(err) => {
            errors.push(format!("{HOOKS_FILE} is not valid JSON: {err}"));
            return;
        }
    };
    let Some(hooks) = doc.get("hooks").and_then(Value::as_object) else {
        errors.push(format!(
            "{HOOKS_FILE} needs a top-level {{\"hooks\": {{\"<Event>\": [...]}}}} object"
        ));
        return;
    };
    if hooks.is_empty() {
        errors.push(format!("{HOOKS_FILE} needs at least one hook event"));
        return;
    }
    let known = [
        "PreToolUse",
        "PostToolUse",
        "PermissionRequest",
        "PreCompact",
        "PostCompact",
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
        "UserPromptSubmit",
        "Stop",
    ];
    for (event, entries) in hooks {
        if !known.contains(&event.as_str()) {
            errors.push(format!(
                "{HOOKS_FILE}: unknown event {event:?}; expected one of {}",
                known.join(", ")
            ));
            continue;
        }
        let Some(entries) = entries.as_array() else {
            errors.push(format!(
                "{HOOKS_FILE}: {event} must map to a LIST of matcher entries"
            ));
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            let Some(hooks) = entry.get("hooks").and_then(Value::as_array) else {
                errors.push(format!(
                    "{HOOKS_FILE}: {event}[{index}] needs a `hooks` list"
                ));
                continue;
            };
            for hook in hooks {
                if hook.get("type").and_then(Value::as_str) != Some("command") {
                    errors.push(format!(
                        "{HOOKS_FILE}: {event}[{index}] hook type must be \"command\""
                    ));
                } else if hook
                    .get("command")
                    .and_then(Value::as_str)
                    .is_none_or(|command| command.trim().is_empty())
                {
                    errors.push(format!(
                        "{HOOKS_FILE}: {event}[{index}] has an empty `command`"
                    ));
                }
            }
        }
    }
}

fn python_compiles(rel: &str, source: &str, errors: &mut Vec<String>) {
    let mut child = match Command::new("python3")
        .arg("-c")
        .arg("import sys; compile(sys.stdin.read(), sys.argv[1], 'exec')")
        .arg(rel)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return,
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(source.as_bytes());
    }
    if let Ok(output) = child.wait_with_output()
        && !output.status.success()
    {
        let detail = String::from_utf8_lossy(&output.stderr);
        errors.push(format!(
            "{rel} does not compile: {}",
            detail
                .trim()
                .lines()
                .next()
                .unwrap_or("python3 rejected the file")
        ));
    }
}

fn check_skills(tree: &Tree, body: &str, errors: &mut Vec<String>) {
    for name in skill_names(tree) {
        let source = &tree[&format!("{SKILL_DIR}/{name}/SKILL.md")];
        let head = head(source, 600);
        if frontmatter_value(head, "description").is_none() {
            errors.push(format!(
                "{SKILL_DIR}/{name}/SKILL.md needs a `description:` in its frontmatter, \
or the skill is dropped silently at load"
            ));
        }
        if let Some(embedded) = frontmatter_value(head, "name")
            && embedded != name
        {
            errors.push(format!(
                "{SKILL_DIR}/{name}/SKILL.md frontmatter `name: {embedded}` overrides the \
directory name at load, so every trigger line naming `{name}` stops matching; \
drop the `name:` line or make it `name: {name}`"
            ));
        }
        if !body.contains(&name) {
            errors.push(format!(
                "skill `{name}` is never named in AGENTS.md, so the agent will not know to \
invoke it. Add a trigger line naming `{name}`."
            ));
        }
    }
}

fn check_subagents(tree: &Tree, errors: &mut Vec<String>) {
    for name in subagent_names(tree) {
        let source = &tree[&format!("sub_agents/{name}/AGENT.md")];
        let head = head(source, 600);
        if frontmatter_value(head, "description").is_none() {
            errors.push(format!("sub_agents/{name}/AGENT.md needs a `description:`"));
        }
    }
}

fn check_script_refs(tree: &Tree, errors: &mut Vec<String>) {
    let p = patterns();
    let have: BTreeSet<&str> = tree
        .keys()
        .filter(|rel| p.script.is_match(rel) || p.skill_script.is_match(rel))
        .map(String::as_str)
        .collect();
    for (rel, source) in tree {
        if !(p.skill.is_match(rel) || ALWAYS_ON.contains(&rel.as_str())) {
            continue;
        }
        for captured in p.script_ref.captures_iter(source) {
            let reference = &captured[1];
            if RESERVED_SCRIPTS.contains(&reference.rsplit('/').next().unwrap_or(reference)) {
                continue;
            }
            if !have.contains(reference) {
                errors.push(format!(
                    "{rel} invokes `{reference}` but that file is not in the harness"
                ));
            }
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Inventory {
    pub always_on_chars: usize,
    pub n_files: usize,
    pub skills: usize,
    pub sub_agents: usize,
    pub scripts: usize,
    pub hooks: usize,
}

/// Validate a tree, returning every problem at once.
pub fn validate(tree: &Tree) -> Result<Inventory> {
    let mut errors = Vec::new();
    let body = always_on_text(tree);
    if body.len() > ALWAYS_ON_MAX {
        errors.push(format!(
            "always-on content is {} chars, over the {ALWAYS_ON_MAX} cap; {CAP_REASON}",
            body.len()
        ));
    }
    if body.trim().is_empty() {
        errors.push(
            "AGENTS.md is empty -- it is the only always-on file on Codex, so an empty one \
is a harness that does not exist"
                .to_string(),
        );
    }
    check_skills(tree, &body, &mut errors);
    check_subagents(tree, &mut errors);
    let p = patterns();
    for (rel, source) in tree {
        if p.script.is_match(rel) || (p.skill_script.is_match(rel) && rel.ends_with(".py")) {
            python_compiles(rel, source, &mut errors);
        }
    }
    if let Some(hooks) = tree.get(HOOKS_FILE) {
        check_hooks(hooks, &mut errors);
    }
    check_script_refs(tree, &mut errors);
    if !errors.is_empty() {
        bail!("{}", errors.join(" | "));
    }
    Ok(Inventory {
        always_on_chars: body.len(),
        n_files: tree.len(),
        skills: skill_names(tree).len(),
        sub_agents: subagent_names(tree).len(),
        scripts: tree.keys().filter(|rel| p.script.is_match(rel)).count(),
        hooks: usize::from(tree.contains_key(HOOKS_FILE)),
    })
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DiffSummary {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
    pub always_on_char_delta: i64,
}

pub fn diff_summary(before: &Tree, after: &Tree) -> DiffSummary {
    DiffSummary {
        added: after
            .keys()
            .filter(|rel| !before.contains_key(*rel))
            .cloned()
            .collect(),
        removed: before
            .keys()
            .filter(|rel| !after.contains_key(*rel))
            .cloned()
            .collect(),
        modified: before
            .iter()
            .filter_map(|(rel, value)| {
                after
                    .get(rel)
                    .filter(|next| *next != value)
                    .map(|_| rel.clone())
            })
            .collect(),
        always_on_char_delta: always_on_text(after).len() as i64
            - always_on_text(before).len() as i64,
    }
}

/// A one-line census used in library entries. Reads the tree only; it never
/// runs validators (and therefore never spawns Python) while rebuilding an index.
pub fn inventory_line(tree: &Tree) -> String {
    let p = patterns();
    let skills = skill_names(tree).len();
    let sub_agents = subagent_names(tree).len();
    let scripts = tree.keys().filter(|rel| p.script.is_match(rel)).count();
    let hooks = usize::from(tree.contains_key(HOOKS_FILE));
    let mut parts = Vec::new();
    if skills > 0 {
        parts.push(format!("{skills} skills"));
    }
    if sub_agents > 0 {
        parts.push(format!("{sub_agents} sub_agents"));
    }
    if scripts > 0 {
        parts.push(format!("{scripts} scripts"));
    }
    if hooks > 0 {
        parts.push("1 hook".to_string());
    }
    if parts.is_empty() {
        parts.push("always-on edits only".to_string());
    }
    format!(
        "{}; {} always-on chars",
        parts.join(", "),
        always_on_text(tree).len()
    )
}

pub fn base_harness() -> Tree {
    let mut tree = Tree::new();
    tree.insert(
        "AGENTS.md".to_string(),
        "# Working procedure\n\n\
- Read the task and inspect the provided workspace before acting.\n\
- Work in the workspace; put every requested artifact at the path the task asks for.\n\
- Before finishing, verify every requested item is present and usable.\n"
            .to_string(),
    );
    tree
}

/// Load a harness from a directory, returning a friendly error for an empty tree.
pub fn load_harness(root: &Path) -> Result<Tree> {
    let mut rejected = Vec::new();
    let tree = read_tree(root, &mut rejected)?;
    if tree.is_empty() {
        bail!("no harness files found under {}", root.display());
    }
    Ok(tree)
}

/// The reserved directory used for runtime-owned agent role configuration.
pub fn role_file(workspace: &Path, name: &str) -> PathBuf {
    workspace
        .join(".harness2/agents")
        .join(format!("{name}.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_tree() -> Tree {
        let mut tree = Tree::new();
        tree.insert(
            "AGENTS.md".to_string(),
            "# Procedure\n\nWhen a report is requested, load the `report` skill.\n".to_string(),
        );
        tree.insert(
            ".agents/skills/report/SKILL.md".to_string(),
            "---\nname: report\ndescription: Build a report from source rows.\n---\n\nRead the rows.\n"
                .to_string(),
        );
        tree
    }

    #[test]
    fn validate_rejects_untriggered_skill_and_autotrigger_repairs_it() {
        let mut tree = skill_tree();
        tree.insert(
            "AGENTS.md".to_string(),
            "# Procedure\n\nDo the work carefully.\n".to_string(),
        );
        let error = validate(&tree).unwrap_err().to_string();
        assert!(error.contains("never named in AGENTS.md"), "{error}");
        let repaired = autotrigger(&mut tree);
        assert_eq!(repaired, vec!["report"]);
        assert!(tree["AGENTS.md"].contains("report"));
        assert!(validate(&tree).is_ok());
    }

    #[test]
    fn read_and_write_round_trip_only_the_editable_surface() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".agents/skills/report")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("AGENTS.md"), "# Procedure\n").unwrap();
        fs::write(
            root.join(".agents/skills/report/SKILL.md"),
            "---\ndescription: d\n---\n",
        )
        .unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("scripts/README.md"), "stub\n").unwrap();
        fs::write(root.join("notes.txt"), "off surface\n").unwrap();

        let mut rejected = Vec::new();
        let tree = read_tree(root, &mut rejected).unwrap();
        assert!(tree.contains_key("AGENTS.md"));
        assert!(tree.contains_key(".agents/skills/report/SKILL.md"));
        assert!(!tree.contains_key("scripts/README.md"));
        assert!(!tree.contains_key("notes.txt"));
        assert_eq!(rejected, vec!["notes.txt", "src/lib.rs"]);

        let mut next = tree.clone();
        next.remove(".agents/skills/report/SKILL.md");
        next.insert("AGENTS.md".to_string(), "# Changed\n".to_string());
        write_tree(root, &next).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            "# Changed\n"
        );
        assert!(!root.join(".agents/skills/report/SKILL.md").exists());
        assert!(root.join("src/lib.rs").exists());
        assert!(root.join("scripts/README.md").exists());
    }

    #[test]
    fn path_escapes_are_rejected() {
        assert!(normalize_rel("../secret").is_err());
        assert!(normalize_rel("/etc/passwd").is_err());
        assert_eq!(normalize_rel("./scripts/x.py").unwrap(), "scripts/x.py");
    }

    #[test]
    fn diff_summary_reports_component_changes() {
        let before = base_harness();
        let mut after = before.clone();
        after.insert("scripts/check.py".to_string(), "print('ok')\n".to_string());
        after.remove("AGENTS.md");
        let diff = diff_summary(&before, &after);
        assert_eq!(diff.added, vec!["scripts/check.py"]);
        assert_eq!(diff.removed, vec!["AGENTS.md"]);
        assert!(diff.always_on_char_delta < 0);
    }
}
