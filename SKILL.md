---
name: codex-local-maintenance
description: Maintain the codex-local fork of OpenAI Codex: merge upstream release tags, preserve the local-provider and effort patches, build and install the macOS `codex` binary, and verify local-model TUI/catalog behavior. Use when updating the fork, resolving upstream conflicts, reinstalling after a build, or debugging local provider/`/model` issues.
metadata:
  version: "1"
---

# codex-local maintenance

Maintain `/home/kal/Agent/Harness/codex-local`, the CLI-only local-model fork installed as
**`codex`**. The fork is Apache-2.0; do not ship OpenAI marks or imply endorsement.

## Hard rules

- `codex` is the only entry point. **Do not reintroduce `clx`** or any second
  launcher; it was deliberately removed.
- Do not touch OMP (`~/.omp`) or the Recitation repo.
- Back up `~/.codex/config.toml` before changing it.
- Do not commit `auth.json`, tokens, session data, or build artifacts.
- Prefer merge commits over cherry-picking upstream. Never force-push `local`.
- Keep local edits narrow and mergeable. Do not reformat upstream files.

## Repo and install facts

| Item | Value |
| --- | --- |
| Repo | `/home/kal/Agent/Harness/codex-local` |
| Origin | `https://github.com/nero-/codex-local` |
| Upstream | `https://github.com/openai/codex` |
| Working branch | `local` (default branch) |
| Installed binary | `~/.local/share/mise/installs/codex/0.163.0/bin/codex` (first on PATH) |
| CLI symlink | `~/.local/bin/codex` → `~/.codex/packages/standalone/current/bin/codex` |
| Config | `~/.codex/config.toml` |
| Catalog | `~/.codex/model-catalog.json` (real file) |
| Model discovery | `codex models refresh` / `codex models list` |

The fork's current base tag is `rust-v0.163.0`; upstream releases use tags like
`rust-v0.164.0`, not `v0.164.0`.

## Local patch map

Only these upstream seams are intentionally modified:

1. `codex-rs/model-provider-info/src/lib.rs`
   - `LOCAL_OSS_PROVIDER_ID = "local"`
   - `create_local_oss_provider()` (env precedence:
     `CLX_BASE_URL` → `CODEX_LOCAL_BASE_URL` → `CODEX_OSS_BASE_URL` →
     `http://127.0.0.1:8000/v1`)
   - built-in provider registration.
2. `codex-rs/config/src/config_toml.rs`
   - `local` is a reserved/valid OSS provider.
3. `codex-rs/utils/cli/src/shared_options.rs`
   - `--local-provider` help text includes `local`.
4. `codex-rs/core/src/client.rs`
   - `normalize_local_reasoning_effort()` maps `minimal -> low` and
     `medium -> high` for providers whose name starts with `local`.
   - Called from `build_reasoning()`.
5. `codex-rs/cli/src/models_cmd.rs` plus a `models` subcommand in
   `codex-rs/cli/src/main.rs`
   - `codex models refresh`: GET the configured provider's `/v1/models`,
     read `max_model_len`/`context_length`, rewrite the catalog.
   - `codex models list`: print catalog models.
6. Inline reasoning disclosure in `codex-rs/tui`
   - `ReasoningSummaryCell` is a visible transcript block with a compact heading
     and an expandable Markdown body.
   - `F4` focuses the latest activity; `Enter` toggles details. The owned
     transcript also exposes `+ Show reasoning` as a mouse target.
   - Standalone reasoning cells are kept separate from tool groups. Reasoning
     that arrives while a tool is still active is deferred and flushed directly
     after that tool cell.
   - `codex exec --json` emits raw `reasoning_text` as a `reasoning` item when
     the provider supplies no summary. This is what lets local vLLM reasoning
     appear in Harness² trajectories.
7. `codex-rs/harness2` plus `codex harness2` in `codex-rs/cli`
   - Native Propose–Probe–Compose task recursion, parallel and sequential.
   - Per-domain experience library with adopted harnesses and `EXPERIENCE.md`.
   - Rollouts run as child `codex exec --json` processes in isolated workspace
     copies; see `docs/harness2.md`.
