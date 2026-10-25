//! Parallel and sequential task-level recursion, plus the post-task library update.
//!
//! This is a native port of the Harness² improvement step for the Codex substrate:
//! reference rollouts under the current harness, invariant and domain proposals, probes
//! of those proposals, selective composition, blind final selection, and reflection into
//! a per-domain experience library.

use crate::library::Experience;
use crate::library::Library;
use crate::library::parse_choice;
use crate::library::parse_reflection;
use crate::prompts;
use crate::runner;
use crate::runner::Rollout;
use crate::runner::RunnerConfig;
use crate::runner::Snapshot;
use crate::surface;
use crate::surface::Tree;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use futures::future::join_all;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Parallel,
    Sequential,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "parallel" | "agg" => Ok(Self::Parallel),
            "sequential" | "seq" => Ok(Self::Sequential),
            other => bail!("unknown mode {other:?}; expected `parallel` or `sequential`"),
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Parallel => "agg",
            Self::Sequential => "seq",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub task: String,
    pub task_id: String,
    pub workspace: PathBuf,
    pub base_harness: Tree,
    pub run_dir: PathBuf,
    pub library: Library,
    pub k: usize,
    pub mode: Mode,
    pub concurrency: usize,
    pub runner: RunnerConfig,
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageReport {
    pub label: String,
    pub dim: String,
    pub kept_base: bool,
    pub fell_back: bool,
    pub attempts: usize,
    pub wall_s: f64,
    pub error: String,
    pub notes: String,
    pub diff: surface::DiffSummary,
}

#[derive(Debug, Clone)]
pub struct Stage {
    pub label: String,
    pub dim: String,
    pub tree: Tree,
    pub notes: String,
    pub error: String,
    pub attempts: usize,
    pub wall_s: f64,
}

impl Stage {
    fn fallback(label: &str, dim: &str, tree: &Tree, error: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            dim: dim.to_string(),
            tree: tree.clone(),
            notes: String::new(),
            error: error.into(),
            attempts: 0,
            wall_s: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub run_id: String,
    pub mode: Mode,
    pub k: usize,
    pub task_id: String,
    pub selected: String,
    pub adopted_harness: PathBuf,
    pub output_files: Vec<String>,
    pub finals: Vec<String>,
    pub stages: Vec<StageReport>,
    pub library_entry: Option<PathBuf>,
    pub work_dir: PathBuf,
}

#[derive(Clone)]
struct Plan {
    label: String,
    dim: &'static str,
    job: String,
    fallback: Tree,
    harness: Tree,
    rollouts: Vec<Rollout>,
    pool: Vec<(String, Tree, Option<Rollout>)>,
    decisions: Vec<(String, String)>,
}

struct Selection {
    label: String,
    tree: Tree,
    rollout: Rollout,
}

pub async fn run(opts: &RunOptions) -> Result<RunReport> {
    if opts.k == 0 {
        bail!("--k must be at least 1");
    }
    fs::create_dir_all(&opts.run_dir)?;
    let work = opts.run_dir.join("work");
    fs::create_dir_all(&work)?;
    let rollouts_dir = work.join("rollouts");
    fs::create_dir_all(&rollouts_dir)?;
    let base_workspace = opts.workspace.clone();
    let before = runner::snapshot(&base_workspace)?;

    println!(
        "harness2: task {:?} mode={} k={} workspace={}",
        opts.task_id,
        opts.mode.token(),
        opts.k,
        base_workspace.display()
    );

    let base_labels = (1..=opts.k).map(|i| format!("base{i}")).collect::<Vec<_>>();
    let base_trees = base_labels
        .iter()
        .map(|label| (label.clone(), opts.base_harness.clone()))
        .collect::<Vec<_>>();
    let base = roll_many(opts, &base_workspace, &before, base_trees, &rollouts_dir).await;
    let base_ok = base.iter().filter(|rollout| rollout.ok).count();
    if base_ok == 0 {
        bail!(
            "all {} reference rollouts failed; no execution evidence exists. First error: {}",
            base.len(),
            base.first()
                .map(|rollout| rollout.error.as_str())
                .unwrap_or("unknown")
        );
    }
    archive_rollouts(&opts.run_dir, &base)?;

    let (stages, finals, selected, all_rollouts) = match opts.mode {
        Mode::Parallel => run_parallel(opts, &work, &base_workspace, &before, base).await?,
        Mode::Sequential => run_sequential(opts, &work, &base_workspace, &before, base).await?,
    };

    let library_dir = opts.library.context_dir();
    let library_entry = reflect_and_store(
        opts,
        &work,
        &base_workspace,
        &stages,
        &all_rollouts,
        &selected,
        library_dir.as_deref(),
    )
    .await;
    let output_files = runner::publish(&selected.rollout, &opts.out_dir)?;
    let adopted_path = work.join("adopted");
    surface::write_tree(&adopted_path, &selected.tree)?;

    let mut reports = Vec::new();
    for stage in &stages {
        let diff = surface::diff_summary(&opts.base_harness, &stage.tree);
        let kept_base = diff.added.is_empty()
            && diff.removed.is_empty()
            && diff.modified.is_empty()
            && diff.always_on_char_delta == 0;
        reports.push(StageReport {
            label: stage.label.clone(),
            dim: stage.dim.clone(),
            kept_base,
            fell_back: !stage.error.is_empty(),
            attempts: stage.attempts,
            wall_s: stage.wall_s,
            error: stage.error.clone(),
            notes: stage.notes.clone(),
            diff,
        });
    }
    let report = RunReport {
        run_id: opts
            .run_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "run".to_string()),
        mode: opts.mode,
        k: opts.k,
        task_id: opts.task_id.clone(),
        selected: selected.label.clone(),
        adopted_harness: adopted_path,
        output_files,
        finals,
        stages: reports,
        library_entry,
        work_dir: work.clone(),
    };
    runner::write_text(
        &opts.run_dir.join("result.json"),
        &serde_json::to_string_pretty(&report)?,
    )?;
    Ok(report)
}

