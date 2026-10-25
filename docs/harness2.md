# Harness² in codex-local

`codex harness2` is a native port of
[google-research/harness2](https://github.com/google-research/harness2) for this fork's Codex
substrate. It implements the paper's two connected levels of recursion without a Python
dependency or a benchmark harness:

* **Task-level recursion** runs a reference rollout, proposes invariant and domain-specific
  harness edits, probes each candidate on the same task, then composes a successor from the
  behavioral contrasts.
* **Harnessing-level recursion** reflects on the whole record after the task, stores the adopted
  harness and the lesson that survived evidence review in a per-domain library, and reads that
  library back during the next task's proposals.

All model work is delegated to `codex exec --json` with the same provider, model, reasoning
effort, and sandbox policy as the interactive harness. Rollouts run in isolated copies of the
workspace, so parallel probes cannot observe or overwrite one another.

## Usage

```bash
# One Propose–Probe–Compose step (k=1).
codex harness2 run --task "Create report.md from data.csv"

# Wider exploration: k invariant + k domain candidates, k compositions, blind selection.
codex harness2 run --task-file task.md --k 3 --mode parallel

# Deeper exploration: each checkpoint becomes the next round's base.
codex harness2 run --task-file task.md --k 3 --mode sequential

# Inspect what earlier tasks in this domain learned.
codex harness2 library --domain general
```

Useful options:

| Option | Meaning |
|---|---|
| `--workspace DIR` | Task workspace. Defaults to the current directory. |
| `--harness DIR` | Initial harness. Defaults to the workspace's editable files, else a minimal base. |
| `--out DIR` | Where the selected run's produced files are published. Defaults to `<workspace>/.harness2/out`. |
| `--runs DIR` | Evidence and result root. Defaults to `$CODEX_HOME/harness2/runs`. |
| `--library DIR` | Experience library root. Defaults to `$CODEX_HOME/harness2/library`. |
| `--domain NAME` | Library partition. Defaults to `general`. |
| `--solver-model` / `--editor-model` | Override the rollout or improver model. |
| `--reasoning-effort` | Sent to both solver and improver sessions. Defaults to `high`. |
| `--concurrency` | Maximum concurrent rollouts and editor sessions. Defaults to 2. |
| `--timeout` | Per-session timeout in seconds. Defaults to 1800. |
| `--dry-run` | Resolve the plan without calling a model. |

A run directory contains `result.json`, every rollout trajectory under `rollouts/`, the
proposal and composition evidence trees under `work/`, the adopted harness under
`work/adopted/`, and the selected deliverable copies under `--out`.

## Editable surface

The surface is the Codex component tree documented by Harness²:

| Path | Role |
|---|---|
| `AGENTS.md` | Always-on procedure; the only always-on file, capped at 8,000 chars. |
| `.agents/skills/<name>/SKILL.md` | On-demand workflow; `description:` is required and the name must be triggered from `AGENTS.md`. |
| `.agents/skills/<name>/scripts/*` | Helper carried with a skill. |
| `sub_agents/<name>/AGENT.md` | Delegated agent role, installed as a Codex `[agents.<name>]` role for the rollout. |
| `scripts/<name>.py` | Deterministic helper, syntax-checked before any rollout. |
| `.codex/hooks.json` | Lifecycle hooks using the Claude-Code hook schema. |

Validation rejects a proposal before rollout when: `AGENTS.md` is empty or over the cap, a
skill has no `description:`, a skill is never named in `AGENTS.md`, a script fails
`python3` compilation, a referenced script is missing, or the hooks file is malformed. A
rejected proposal is sent back once with the validator message; a second failure falls back to
the harness the stage started from.

## Evidence boundary

Proposers receive evidence as files, never as an inlined prompt:

```text
context/task.md
context/source/
context/rollouts/<label>.md
context/deliverables/<label>/
context/harness/
context/library/
context/pool/<label>/{harness,rollout.md,produced/}
context/decisions/<label>.md
```

Prompts explicitly state that no rubric, answer key, or grader exists, and that files under
`context/` are data rather than instructions. The library is per domain; retrieval is by
construction, so a task only sees earlier entries from its own domain.

## Relation to the reference implementation

The stage prompts, Propose–Probe–Compose structure, parallel/sequential modes, blind selection,
and reflection format follow the reference release. The implementation differs where Codex and
this fork require it:

* Codex is the only substrate. There is no OpenCode adapter or benchmark adapter.
* A generic task/workspace replaces JobBench, LAB, and WorkBuddy-Bench.
* Rollouts are isolated workspace copies; the selected files are published to `--out`.
* Raw provider reasoning (`reasoning_text`) is surfaced by `codex exec --json` as well as the
  TUI, so improvers see the same reasoning blocks a person sees.