8. Local multi-agent compatibility
   - `Config::multi_agent_version_for_model` forces V1 for every provider whose
     name starts with `local`. This overrides both a catalog
     `multi_agent_version` and `features.multi_agent_v2`, so there is one local
     subagent protocol to maintain.
   - `codex-rs/cli/src/models_cmd.rs` leaves `multi_agent_version` unset.
   - `codex-rs/core/src/client_common.rs` still rewrites
     `ResponseItem::AgentMessage` to a portable assistant `output_text` message
     for non-OpenAI providers, as a second line of defence if a future path
     bypasses the V1 override.
   - The generated catalog advertises `"input_modalities": ["text", "image"]`
     and `supports_image_detail_original`.
9. Inline terminal graphics and text sizing
   - `codex-rs/tui/src/graphics.rs` scans transcript markers and renders
     `view_image` cells with `ratatui-image` (Kitty placeholders, Sixel, or
     halfblocks).
   - `tui.rendering.inline_images` toggles images.
   - `tui.rendering.text_sizing` rewrites bold+underlined H1 rows as Kitty
     OSC 66 `s=2` text, removing the `# ` prefix. It skips wrapped headings,
     headings whose 2x text would overflow, and headings on the last viewport row.
     Enable on Kitty/Tern only; WezTerm/Ghostty do not render OSC 66. Like
     inline images, sizing applies in the owned/fullscreen transcript; raw
     scrollback stays 1x.

Do not rename the local provider away from a `local*` prefix without updating
that normalization; vLLM rejects `minimal`/`medium` with HTTP 400.

`FORK.md` is the human-readable companion. Keep it in sync when behavior changes.

## Merge upstream correctly

```bash
cd /home/kal/Agent/Harness/codex-local
git status --short                 # must be clean
git remote -v                      # origin=fork, upstream=openai/codex
git fetch upstream --tags

# latest stable tag (ignore alpha/beta)
git tag -l 'rust-v*' --sort=-v:refname \
  | grep -E '^rust-v[0-9]+\.[0-9]+\.[0-9]+$' | head -1

git checkout local
git merge rust-v0.164.0            # merge, do NOT cherry-pick
```

Resolve conflicts by preserving the local seams above. If a conflict touches a
new upstream API, adapt the local patch around it rather than reverting upstream.
Run `git rerere` (already enabled locally) so repeated conflicts replay.

After a successful build/install:

```bash
git tag codex-local-v0.164.0
git push origin local codex-local-v0.164.0
```

Update the base version and install path in this file and `FORK.md` when the
installed release directory changes.

## Build on this machine

`codex-rs/rust-toolchain.toml` pins an older Rust; rustup may fail to download it.
Use the already-installed toolchain directly:

```bash
cd /home/kal/Agent/Harness/codex-local/codex-rs

cargo check -p codex-cli
cargo test -p codex-model-provider-info
cargo test -p codex-harness2
cargo test -p codex-exec --lib reasoning_without_a_summary_emits_raw_content
```

Release build (LTO off keeps memory sane; the link step still spikes and is not a
leak):

```bash
CARGO_PROFILE_RELEASE_LTO=false \
CARGO_PROFILE_RELEASE_DEBUG=0 \
CARGO_PROFILE_RELEASE_STRIP=symbols \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
cargo build --release -p codex-cli --bin codex -j 8
```

Expected: `target/release/codex` built in roughly 6–8 minutes. Commit
`codex-rs/Cargo.lock` only when the change intentionally adds or updates a
dependency (as the `codex-harness2` crate does); otherwise revert churn.

The `target/` directory is large (tens of GB); it is gitignored.

## Install

Never overwrite an unexpected path.