async fn run_parallel(
    opts: &RunOptions,
    work: &Path,
    base_workspace: &Path,
    before: &Snapshot,
    base: Vec<Rollout>,
) -> Result<(Vec<Stage>, Vec<String>, Selection, Vec<Rollout>)> {
    let mut plans = Vec::new();
    for index in 1..=opts.k {
        for dim in ["INV", "DOM"] {
            plans.push(Plan {
                label: format!("{}{}", dim.to_ascii_lowercase(), index),
                dim,
                job: prompts::propose(dim, opts.k, index, None),
                fallback: opts.base_harness.clone(),
                harness: opts.base_harness.clone(),
                rollouts: base.clone(),
                pool: Vec::new(),
                decisions: Vec::new(),
            });
        }
    }
    let mut stages = run_plans(opts, work, plans).await;
    let rollouts = roll_many(
        opts,
        base_workspace,
        before,
        stages
            .iter()
            .map(|stage| (stage.label.clone(), stage.tree.clone()))
            .collect(),
        &work.join("rollouts"),
    )
    .await;
    archive_rollouts(&opts.run_dir, &rollouts)?;
    let pool = pairs(&stages, &rollouts);
    let decisions = stages
        .iter()
        .map(|stage| {
            (
                stage.label.clone(),
                if stage.error.is_empty() {
                    stage.notes.clone()
                } else {
                    stage.error.clone()
                },
            )
        })
        .collect::<Vec<_>>();

    let mut agg_plans = Vec::new();
    for index in 1..=opts.k {
        agg_plans.push(Plan {
            label: format!("agg{index}"),
            dim: "AGG",
            job: prompts::aggregate(opts.k, index),
            fallback: opts.base_harness.clone(),
            harness: opts.base_harness.clone(),
            rollouts: base.clone(),
            pool: pool.clone(),
            decisions: decisions.clone(),
        });
    }
    let aggs = run_plans(opts, work, agg_plans).await;
    let agg_rollouts = roll_many(
        opts,
        base_workspace,
        before,
        aggs.iter()
            .map(|stage| (stage.label.clone(), stage.tree.clone()))
            .collect(),
        &work.join("rollouts"),
    )
    .await;
    archive_rollouts(&opts.run_dir, &agg_rollouts)?;
    stages.extend(aggs.iter().cloned());
    let finals = aggs
        .iter()
        .map(|stage| stage.label.clone())
        .collect::<Vec<_>>();
    let selected = if opts.k == 1 {
        final_selection(&finals[0], &aggs, &agg_rollouts, &opts.base_harness)?
    } else {
        let candidates = base
            .iter()
            .cloned()
            .chain(agg_rollouts.iter().cloned())
            .collect::<Vec<_>>();
        select(
            opts,
            work,
            &base_workspace,
            &candidates,
            &aggs,
            &agg_rollouts,
        )
        .await?
    };
    let mut all_rollouts = base.clone();
    all_rollouts.extend(rollouts.clone());
    all_rollouts.extend(agg_rollouts.clone());
    Ok((stages, finals, selected, all_rollouts))
}

