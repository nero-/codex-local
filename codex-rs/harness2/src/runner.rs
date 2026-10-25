//! Running Codex rollouts and editor sessions, and observing what they produced.
//!
//! Every model interaction is a `codex exec --json` child process. Rollouts run in an
//! isolated copy of the task workspace with one harness installed at its root, so
//! parallel probes cannot observe or overwrite one another.

use crate::surface;
use crate::surface::Tree;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use walkdir::WalkDir;

/// Directories never copied into a rollout or served as evidence.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "venv",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".cache",
    ".harness2",
    "__pycache__",
    "runs",
];

/// Files larger than this are hashed by size only and never served as evidence.
const MAX_EVIDENCE_FILE: u64 = 1_048_576;
const MAX_HASH_FILE: u64 = 16_777_216;

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub codex_bin: PathBuf,
    pub codex_home: Option<PathBuf>,
    pub solver_model: Option<String>,
    pub editor_model: Option<String>,
    pub reasoning_effort: String,
    pub timeout: Duration,
}

impl RunnerConfig {
    pub fn new() -> Self {
        Self {
            codex_bin: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codex")),
            codex_home: std::env::var_os("CODEX_HOME").map(PathBuf::from),
            solver_model: None,
            editor_model: None,
            reasoning_effort: "high".to_string(),
            timeout: Duration::from_secs(1_800),
        }
    }
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ExecResult {
    pub ok: bool,
    pub final_message: String,
    pub trajectory: String,
    pub error: String,
    pub wall_s: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct Produced {
    pub path: String,
    pub text: Option<String>,
    pub bytes: u64,
    pub deleted: bool,
    pub source: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Rollout {
    pub label: String,
    pub harness: Tree,
    pub ok: bool,
    pub delivered: bool,
    pub trajectory: String,
    pub final_message: String,
    pub produced: Vec<Produced>,
    pub error: String,
    pub wall_s: f64,
}

impl Rollout {
    pub fn failed(label: &str, harness: &Tree, error: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            harness: harness.clone(),
            ok: false,
            delivered: false,
            trajectory: String::new(),
            final_message: String::new(),
            produced: Vec::new(),
            error: error.into(),
            wall_s: 0.0,
        }
    }
}

fn should_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

fn should_skip_rel(rel: &str) -> bool {
    surface::is_allowed(rel)
        || surface::STUBS.iter().any(|(stub, _)| *stub == rel)
        || rel.starts_with(".harness2/")
}

/// Copy a workspace, skipping dependency/build directories and symlinks.
pub fn copy_workspace(source: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest).with_context(|| format!("clear {}", dest.display()))?;
    }
    fs::create_dir_all(dest)?;
    for entry in WalkDir::new(source)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                !should_skip_dir(&entry.file_name().to_string_lossy())
            } else {
                true
            }
        })
    {
        let entry = entry?;
        if entry.file_type().is_symlink() || !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(source)?;
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &target)
            .with_context(|| format!("copy {}", entry.path().display()))?;
    }
    Ok(())
}

pub type Snapshot = BTreeMap<String, String>;

pub fn snapshot(root: &Path) -> Result<Snapshot> {
    let mut out = Snapshot::new();
    for entry in WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_dir() {
                !should_skip_dir(&entry.file_name().to_string_lossy())
            } else {
                true
            }
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        if should_skip_rel(&rel) {
            continue;
        }
        let metadata = entry.metadata()?;
        let hash = if metadata.len() > MAX_HASH_FILE {
            format!("size:{}", metadata.len())
        } else {
            let bytes = fs::read(entry.path())?;
            let digest = Sha256::digest(&bytes);
            digest.iter().map(|byte| format!("{byte:02x}")).collect()
        };
        out.insert(rel, hash);
    }
    Ok(out)
}

impl Produced {
    fn from_file(root: &Path, rel: &str) -> Result<Self> {
        let source = root.join(rel);
        let bytes = fs::metadata(&source)?.len();
        let text = if bytes <= MAX_EVIDENCE_FILE {
            fs::read(&source)
                .ok()
                .and_then(|data| String::from_utf8(data).ok())
        } else {
            None
        };
        Ok(Self {
            path: rel.to_string(),
            text,
            bytes,
            deleted: false,
            source,
        })
    }