```bash
cd /home/kal/Agent/Harness/codex-local/codex-rs
CODEX_BIN="$(readlink -f "$(command -v codex)")"
case "$CODEX_BIN" in
  "$HOME/.local/share/mise/installs/codex/"* | "$HOME/.codex/packages/standalone/"*) ;;
  *) echo "unexpected codex path: $CODEX_BIN" >&2; exit 1 ;;
esac

cp -p "$HOME/.codex/config.toml" \
  "$HOME/.codex/config.toml.chatgpt-backup-$(date +%Y%m%d-%H%M%S)"
cp -p "$CODEX_BIN" "$CODEX_BIN.upstream-$(date +%Y%m%d-%H%M%S)"
install -m 755 target/release/codex "$CODEX_BIN"
codex --version
```

Do not run the official Codex installer or `codex update` unless intentionally
restoring upstream; both can overwrite the fork.

## Required config invariants

`~/.codex/config.toml` must keep these top-level keys (values may change with the
endpoint/model):

```toml
model = "DeepSeek-V4.1-Flash-TP4"
model_provider = "local-default"
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"
show_raw_agent_reasoning = true
check_for_update_on_startup = false
model_catalog_json = "/home/kal/.codex/model-catalog.json"
web_search = "disabled"

[tui]
fullscreen_transcript = true
raw_output_mode = false
pet = "disabled"

[tui.rendering]
inline_images = true
text_sizing = true
mermaid = true
math = true
tables = true
lists = true

[model_providers.local-default]
name = "local"
base_url = "http://192.168.50.219:8015/v1"
wire_api = "responses"
requires_openai_auth = false

[features]
daemon_auto_start = false
multi_agent_v2 = false

# Local web search (no OpenAI-hosted search).
[mcp_servers.ddg]
command = "uvx"
args = ["duckduckgo-mcp-server"]
startup_timeout_sec = 90
tool_timeout_sec = 120
```

Why these matter:

- `wire_api = "chat"` is rejected by Codex 0.16x; use `responses`.
- `requires_openai_auth = false` keeps the endpoint unauthenticated.
- `fullscreen_transcript = true` enables in-place disclosure controls; without
  an alternate screen, the TUI falls back to the terminal-owned transcript.
- `raw_output_mode = false` keeps Markdown rich rendering enabled. The renderer
  already supports headings, lists, tables, code fences, math, and Mermaid.
- Provider `name` must start with `local` so effort normalization applies.
- `daemon_auto_start = false` is required for `/model` correctness. The TUI gets
  its model list from the app-server; a managed daemon can serve a stale GPT
  catalog. Embedded mode reads `model_catalog_json` directly.
- If `/model` still shows GPT models, confirm the feature flag, then stop/restart
  any managed daemon:
  `codex app-server daemon stop` (or `codex --no-daemon` for a one-off TUI).

The catalog is a normal file, not a symlink. It is generated by
`codex models refresh`; per-model `context_window` comes from the endpoint, so do
not put a global `model_context_window` override back in the config. Preserve
the effective fields if editing by hand: slug/display_name/context_window/
base_instructions and the effort list (`none, low, medium, high, xhigh, max`).

## Verification checklist

Run all of these after an install:

```bash
codex --version
codex debug models | jq -r '.models[].slug'          # expect local model(s) only
codex exec --sandbox read-only --skip-git-repo-check "Say OK." </dev/null
```

- `codex debug models` must not list GPT models.
- `codex models refresh` and `codex models list` must work against the local
  endpoint.
- `codex exec` must answer through provider `local` (check the header if needed).
- Reasoning should be visible in human `exec` output (raw text before answer).
- `codex exec -c model_reasoning_effort=medium ...` must not return HTTP 400.
- In the TUI, `/model` must list only the local catalog; effort must show
  off/low/medium/high/xhigh/max.
- In the TUI, reasoning appears as a collapsed `Reasoning` block. Press `F4`,
  then `Enter`, or click `+ Show reasoning`, to expand it; press `Enter` again to
  collapse it. `Ctrl+T` shows the full transcript.
- `codex exec --json` must contain a `reasoning` item on a prompt that triggers
  thinking, and `reasoning_output_tokens` must be non-zero.
