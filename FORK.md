# codex-local

A CLI-only downstream fork of [OpenAI Codex](https://github.com/openai/codex) for
running against **your own local models**. Installed command: **`codex`**.

This is not affiliated with or endorsed by OpenAI. Codex is Apache-2.0; this fork
keeps the license and adds local-only behavior. For distributed builds, rename the
product and remove OpenAI/Codex branding.

## Goals

- `codex` is the only entry point: no ChatGPT login, no cloud model picker.
- Any OpenAI-compatible **Responses API** endpoint works (vLLM, Flare, etc.).
- A local model catalog supplies the model picker and context window.
- Reasoning appears in chat as a compact block that expands to richly rendered
  Markdown on demand; raw reasoning remains available by default.
- Plugins/extensions/MCP/skills stay configurable without fighting upstream.
- Desktop app is out of scope; DeepSeek Harness covers that surface.

## Upstream merge workflow

`main` mirrors upstream. All local work lives as a small commit stack on `local`.

```bash
git remote add upstream https://github.com/openai/codex.git
git fetch upstream --tags

git checkout local
git merge rust-v0.165.0      # merge the release tag, do not cherry-pick
# resolve conflicts, run checks, commit
git tag codex-local-v0.165.0
git push origin local codex-local-v0.165.0
```

Rules that keep merges cheap:

1. Prefer new modules over edits to upstream files.
2. Keep local edits behind narrow seams (provider registry, config validation,
   effort normalization) and feature flags.
3. One concern per commit; write a short `FORK.md` note for each seam.
4. Enable `git rerere` so recurring conflicts are replayed.
5. Run upstream tests plus the local smoke test before tagging.
6. Merge upstream monthly; merge security fixes immediately.

## Current patch set

1. **Generic `local` OSS provider**
   - `codex-rs/model-provider-info/src/lib.rs`: `LOCAL_OSS_PROVIDER_ID`,
     `create_local_oss_provider()`.
   - Base URL precedence: `CLX_BASE_URL` → `CODEX_LOCAL_BASE_URL` →
     `CODEX_OSS_BASE_URL` → `http://127.0.0.1:8000/v1`.
   - No Ollama/LM Studio readiness checks or model pulling.
2. **Config validation**
   - `local` accepted as an OSS provider.
3. **Effort normalization for local providers**
   - Catalog exposes `none` (off), `low`, `medium`, `high`, `xhigh`, `max`.
   - vLLM rejects `minimal`/`medium`, so the client maps `minimal -> low` and
     `medium -> high` on the wire while keeping the selected label.
4. **Daemon-free local TUI**
   - `features.daemon_auto_start = false` in the local config so the TUI's
     `/model` list is served by the embedded app-server and the local catalog,
     not a stale managed daemon.
5. **Native model discovery**
   - `codex models refresh` queries the configured local provider's
     `GET /v1/models`, reads `max_model_len`/`context_length`, and rewrites the
     model catalog.
   - `codex models list` prints the current catalog.
6. **Inline reasoning disclosure**
   - Reasoning is a first-class transcript cell instead of a footer-only status
     update or a hidden child of a tool activity group.
   - The default compact row shows `Reasoning · <latest useful heading>`.
   - In the owned fullscreen transcript, focus the latest activity with `F4`
     and press `Enter` to toggle it. Clicking `+ Show reasoning` also works.
   - Expanded reasoning uses the normal Markdown renderer, preserving headings,
     emphasis, lists, tables, fenced-code highlighting, math, and Mermaid.
   - Reasoning received while a tool is still running is deferred and emitted
     immediately after that tool cell to preserve chronology.
   - `codex exec --json` now emits a `reasoning` item from raw
     `reasoning_text` content when the provider returns no summary, so local
     vLLM reasoning is visible to both people and Harness² proposers.
7. **Native Harness² recursion**
   - `codex harness2 run` implements Propose–Probe–Compose and the
     harness-improvement library for the Codex component tree.
   - `codex harness2 library` prints a domain's adopted harnesses and lessons.
   - See `docs/harness2.md` for the surface, evidence layout, and options.
8. **Rich local defaults**
   - The installed config enables fullscreen transcript, Markdown tables, math,
     Mermaid, and raw reasoning blocks, and sets `tui.pet = "disabled"`.
   - `codex models refresh` already discovers the local model and its 1M context
     window; no second launcher is required.
9. **Local multi-agent compatibility**
   - Every local provider now forces `MultiAgentVersion::V1` in
     `Config::multi_agent_version_for_model`, regardless of a catalog value or
     `features.multi_agent_v2`. V1 is the only local subagent path; the installed
     config also sets `multi_agent_v2 = false`.
   - Non-OpenAI providers rewrite `ResponseItem::AgentMessage` into an assistant
     `output_text` message. V2 uses that provider-native item for child results;
     vLLM validates the public schema and otherwise rejects the next request
     with hundreds of `agent_message` validation errors.
   - The local catalog advertises `text` and `image` input, so `view_image` and
     `-i/--image` work against a multimodal endpoint.
   - V1 `fork_context` defaults to `false`: children start with only their spawn
     message. `fork_context: true` copies parent history and fails under
     `--ephemeral` because there is no rollout to fork. Sandbox, approval policy,
     cwd, and workspace roots are copied from the parent turn.
10. **Inline terminal graphics**
   - `view_image` history cells reserve rows and render the actual image with
     `ratatui-image`: Kitty graphics with Unicode placeholders, Sixel, or
     halfblocks fallback.
   - Images participate in ratatui diffing and scrollback instead of being
     painted once at a fixed cursor position. They require the owned/fullscreen
     transcript (the default); raw scrollback mode has no visible buffer to map.
   - Controlled by `tui.rendering.inline_images` (default `true`).
   - `tui.rendering.text_sizing = true` renders Markdown H1 rows at 2x scale
     with Kitty's OSC 66 protocol. It only rewrites a non-wrapped heading whose
     2x text fits the remaining row and whose following row is blank, so text is
     never truncated and the two-row block does not paint over the composer.
   - Both options are opt-in at the repository level; the installed local config
     enables them for Tern/Kitty. Text sizing, like inline images, applies to the
     owned/fullscreen transcript; raw terminal scrollback stays 1x.

Build/check while iterating:

```bash
cd codex-rs
cargo build -p codex-cli --bin codex
cargo test -p codex-model-provider-info
cargo test -p codex-harness2
cargo test -p codex-exec --lib reasoning_without_a_summary_emits_raw_content
cargo test -p codex-core --lib localize_agent_messages
cargo test -p codex-tui --lib graphics::tests
```

## Install into `~/.codex`

This replaces the normal `codex` binary so tools that look for Codex find the
fork. Back up the config first; keep the old binary if you want a rollback.

```bash
cd codex-rs
cargo build --release -p codex-cli --bin codex

CODEX_BIN="$(readlink -f "$(command -v codex)")"
cp -p ~/.codex/config.toml ~/.codex/config.toml.chatgpt-backup-$(date +%Y%m%d-%H%M%S)
install -m 755 target/release/codex "$CODEX_BIN"
```

Point plain `codex` at the local provider by adding these top-level keys to
`~/.codex/config.toml`:

```toml
model = "DeepSeek-V4.1-Flash-TP4"
model_provider = "local-default"
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"
show_raw_agent_reasoning = true
check_for_update_on_startup = false
model_catalog_json = "/Users/you/.codex/model-catalog.json"

[tui]
fullscreen_transcript = true
raw_output_mode = false
pet = "disabled"

[tui.rendering]
mermaid = true
math = true
tables = true
lists = true

[model_providers.local-default]
name = "local"
base_url = "http://127.0.0.1:8000/v1" # or your LAN endpoint, e.g. http://192.168.50.219:8015/v1
wire_api = "responses"
requires_openai_auth = false

[features]
daemon_auto_start = false
```

The catalog is a normal `~/.codex/model-catalog.json` file. Disabling
`daemon_auto_start` is important: the TUI's `/model` list otherwise comes from a
managed app-server daemon, which can keep a stale GPT catalog.

## Roadmap

- Auto-refresh the catalog when the local endpoint's model list changes.
- Multi-endpoint config (vllm, flare, etc.).
- Per-backend reasoning adapters (`reasoning`, `reasoning_content`,
  `reasoning_text`) and Responses↔Chat-Completions fallback.
- Optional embedded reasoning-summary bridge for `exec --json` consumers.
- Local-only config defaults: verify no auth/telemetry/update surfaces remain.
- Packaging: `codex` release binaries for macOS/Linux.

## License

Apache-2.0. See `LICENSE` and `NOTICE`. The `codex-local` name is this fork's own;
do not ship OpenAI marks or imply endorsement.