async fn run_sequential(
    opts: &RunOptions,
    work: &Path,
    base_workspace: &Path,
    before: &Snapshot,
    base: Vec<Rollout>,
) -> Result<(Vec<Stage>, Vec<String>, Selection, Vec<Rollout>)> {
    let mut stages = Vec::new();
    let mut finals = Vec::new();
    let mut current = opts.base_harness.clone();
    let mut history: Vec<(String, Tree, Option<Rollout>)> = Vec::new();
    let mut checkpoint_rollouts: Vec<Rollout> = Vec::new();
    let mut all_rollouts = base.clone();

    for round in 1..=opts.k {
        let mut plans = Vec::new();
        for dim in ["INV", "DOM"] {
            plans.push(Plan {
                label: format!("{}{round}", dim.to_ascii_lowercase()),
                dim,
                job: prompts::propose(dim, opts.k, round, Some(round)),
                fallback: current.clone(),
                harness: current.clone(),
                rollouts: base.clone(),
                pool: history.clone(),
                decisions: stage_decisions(&stages),
            });
        }
        let round_stages = run_plans(opts, work, plans).await;
        let round_rollouts = roll_many(
            opts,
            base_workspace,
            before,
            round_stages
                .iter()
                .map(|stage| (stage.label.clone(), stage.tree.clone()))
                .collect(),
            &work.join("rollouts"),
        )
        .await;
        archive_rollouts(&opts.run_dir, &round_rollouts)?;
        all_rollouts.extend(round_rollouts.iter().cloned());
        let mut round_pool = history.clone();
        round_pool.extend(pairs(&round_stages, &round_rollouts));
        let mut decisions = stage_decisions(&stages);
        decisions.extend(stage_decisions(&round_stages));

        let checkpoint_label = format!("c{round}");
        let comp_job = if opts.k == 1 {
            prompts::aggregate(1, 1)
        } else {
            prompts::compose_round(round, opts.k)
        };
        let comp_plan = Plan {
            label: checkpoint_label.clone(),
            dim: "COMPOSE",
            job: comp_job,
            fallback: current.clone(),
            harness: current.clone(),
            rollouts: base.clone(),
            pool: round_pool,
            decisions,
        };
        let mut comp = run_plans(opts, work, vec![comp_plan]).await;
        let mut checkpoint = comp.pop().expect("one composition");
        let checkpoint_rollout = roll_many(
            opts,
            base_workspace,
            before,
            vec![(checkpoint.label.clone(), checkpoint.tree.clone())],
            &work.join("rollouts"),
        )
        .await
        .into_iter()
        .next()
        .expect("one checkpoint rollout");
        archive_rollouts(&opts.run_dir, std::slice::from_ref(&checkpoint_rollout))?;
        if checkpoint.error.is_empty() {
            checkpoint.error = checkpoint_rollout.error.clone();
        }
        stages.extend(round_stages.iter().cloned());
        stages.push(checkpoint.clone());
        checkpoint_rollouts.push(checkpoint_rollout.clone());
        all_rollouts.push(checkpoint_rollout.clone());
        let mut next_history = pairs(&[checkpoint.clone()], &[checkpoint_rollout]);
        next_history.extend(pairs(&round_stages, &round_rollouts));
        next_history.extend(history);
        history = next_history;
        current = checkpoint.tree.clone();
        finals.push(checkpoint_label);
    }

    let selected = if opts.k == 1 {
        final_selection(
            &finals[0],
            &stages,
            &checkpoint_rollouts,
            &opts.base_harness,
        )?
    } else {
        let mut candidates = base.clone();
        candidates.extend(checkpoint_rollouts.clone());
        select(
            opts,
            work,
            base_workspace,
            &candidates,
            &stages,
            &checkpoint_rollouts,
        )
        .await?
    };
    Ok((stages, finals, selected, all_rollouts))
}

