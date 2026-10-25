//! `codex harness2`: recursive harness improvement for the local Codex harness.

use anyhow::Context;
use anyhow::Result;
use clap::Args;
use clap::Subcommand;
use codex_harness2::Mode;
use codex_harness2::RunOptions;
use codex_harness2::library::Library;
use codex_harness2::runner;
use codex_harness2::surface;
use codex_harness2::surface::Tree;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Args)]
pub struct Harness2Command {
    #[command(subcommand)]
    pub subcommand: Harness2Subcommand,
}

#[derive(Debug, Subcommand)]
pub enum Harness2Subcommand {
    /// Run the Propose-Probe-Compose loop for one task.
    Run(RunArgs),

    /// Print the experience library for a domain.
    Library(LibraryArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// The task, exactly as the solver should receive it.
    #[arg(long, conflicts_with = "task_file")]
    pub task: Option<String>,

    /// Read the task from a file.
    #[arg(long, value_name = "FILE", conflicts_with = "task")]
    pub task_file: Option<PathBuf>,

    /// Workspace the task is performed in. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<PathBuf>,

    /// Existing harness directory to start from. Defaults to the workspace's editable
    /// harness files, or a minimal base harness when the workspace has none.
    #[arg(long, value_name = "DIR")]
    pub harness: Option<PathBuf>,

    /// Where the selected deliverable is written. Defaults to <workspace>/.harness2/out.
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Root for run evidence and results. Defaults to $CODEX_HOME/harness2/runs.
    #[arg(long, value_name = "DIR")]
    pub runs: Option<PathBuf>,

    /// Root for the per-domain experience library. Defaults to $CODEX_HOME/harness2/library.
    #[arg(long, value_name = "DIR")]
    pub library: Option<PathBuf>,

    /// Stable task id used for the library entry and run layout.
    #[arg(long)]
    pub task_id: Option<String>,

    /// Domain name that scopes the experience library.
    #[arg(long, default_value = "general")]
    pub domain: String,

    /// Task-level recursion budget.
    #[arg(long, default_value_t = 1)]
    pub k: usize,

    /// Recursion mode: parallel (width) or sequential (depth).
    #[arg(long, default_value = "parallel")]
    pub mode: String,

    /// Maximum concurrent rollouts / editor sessions.
    #[arg(long, default_value_t = 2)]
    pub concurrency: usize,

    /// Model for solver rollouts. Defaults to the active Codex model.
    #[arg(long)]
    pub solver_model: Option<String>,

    /// Model for proposal, composition, selection, and reflection sessions.
    #[arg(long)]
    pub editor_model: Option<String>,

    /// Reasoning effort sent to the local provider.
    #[arg(long, default_value = "high")]
    pub reasoning_effort: String,

    /// Per-session timeout in seconds.
    #[arg(long, default_value_t = 1_800)]
    pub timeout: u64,

    /// Print the resolved plan without calling a model.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct LibraryArgs {
    /// Root of the experience library. Defaults to $CODEX_HOME/harness2/library.
    #[arg(long, value_name = "DIR")]
    pub library: Option<PathBuf>,

    /// Domain to print.
    #[arg(long, default_value = "general")]
    pub domain: String,
}

pub async fn run(command: Harness2Command) -> Result<()> {
    match command.subcommand {
        Harness2Subcommand::Run(args) => run_task(args).await,
        Harness2Subcommand::Library(args) => {
            let root = args.library.unwrap_or_else(default_library_root);
            let library = Library::new(root, args.domain);
            print!("{}", codex_harness2::library::list_text(&library)?);
            Ok(())
        }
    }
}

async fn run_task(args: RunArgs) -> Result<()> {
    let workspace = args
        .workspace
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()
        .with_context(|| "resolve --workspace")?;
    let task = match (args.task, args.task_file) {
        (Some(task), None) => task,
        (None, Some(path)) => fs::read_to_string(&path)
            .with_context(|| format!("read task file {}", path.display()))?,
        (Some(_), Some(_)) => unreachable!("clap conflict"),
        (None, None) => anyhow::bail!("provide --task or --task-file"),
    };
    if task.trim().is_empty() {
        anyhow::bail!("the task is empty");
    }
    let base_harness = load_base_harness(&workspace, args.harness.as_deref())?;
    let mode = Mode::parse(&args.mode)?;
    let task_id = args.task_id.unwrap_or_else(|| default_task_id(&task));
    let runs_root = args.runs.unwrap_or_else(default_runs_root);
    let run_dir = unique_run_dir(&runs_root)?;
    let library_root = args.library.unwrap_or_else(default_library_root);
    let out = args
        .out
        .unwrap_or_else(|| workspace.join(".harness2").join("out"));
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".codex")));

    if args.dry_run {
        println!("workspace: {}", workspace.display());
        println!("run dir:   {}", run_dir.display());
        println!("out:       {}", out.display());
        println!("domain:    {}", args.domain);
        println!("mode:      {} (k={})", mode.token(), args.k);
        println!("task id:   {task_id}");
        println!("harness:");
        for (rel, text) in &base_harness {
            println!("  {rel} ({} chars)", text.len());
        }
        return Ok(());
    }

    let mut config = runner::RunnerConfig::new();
    config.codex_bin = std::env::current_exe().context("resolve codex binary")?;
    config.codex_home = codex_home;
    config.solver_model = args.solver_model;
    config.editor_model = args.editor_model;
    config.reasoning_effort = args.reasoning_effort;
    config.timeout = Duration::from_secs(args.timeout);

    let options = RunOptions {
        task,
        task_id,
        workspace,
        base_harness,
        run_dir,
        library: Library::new(library_root, args.domain),
        k: args.k,
        mode,
        concurrency: args.concurrency.max(1),
        runner: config,
        out_dir: out,
    };
    let report = codex_harness2::run(&options).await?;
    println!();
    println!("selected: {}", report.selected);
    println!("harness:  {}", report.adopted_harness.display());
    println!(
        "results:  {}",
        options.run_dir.join("result.json").display()
    );
    if report.output_files.is_empty() {
        println!("output:   (no files; the selected run returned only a final message)");
    } else {
        println!("output:   {}", options.out_dir.display());
        for file in &report.output_files {
            println!("  {file}");
        }
    }
    if let Some(entry) = report.library_entry {
        println!("library:  {}", entry.display());
    }
    Ok(())
}

fn load_base_harness(
    workspace: &std::path::Path,
    explicit: Option<&std::path::Path>,
) -> Result<Tree> {
    if let Some(path) = explicit {
        return surface::load_harness(path);
    }
    let mut rejected = Vec::new();
    let tree = surface::read_tree(workspace, &mut rejected)?;
    if tree.is_empty() {
        Ok(surface::base_harness())
    } else {
        Ok(tree)
    }
}

fn default_task_id(task: &str) -> String {
    let first = task
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("task")
        .trim();
    let mut slug = first
        .chars()
        .take(48)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug.to_string()
    }
}

fn unique_run_dir(root: &std::path::Path) -> Result<PathBuf> {
    let base = format!("harness2-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let mut candidate = root.join(&base);
    let mut counter = 1;
    while candidate.exists() {
        candidate = root.join(format!("{base}-{counter}"));
        counter += 1;
    }
    fs::create_dir_all(&candidate)?;
    Ok(candidate)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

fn default_runs_root() -> PathBuf {
    codex_home().join("harness2").join("runs")
}

fn default_library_root() -> PathBuf {
    codex_home().join("harness2").join("library")
}