    fn deleted(rel: &str) -> Self {
        Self {
            path: rel.to_string(),
            text: None,
            bytes: 0,
            deleted: true,
            source: PathBuf::new(),
        }
    }
}

pub fn produced_since(root: &Path, before: &Snapshot) -> Result<Vec<Produced>> {
    let after = snapshot(root)?;
    let mut out = Vec::new();
    for (rel, hash) in &after {
        if before.get(rel) != Some(hash) {
            out.push(Produced::from_file(root, rel)?);
        }
    }
    for rel in before.keys() {
        if !after.contains_key(rel) {
            out.push(Produced::deleted(rel));
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

pub fn render_produced(produced: &[Produced]) -> String {
    if produced.is_empty() {
        return "_(no files were created or modified)_\n".to_string();
    }
    let mut out = String::new();
    for item in produced {
        out.push_str(&format!("## `{}`", item.path));
        if item.deleted {
            out.push_str(" _(deleted)_\n\n");
            continue;
        }
        out.push_str(&format!(" ({} bytes)\n\n", item.bytes));
        match &item.text {
            Some(text) => {
                out.push_str("```\n");
                out.push_str(text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("```\n\n");
            }
            None => out.push_str("_(binary or oversized file; contents not rendered)_\n\n"),
        }
    }
    out
}

/// Write an installed harness into a rollout workspace, plus runtime-owned agent roles.
pub fn install_harness(workspace: &Path, tree: &Tree) -> Result<Vec<String>> {
    surface::write_tree(workspace, tree)?;
    let mut overrides = Vec::new();
    for name in surface::subagent_names(tree) {
        let rel = format!("sub_agents/{name}/AGENT.md");
        let source = tree.get(&rel).cloned().unwrap_or_default();
        let role_path = surface::role_file(workspace, &name);
        if let Some(parent) = role_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let description = source
            .lines()
            .find_map(|line| line.trim().strip_prefix("description:"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(name.as_str());
        let role = format!(
            "name = \"{name}\"\ndescription = {description}\ndeveloper_instructions = {instructions}\n",
            description = toml_string(description),
            instructions = toml_string(instruction_body(&source)),
        );
        fs::write(&role_path, role)?;
        overrides.push(format!(
            "agents.{name}.description={}",
            toml_string(description)
        ));
        overrides.push(format!(
            "agents.{name}.config_file={}",
            toml_string(&role_path.to_string_lossy())
        ));
    }
    Ok(overrides)
}

fn instruction_body(source: &str) -> &str {
    let Some(rest) = source.strip_prefix("---") else {
        return source;
    };
    let Some(end) = rest.find("\n---") else {
        return source;
    };
    rest[end + 4..].trim_start_matches(['\r', '\n'])
}

fn toml_string(value: &str) -> String {
    // JSON string escaping is compatible with TOML basic strings for our values.
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// Run `codex exec` and collect its JSONL event stream.
pub async fn run_codex(
    config: &RunnerConfig,
    workspace: &Path,
    prompt: &str,
    model: Option<&str>,
    config_overrides: &[String],
) -> Result<ExecResult> {
    let mut args = vec![
        "exec".to_string(),
        prompt.to_string(),
        "--json".to_string(),
        "--skip-git-repo-check".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
        "--dangerously-bypass-hook-trust".to_string(),
        "--ephemeral".to_string(),
        "-C".to_string(),
        workspace.to_string_lossy().to_string(),
        "-c".to_string(),
        format!(
            "model_reasoning_effort={}",
            toml_string(&config.reasoning_effort)
        ),
        "-c".to_string(),
        "model_reasoning_summary=\"detailed\"".to_string(),
        "-c".to_string(),
        "show_raw_agent_reasoning=true".to_string(),
    ];
    if let Some(model) = model {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    for override_value in config_overrides {
        args.push("-c".to_string());
        args.push(override_value.clone());
    }

    let mut command = tokio::process::Command::new(&config.codex_bin);
    command
        .args(&args)
        .current_dir(workspace)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null());
    if let Some(home) = &config.codex_home {
        command.env("CODEX_HOME", home);
    }

    let started = Instant::now();
    let output = tokio::time::timeout(config.timeout, command.output()).await;
    let wall_s = started.elapsed().as_secs_f64();
    let output = match output {
        Ok(result) => result.context("spawn codex exec")?,
        Err(_) => {
            return Ok(ExecResult {
                ok: false,
                error: format!("timeout after {}s", config.timeout.as_secs()),
                wall_s,
                ..ExecResult::default()
            });
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut result = ExecResult {
        ok: output.status.success(),
        wall_s,
        ..ExecResult::default()
    };
    let mut trajectory = String::new();
    let mut last_message = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            trajectory.push_str(&format!("[STDOUT] {line}\n"));
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("item.completed") => {
                let Some(item) = event.get("item") else {
                    continue;
                };
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "agent_message" => {
                        let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                        last_message = Some(text.to_string());
                        trajectory.push_str(&format!("[ASSISTANT]\n{text}\n\n"));
                    }
                    "reasoning" => {
                        let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                        if !text.trim().is_empty() {
                            trajectory.push_str(&format!("[REASONING]\n{text}\n\n"));
                        }
                    }
                    "command_execution" => {
                        let command = item
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or("<command>");
                        let output_text = item
                            .get("aggregated_output")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let exit = item.get("exit_code").and_then(Value::as_i64);
                        trajectory.push_str(&format!("[ACTION] {command}\n"));
                        if !output_text.trim().is_empty() {
                            trajectory.push_str(&format!("[OBSERVATION]\n{output_text}\n"));
                        }
                        if let Some(exit) = exit
                            && exit != 0
                        {
                            trajectory.push_str(&format!("[ERROR] exit code {exit}\n"));
                        }
                        trajectory.push('\n');
                    }
                    "file_change" => {
                        let mut paths = Vec::new();
                        if let Some(changes) = item.get("changes").and_then(Value::as_array) {
                            for change in changes {
                                let path =
                                    change.get("path").and_then(Value::as_str).unwrap_or("?");
                                let kind = change
                                    .get("kind")
                                    .and_then(Value::as_str)
                                    .unwrap_or("update");
                                paths.push(format!("{kind} {path}"));
                            }
                        }
                        trajectory.push_str(&format!("[ACTION] patch: {}\n\n", paths.join(", ")));
                    }
                    "mcp_tool_call" => {
                        let tool = item.get("tool").and_then(Value::as_str).unwrap_or("<tool>");
                        trajectory.push_str(&format!("[ACTION] mcp {tool}\n\n"));
                    }
                    "error" => {
                        let message = item
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error");
                        // Non-fatal warning/error items are useful in the trajectory but do
                        // not mean the turn failed. `turn.failed` and the process status do.
                        trajectory.push_str(&format!("[ERROR] {message}\n\n"));
                    }
                    _ => {}
                }
            }
            Some("turn.completed") => {
                if let Some(usage) = event.get("usage") {
                    result.input_tokens = usage
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    result.output_tokens = usage
                        .get("output_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    result.reasoning_tokens = usage
                        .get("reasoning_output_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                }
            }
            Some("turn.failed") => {
                let message = event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("turn failed");
                result.ok = false;
                result.error = message.to_string();
                trajectory.push_str(&format!("[ERROR] {message}\n\n"));
            }
            Some("error") => {
                let message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("stream error");
                result.ok = false;
                result.error = message.to_string();
                trajectory.push_str(&format!("[ERROR] {message}\n\n"));
            }
            _ => {}
        }
    }
    if !stderr.trim().is_empty() {
        trajectory.push_str(&format!("[STDERR]\n{}\n", stderr.trim()));
        if !result.ok && result.error.is_empty() {
            result.error = stderr.trim().lines().next().unwrap_or("").to_string();
        }
    }
    if !output.status.success() && result.error.is_empty() {
        result.error = format!("codex exec exited with {}", output.status);
    }
    result.final_message = last_message.unwrap_or_default();
    result.trajectory = trajectory;
    if result.ok && result.final_message.trim().is_empty() {
        result.ok = false;
        result.error = "codex exec returned no agent message".to_string();
    }
    Ok(result)
}

/// Run one solver rollout with `harness` installed in an isolated copy of the workspace.
pub async fn rollout(
    config: &RunnerConfig,
    base_workspace: &Path,
    before: &Snapshot,
    task: &str,
    label: &str,
    harness: &Tree,
    rollouts_dir: &Path,
) -> Result<Rollout> {
    let workspace = rollouts_dir.join(label);
    copy_workspace(base_workspace, &workspace)?;
    let overrides = install_harness(&workspace, harness)?;
    let started = Instant::now();
    let exec = run_codex(
        config,
        &workspace,
        task,
        config.solver_model.as_deref(),
        &overrides,
    )
    .await?;
    let produced = produced_since(&workspace, before).unwrap_or_default();
    let delivered = !produced.is_empty() || !exec.final_message.trim().is_empty();
    Ok(Rollout {
        label: label.to_string(),
        harness: harness.clone(),
        ok: exec.ok,
        delivered,
        trajectory: exec.trajectory,
        final_message: exec.final_message,
        produced,
        error: exec.error,
        wall_s: started.elapsed().as_secs_f64(),
    })
}

/// Copy the selected rollout's produced files into `out`.
pub fn publish(rollout: &Rollout, out: &Path) -> Result<Vec<String>> {
    fs::create_dir_all(out)?;
    let mut written = Vec::new();
    let mut seen = BTreeSet::new();
    for item in &rollout.produced {
        if item.deleted || !seen.insert(item.path.clone()) {
            continue;
        }
        let target = out.join(&item.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if item.source.is_file() {
            fs::copy(&item.source, &target).with_context(|| format!("publish {}", item.path))?;
            written.push(item.path.clone());
        }
    }
    Ok(written)
}

/// A single rendered candidate used for blind selection.
pub fn candidate_note(rollout: &Rollout) -> String {
    let mut out = format!("# {}\n\n", rollout.label);
    if !rollout.final_message.trim().is_empty() {
        out.push_str("## Final message\n\n");
        out.push_str(&rollout.final_message);
        out.push_str("\n\n");
    }
    out.push_str("## Produced files\n\n");
    out.push_str(&render_produced(&rollout.produced));
    out
}

pub fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

pub fn ensure_nonempty(value: &str, what: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{what} is empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_skips_dependency_and_vcs_directories() {
        let source = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("src")).unwrap();
        fs::create_dir_all(source.path().join(".git")).unwrap();
        fs::create_dir_all(source.path().join("target")).unwrap();
        fs::write(source.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(source.path().join(".git/HEAD"), "ref\n").unwrap();
        fs::write(source.path().join("target/artifact"), "x\n").unwrap();
        copy_workspace(source.path(), dest.path()).unwrap();
        assert!(dest.path().join("src/main.rs").is_file());
        assert!(!dest.path().join(".git").exists());
        assert!(!dest.path().join("target").exists());
    }

    #[test]
    fn produced_since_observes_worker_changes_and_ignores_harness_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.txt"), "one\n").unwrap();
        fs::write(root.path().join("AGENTS.md"), "# base\n").unwrap();
        let before = snapshot(root.path()).unwrap();

        fs::write(root.path().join("a.txt"), "two\n").unwrap();
        fs::write(root.path().join("b.txt"), "new\n").unwrap();
        fs::write(root.path().join("AGENTS.md"), "# replaced\n").unwrap();
        let produced = produced_since(root.path(), &before).unwrap();
        let paths = produced
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);
        assert_eq!(produced[0].text.as_deref(), Some("two\n"));
        assert!(render_produced(&produced).contains("b.txt"));
    }
}