async fn run_plans(opts: &RunOptions, work: &Path, plans: Vec<Plan>) -> Vec<Stage> {
    let mut out = Vec::new();
    for chunk in plans.chunks(opts.concurrency.max(1)) {
        let futures = chunk.iter().map(|plan| propose_stage(opts, work, plan));
        out.extend(join_all(futures).await);
    }
    out
}

async fn propose_stage(opts: &RunOptions, work: &Path, plan: &Plan) -> Stage {
    let started = std::time::Instant::now();
    let mut last_error = String::new();
    for attempt in 1..=2 {
        let dest = work
            .join("proposals")
            .join(format!("{}.a{attempt}", plan.label));
        let library_dir = opts.library.context_dir();
        let evidence = EvidenceInput {
            task: &opts.task,
            source: &opts.workspace,
            harness: &plan.harness,
            library: library_dir.as_deref(),
            rollouts: &plan.rollouts,
            pool: &plan.pool,
            decisions: &plan.decisions,
            base_harness: None,
            candidates: &[],
        };
        if let Err(err) = build_evidence(&dest, &evidence) {
            last_error = format!("could not build evidence: {err:#}");
            continue;
        }
        let layout = evidence_layout(&dest, &evidence);
        let mut prompt = format!("{}\n\n{}", plan.job, mission(&layout));
        if !last_error.is_empty() {
            prompt.push_str(&format!(
                "\n\n# YOUR PREVIOUS ATTEMPT WAS REJECTED\n\n{last_error}\n\n\
Fix it in `context/harness/` and reply again."
            ));
        }
        let exec = runner::run_codex(
            &opts.runner,
            &dest,
            &prompt,
            opts.runner.editor_model.as_deref(),
            &[],
        )
        .await;
        let exec = match exec {
            Ok(exec) => exec,
            Err(err) => {
                last_error = format!("editor session failed: {err:#}");
                continue;
            }
        };
        if !exec.ok {
            last_error = if exec.error.is_empty() {
                "editor session did not complete".to_string()
            } else {
                exec.error.clone()
            };
            continue;
        }
        let mut rejected = Vec::new();
        let mut tree = match surface::read_tree(&dest.join("context/harness"), &mut rejected) {
            Ok(tree) => tree,
            Err(err) => {
                last_error = format!("could not read edited harness: {err:#}");
                continue;
            }
        };
        if !rejected.is_empty() {
            last_error = format!(
                "these files are off the editable surface and were not applied: {}. \
Editable paths are AGENTS.md, .agents/skills/<name>/SKILL.md, \
.agents/skills/<name>/scripts/*, sub_agents/<name>/AGENT.md, scripts/<name>.py, and \
.codex/hooks.json.",
                rejected.join(", ")
            );
            continue;
        }
        if tree.is_empty() {
            last_error =
                "`context/harness/` came back empty -- edit the files, do not delete them all"
                    .to_string();
            continue;
        }
        surface::autotrigger(&mut tree);
        match surface::validate(&tree) {
            Ok(_) => {
                return Stage {
                    label: plan.label.clone(),
                    dim: plan.dim.to_string(),
                    tree,
                    notes: extract_notes(&exec.final_message),
                    error: String::new(),
                    attempts: attempt,
                    wall_s: started.elapsed().as_secs_f64(),
                };
            }
            Err(err) => {
                last_error = format!("{err:#}");
            }
        }
    }
    Stage::fallback(&plan.label, plan.dim, &plan.fallback, last_error)
}

fn stage_decisions(stages: &[Stage]) -> Vec<(String, String)> {
    stages
        .iter()
        .map(|stage| {
            (
                stage.label.clone(),
                if stage.error.is_empty() {
                    stage.notes.clone()
                } else {
                    stage.error.clone()
                },
            )
        })
        .collect()
}

fn pairs(stages: &[Stage], rollouts: &[Rollout]) -> Vec<(String, Tree, Option<Rollout>)> {
    stages
        .iter()
        .map(|stage| {
            let rollout = rollouts
                .iter()
                .find(|rollout| rollout.label == stage.label)
                .cloned();
            (stage.label.clone(), stage.tree.clone(), rollout)
        })
        .collect()
}

