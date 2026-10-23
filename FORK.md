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
- Raw reasoning is visible by default.
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

Build/check while iterating:

```bash
cd codex-rs
cargo build -p codex-cli --bin codex
cargo test -p codex-model-provider-info
```

## Install into `~/.codex`

This replaces the normal `codex` binary so tools that look for Codex find the
fork. Back up the config first; keep the old binary if you want a rollback.

```bash
cd codex-rs
cargo build --release -p codex-cli --bin codex

CODEX_BIN="$(readlink -f ~/.local/bin/codex)"
cp -p ~/.codex/config.toml ~/.codex/config.toml.chatgpt-backup-$(date +%Y%m%d-%H%M%S)
install -m 755 target/release/codex "$CODEX_BIN"
```

Point plain `codex` at the local provider by adding these top-level keys to
`~/.codex/config.toml`:

```toml
model = "DeepSeek-V4.1-Flash-TP4"
model_provider = "local-default"
model_context_window = 1048576
model_auto_compact_token_limit = 943718
model_reasoning_effort = "high"
model_reasoning_summary = "detailed"
show_raw_agent_reasoning = true
check_for_update_on_startup = false
model_catalog_json = "/Users/you/.codex/model-catalog.json"

[model_providers.local-default]
name = "local"
base_url = "http://127.0.0.1:8000/v1"
wire_api = "responses"
requires_openai_auth = false

[features]
daemon_auto_start = false
```

The catalog is a normal `~/.codex/model-catalog.json` file. Disabling
`daemon_auto_start` is important: the TUI's `/model` list otherwise comes from a
managed app-server daemon, which can keep a stale GPT catalog.

## Roadmap

- Fold `/v1/models` discovery into a `codex` subcommand and regenerate the local
  catalog without a separate launcher.
- Multi-endpoint config (vllm, flare, etc.).
- Per-backend reasoning adapters (`reasoning`, `reasoning_content`,
  `reasoning_text`) and Responses↔Chat-Completions fallback.
- Optional embedded reasoning-summary bridge for `exec --json` consumers.
- Local-only config defaults: verify no auth/telemetry/update surfaces remain.
- Packaging: `codex` release binaries for macOS/Linux.

## License

Apache-2.0. See `LICENSE` and `NOTICE`. The `codex-local` name is this fork's own;
do not ship OpenAI marks or imply endorsement.