- `codex harness2 run --task ... --k 1 --dry-run` resolves; a real run writes
  `result.json`, `rollouts/`, `work/adopted/`, and publishes produced files.
- `codex exec` with `--image` must describe a test PNG when the endpoint supports
  vision.
- In a Kitty terminal, a `view_image` cell must show the image instead of a
  private-use marker. `tui.rendering.inline_images = false` restores the text
  caption only.
- V1 subagents do not inherit parent history by default: `fork_context` defaults
  to false. If a child sees the parent prompt, the spawn call used
  `fork_context: true` or the parent copied context into the spawn message.
- `fork_context: true` fails with `--ephemeral` (`no rollout found for thread`).
  Use a persisted session when testing history forks.
- Permission inheritance was verified end-to-end: read-only children cannot
  write anywhere, workspace-write children can write the same roots as the
  parent, and approvals are inherited from the parent turn.
- Assistant and expanded reasoning Markdown should render code fences, tables,
  lists, math, and Mermaid without literal Markdown delimiters.
- If testing TUI with a pty, pass `--no-daemon` only as a diagnostic; the shipped
  config already disables daemon auto-start.

## Known traps

- `/model` shows GPT models: stale managed daemon or `daemon_auto_start` missing.
- `medium`/`minimal` HTTP 400: effort normalization not running; check provider
  name prefix and `normalize_local_reasoning_effort`.
- `apply_patch` unavailable: Codex sends it as a custom/freeform tool; this vLLM
  build drops it. The model edits via shell instead.
- `previous_response_id`/`store`: unsupported (404); Codex must stay stateless.
- `codex exec --json` emits summaries first, then raw `reasoning_text` when no
  summary exists. If a future upstream change drops raw content again, fix the
  `ThreadItem::Reasoning` mapping in
  `codex-rs/exec/src/event_processor_with_jsonl_output.rs`; do not add a bridge.
- Subagents produce `agent_message` validation errors under V2: the catalog must
  not force V2. Prefer V1 (`multi_agent` feature, default true). The portable
  `AgentMessage` rewrite in `client_common.rs` is the safety net for explicit V2.
- Hosted `web_search` is unavailable on vLLM (`alpha/search` is not implemented).
  Keep `web_search = "disabled"` and use a local MCP search server, e.g.
  `[mcp_servers.ddg] command = "uvx" args = ["duckduckgo-mcp-server"]`. Do not
  enable OpenAI-hosted search.
- `view_image` needs image modalities in the catalog. If the tool refuses with
  "image inputs aren't supported", rerun `codex models refresh` after updating
  `models_cmd.rs` and confirm `input_modalities` contains `image`.
- Inline images need the owned/fullscreen transcript. If
  `tui.fullscreen_transcript = false` or `raw_output_mode = true`, history lives
  in terminal scrollback and Codex cannot map a marker row to the image.
- Harness² editor sessions are agentic `codex exec` children. They need the same
  provider, catalog, and `CODEX_HOME` as the parent; do not run them with
  `--ignore-user-config`.
- Build memory spike: compile/link, not a leak; check `memory_pressure` and swap
  before investigating.
- Upstream tag merge conflicts often land in `cli`/`core`/`config`; keep local
  helper functions self-contained and re-apply them rather than rewriting
  upstream logic.

## Commit and push

```bash
cd /home/kal/Agent/Harness/codex-local
git status --short
# never stage codex-rs/Cargo.lock churn or target artifacts
git add <specific files>
git commit -m "<concise local change>"
git push origin local
```

Keep local commits small and one concern each. If a change affects behavior,
update `FORK.md` and this skill in the same commit.

## Rollback

- Restore the newest `~/.codex/config.toml.chatgpt-backup-*` if config is wrong.
- Reinstall upstream Codex from `https://chatgpt.com/codex/install.sh` only if the
  user explicitly wants to abandon the fork.
- Keep the `*.upstream-*` copy made by the install step; it is the rollback.