async fn roll_many(
    opts: &RunOptions,
    base_workspace: &Path,
    before: &Snapshot,
    harnesses: Vec<(String, Tree)>,
    rollouts_dir: &Path,
) -> Vec<Rollout> {
    let mut out = Vec::new();
    for chunk in harnesses.chunks(opts.concurrency.max(1)) {
        let futures = chunk.iter().map(|(label, tree)| {
            runner::rollout(
                &opts.runner,
                base_workspace,
                before,
                &opts.task,
                label,
                tree,
                rollouts_dir,
            )
        });
        for (item, result) in chunk.iter().zip(join_all(futures).await) {
            out.push(match result {
                Ok(rollout) => rollout,
                Err(err) => Rollout::failed(&item.0, &item.1, format!("{err:#}")),
            });
        }
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

fn archive_rollouts(run_dir: &Path, rollouts: &[Rollout]) -> Result<()> {
    for rollout in rollouts {
        let path = run_dir
            .join("rollouts")
            .join(format!("{}.md", rollout.label));
        let mut text = format!("# {}\n\n", rollout.label);
        if !rollout.error.is_empty() {
            text.push_str(&format!("**error:** {}\n\n", rollout.error));
        }
        if !rollout.final_message.trim().is_empty() {
            text.push_str("## Final message\n\n");
            text.push_str(&rollout.final_message);
            text.push_str("\n\n");
        }
        text.push_str("## Produced files\n\n");
        text.push_str(&runner::render_produced(&rollout.produced));
        text.push_str("\n## Trajectory\n\n");
        text.push_str(&rollout.trajectory);
        runner::write_text(&path, &text)?;
    }
    Ok(())
}

struct EvidenceInput<'a> {
    task: &'a str,
    source: &'a Path,
    harness: &'a Tree,
    library: Option<&'a Path>,
    rollouts: &'a [Rollout],
    pool: &'a [(String, Tree, Option<Rollout>)],
    decisions: &'a [(String, String)],
    base_harness: Option<&'a Tree>,
    candidates: &'a [(String, Rollout)],
}

fn build_evidence(dest: &Path, input: &EvidenceInput<'_>) -> Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::create_dir_all(dest)?;
    runner::write_text(&dest.join("context/task.md"), input.task)?;
    runner::copy_workspace(input.source, &dest.join("context/source"))?;
    surface::write_tree(&dest.join("context/harness"), input.harness)?;
    if let Some(base) = input.base_harness {
        surface::write_tree(&dest.join("context/base_harness"), base)?;
    }

    for rollout in input.rollouts {
        write_rollout_evidence(&dest.join("context"), rollout)?;
    }
    for (label, notes) in input.decisions {
        runner::write_text(
            &dest.join("context/decisions").join(format!("{label}.md")),
            if notes.trim().is_empty() {
                "No decision notes returned."
            } else {
                notes
            },
        )?;
    }
    for (label, tree, rollout) in input.pool {
        let dir = dest.join("context/pool").join(label);
        surface::write_tree(&dir.join("harness"), tree)?;
        if let Some(rollout) = rollout {
            runner::write_text(&dir.join("rollout.md"), &rollout_evidence_text(rollout))?;
            for item in &rollout.produced {
                if let Some(content) = &item.text {
                    runner::write_text(&dir.join("produced").join(&item.path), content)?;
                }
            }
        }
    }
    if let Some(library) = input.library
        && library.is_dir()
    {
        runner::copy_workspace(library, &dest.join("context/library"))?;
    }
    for (label, rollout) in input.candidates {
        runner::write_text(
            &dest.join("context/candidates").join(format!("{label}.md")),
            &runner::candidate_note(rollout),
        )?;
    }
    Ok(())
}

fn rollout_evidence_text(rollout: &Rollout) -> String {
    let mut text = String::new();
    if !rollout.error.is_empty() {
        text.push_str(&format!("**error:** {}\n\n", rollout.error));
    }
    if !rollout.final_message.trim().is_empty() {
        text.push_str("## Final message\n\n");
        text.push_str(&rollout.final_message);
        text.push_str("\n\n");
    }
    text.push_str("## Produced files\n\n");
    text.push_str(&runner::render_produced(&rollout.produced));
    text.push_str("\n## Trajectory\n\n");
    text.push_str(&rollout.trajectory);
    text
}

fn write_rollout_evidence(context: &Path, rollout: &Rollout) -> Result<()> {
    runner::write_text(
        &context
            .join("rollouts")
            .join(format!("{}.md", rollout.label)),
        &rollout_evidence_text(rollout),
    )?;
    for item in &rollout.produced {
        if item.deleted {
            runner::write_text(
                &context
                    .join("deliverables")
                    .join(&rollout.label)
                    .join(format!("{}.deleted", item.path)),
                "(deleted)\n",
            )?;
            continue;
        }
        if let Some(content) = &item.text {
            runner::write_text(
                &context
                    .join("deliverables")
                    .join(&rollout.label)
                    .join(&item.path),
                content,
            )?;
        }
    }
    Ok(())
}

fn evidence_layout(dest: &Path, input: &EvidenceInput<'_>) -> String {
    let mut lines =
        vec!["context/task.md                  the task as the agent received it".to_string()];
    if dest.join("context/source").is_dir() {
        lines.push("context/source/                 task source materials".to_string());
    }
    if dest.join("context/decisions").is_dir() {
        lines.push(
            "context/decisions/              earlier edits and composition decisions".to_string(),
        );
    }
    if !input.rollouts.is_empty() {
        lines.push(format!(
            "context/rollouts/               FULL trajectories: {}",
            input
                .rollouts
                .iter()
                .map(|rollout| format!("{}.md", rollout.label))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        lines.push(
            "context/deliverables/<label>/    the files each rollout produced, one per artifact"
                .to_string(),
        );
    }
    lines.push(format!(
        "context/harness/                the harness you are editing ({} files)",
        input.harness.len()
    ));
    if !input.pool.is_empty() {
        lines.push(format!(
            "context/pool/                   {} candidates, each with its harness and rollout",
            input.pool.len()
        ));
    }
    if dest.join("context/library").is_dir() {
        lines.push(
            "context/library/                earlier tasks in this area -- browse INDEX.md first"
                .to_string(),
        );
    }
    if !input.candidates.is_empty() {
        lines.push(format!(
            "context/candidates/             {} anonymous artifacts to compare",
            input.candidates.len()
        ));
    }
    lines.join("\n")
}

fn mission(layout: &str) -> String {
    format!(
        "# YOUR EVIDENCE\n\n\
It is on disk, in `context/`. Read it -- do not answer from this message alone.\n\n\
{layout}\n\n\
Every trajectory EVENT is present: the trajectory was not clipped to fit this mission. An \
individual observation may contain an explicit renderer-truncation marker; that marker is ours, \
not behavior by the solver. Do not diagnose it as a solver failure.\n\n\
Read `task.md` and the current harness first. Inventory every available rollout or candidate and \
inspect each at least once before deciding. For a candidate, compare what changed from its \
editing baseline, what behavior changed in its rollout, what improved, and what regressed or \
remained unresolved. Then deep-read the evidence relevant to any edit. Cite the specific \
trajectory moment behind every change; one you cannot trace to a moment is speculation."
    )
}

fn extract_notes(reply: &str) -> String {
    if let Some(start) = reply.find("<notes>")
        && let Some(end) = reply[start..].find("</notes>")
    {
        return reply[start + "<notes>".len()..start + end]
            .trim()
            .to_string();
    }
    reply.trim().to_string()
}

fn final_selection(
    label: &str,
    stages: &[Stage],
    rollouts: &[Rollout],
    base: &Tree,
) -> Result<Selection> {
    let rollout = rollouts
        .iter()
        .find(|rollout| rollout.label == label)
        .cloned()
        .with_context(|| format!("missing final rollout {label}"))?;
    let tree = stages
        .iter()
        .find(|stage| stage.label == label)
        .map(|stage| stage.tree.clone())
        .unwrap_or_else(|| base.clone());
    Ok(Selection {
        label: label.to_string(),
        tree,
        rollout,
    })
}

async fn select(
    opts: &RunOptions,
    work: &Path,
    base_workspace: &Path,
    candidates: &[Rollout],
    stages: &[Stage],
    finals: &[Rollout],
) -> Result<Selection> {
    if candidates.is_empty() {
        bail!("no candidate executions were produced");
    }
    // Anonymise the pool. Base runs and adapted runs are deliberately indistinguishable.
    let mut ordered = candidates.to_vec();
    ordered.rotate_left(candidates.len().saturating_sub(1));
    let anon = ordered
        .iter()
        .enumerate()
        .map(|(index, rollout)| (format!("cand{:02}", index + 1), rollout.clone()))
        .collect::<Vec<_>>();
    let by_anon = anon
        .iter()
        .map(|(id, rollout)| (id.clone(), rollout.clone()))
        .collect::<BTreeMap<_, _>>();
    let dest = work.join("selection");
    let empty = EvidenceInput {
        task: &opts.task,
        source: base_workspace,
        harness: &opts.base_harness,
        library: None,
        rollouts: &[],
        pool: &[],
        decisions: &[],
        base_harness: None,
        candidates: &anon,
    };
    build_evidence(&dest, &empty)?;
    let layout = evidence_layout(&dest, &empty);
    let prompt = format!("{}\n\n{}", prompts::select(anon.len()), mission(&layout));
    let exec = runner::run_codex(
        &opts.runner,
        &dest,
        &prompt,
        opts.runner.editor_model.as_deref(),
        &[],
    )
    .await?;
    let chosen = parse_choice(&exec.final_message)
        .and_then(|id| by_anon.get(&id).cloned())
        .or_else(|| finals.last().cloned())
        .or_else(|| candidates.last().cloned())
        .context("selector returned no usable candidate")?;

    // Find the harness that produced the chosen rollout.
    let tree = stages
        .iter()
        .find(|stage| stage.label == chosen.label)
        .map(|stage| stage.tree.clone())
        .or_else(|| {
            if chosen.label.starts_with("base") {
                Some(opts.base_harness.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| opts.base_harness.clone());
    Ok(Selection {
        label: chosen.label.clone(),
        tree,
        rollout: chosen,
    })
}

async fn reflect_and_store(
    opts: &RunOptions,
    work: &Path,
    base_workspace: &Path,
    stages: &[Stage],
    all_rollouts: &[Rollout],
    selected: &Selection,
    library_dir: Option<&Path>,
) -> Option<PathBuf> {
    let dest = work.join("reflection");
    let pool = stages
        .iter()
        .map(|stage| {
            let rollout = all_rollouts
                .iter()
                .find(|rollout| rollout.label == stage.label)
                .cloned();
            (stage.label.clone(), stage.tree.clone(), rollout)
        })
        .collect::<Vec<_>>();
    let input = EvidenceInput {
        task: &opts.task,
        source: base_workspace,
        harness: &selected.tree,
        library: library_dir,
        rollouts: all_rollouts,
        pool: &pool,
        decisions: &stage_decisions(stages),
        base_harness: Some(&opts.base_harness),
        candidates: &[],
    };
    if let Err(err) = build_evidence(&dest, &input) {
        eprintln!("harness2: reflection evidence failed: {err:#}");
        return None;
    }
    let layout = evidence_layout(&dest, &input);
    let prompt = format!("{}\n\n{}", prompts::experience(), mission(&layout));
    let exec = match runner::run_codex(
        &opts.runner,
        &dest,
        &prompt,
        opts.runner.editor_model.as_deref(),
        &[],
    )
    .await
    {
        Ok(exec) => exec,
        Err(err) => {
            eprintln!("harness2: reflection session failed: {err:#}");
            return None;
        }
    };
    if !exec.ok {
        eprintln!(
            "harness2: reflection session did not complete: {}",
            exec.error
        );
        return None;
    }
    let experience: Experience = parse_reflection(&exec.final_message);
    let why = format!(
        "Blind selection chose `{}` after {} executed candidate(s).",
        selected.label,
        all_rollouts.len()
    );
    match opts.library.add(
        &opts.task_id,
        &selected.tree,
        &selected.label,
        &opts
            .run_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        &why,
        &experience,
    ) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("harness2: could not update library: {err:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_codex(path: &Path) {
        let script = r#"#!/usr/bin/env python3
import json, os, pathlib, sys

workspace = pathlib.Path(os.getcwd())
args = sys.argv[1:]
prompt = ""
if len(args) > 1 and args[0] == "exec":
    prompt = args[1]

def emit(value):
    print(json.dumps(value), flush=True)

emit({"type": "thread.started", "thread_id": "t"})
emit({"type": "turn.started"})

def message(text):
    emit({"type": "item.completed", "item": {"id": "m", "type": "agent_message", "text": text}})
    emit({"type": "turn.completed", "usage": {"input_tokens": 1, "cached_input_tokens": 0, "output_tokens": 1, "reasoning_output_tokens": 0}})

harness = workspace / "context" / "harness"
if harness.is_dir():
    if "YOUR DIMENSION" in prompt:
        agents = harness / "AGENTS.md"
        text = agents.read_text() if agents.exists() else ""
        agents.write_text(text + "\n- Verify every value twice before finishing.\n")
        message("<notes>added a verification rule</notes>")
    elif "complete record of one improvement round" in prompt:
        message("<edits>\ncomponent: AGENTS.md\ncandidates: inv1\nchange: added verification\nobserved: base1 wrote without checking\nverdict: helped\n</edits>\n"
                "<experience>\ncue: when a value must be exact\nlesson: add a verification rule to AGENTS.md\nevidence: inv1 verified the value\nconfidence: medium\n</experience>")
    elif "choosing which of" in prompt:
        candidates = sorted((workspace / "context" / "candidates").glob("*.md"))
        choice = candidates[0].stem if candidates else "cand01"
        message("<why>\nclosest\n</why>\n```choice\n" + choice + "\n```")
    else:
        pools = sorted((workspace / "context" / "pool").glob("*/harness/AGENTS.md"))
        if pools:
            (harness / "AGENTS.md").write_text(pools[0].read_text())
        message("<notes>merged the verified candidate</notes>")
else:
    (workspace / "answer.txt").write_text("391")
    message("DONE")
"#;
        fs::write(path, script).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    #[tokio::test]
    async fn parallel_run_proposes_probes_selects_and_reflects() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("source.txt"), "17*23\n").unwrap();
        let fake = temp.path().join("codex-fake");
        fake_codex(&fake);

        let mut runner = RunnerConfig::new();
        runner.codex_bin = fake;
        runner.timeout = std::time::Duration::from_secs(30);
        let options = RunOptions {
            task: "Create answer.txt containing 391.".to_string(),
            task_id: "task-test".to_string(),
            workspace: workspace.clone(),
            base_harness: surface::base_harness(),
            run_dir: temp.path().join("run"),
            library: Library::new(temp.path().join("library"), "general"),
            k: 1,
            mode: Mode::Parallel,
            concurrency: 2,
            runner,
            out_dir: temp.path().join("out"),
        };
        let report = run(&options).await.unwrap();
        assert_eq!(report.selected, "agg1");
        assert_eq!(
            fs::read_to_string(options.out_dir.join("answer.txt")).unwrap(),
            "391"
        );
        let adopted = fs::read_to_string(report.adopted_harness.join("AGENTS.md")).unwrap();
        assert!(adopted.contains("Verify every value twice"));
        let entry = report.library_entry.expect("library entry");
        let experience = fs::read_to_string(entry.join("EXPERIENCE.md")).unwrap();
        assert!(experience.contains("when a value must be exact"));
    }

    #[tokio::test]
    async fn sequential_run_chains_checkpoints() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let fake = temp.path().join("codex-fake");
        fake_codex(&fake);
        let mut runner = RunnerConfig::new();
        runner.codex_bin = fake;
        runner.timeout = std::time::Duration::from_secs(30);
        let options = RunOptions {
            task: "Create answer.txt containing 391.".to_string(),
            task_id: "task-seq".to_string(),
            workspace,
            base_harness: surface::base_harness(),
            run_dir: temp.path().join("run"),
            library: Library::new(temp.path().join("library"), "general"),
            k: 2,
            mode: Mode::Sequential,
            concurrency: 2,
            runner,
            out_dir: temp.path().join("out"),
        };
        let report = run(&options).await.unwrap();
        assert_eq!(report.finals, vec!["c1", "c2"]);
        assert!(report.stages.iter().any(|stage| stage.label == "c2"));
        assert_eq!(
            fs::read_to_string(options.out_dir.join("answer.txt")).unwrap(),
            "391"
        );
    }
}
