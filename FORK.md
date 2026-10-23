# codex-local

A CLI-only downstream fork of [OpenAI Codex](https://github.com/openai/codex) for
running against **your own local models**. Installed command: **`clx`**.

This is not affiliated with or endorsed by OpenAI. Codex is Apache-2.0; this fork
keeps the license and adds local-only behavior. For distributed builds, rename the
product and remove OpenAI/Codex branding.

## Goals

- `clx` is the only entry point: no ChatGPT login, no cloud model picker.
- Any OpenAI-compatible **Responses API** endpoint works (vLLM, Flare, etc.).
- Model list comes from `GET /v1/models`; per-model metadata lives in a local
  catalog.
- Raw reasoning is visible by default (Codex's `--oss` mode already sets
  `show_raw_agent_reasoning = true`).
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
git tag clx-v0.165.0
git push origin local clx-v0.165.0
```

Rules that keep merges cheap:

1. Prefer new modules/crates over edits to upstream files.
2. Keep local edits behind narrow seams (provider registry, config validation,
   launcher) and feature flags.
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
3. **Native `clx` binary** (`codex-rs/clx`)
   - Owns `CLX_HOME` (default `~/.clx`) so local sessions do not inherit
     ChatGPT plugins/MCP servers from `~/.codex`.
   - Queries `GET /v1/models`, reads `max_model_len` (or `context_length`) and
     writes a model catalog.
   - Generates `~/.clx/config.toml` and launches the fork's `codex` with
     `--oss --local-provider local`.
   - Optional `~/.clx/clx.toml` for `base_url` / `model` defaults.
4. **Help text** for `--local-provider`.
5. **Effort normalization for local providers**
   - Catalog exposes `none` (off), `low`, `medium`, `high`, `xhigh`, `max`.
   - vLLM rejects `minimal`/`medium`, so the client maps `minimal -> low` and
     `medium -> high` on the wire while keeping the selected label.

Build/check while iterating:

```bash
cd codex-rs
cargo build -p clx --bin clx
cargo build -p codex-cli --bin codex
cargo test -p codex-model-provider-info -p clx
```

Run (from `codex-rs`):

```bash
CLX_BASE_URL=http://192.168.50.219:8015/v1 ./target/debug/clx --model DeepSeek-V4.1-Flash-TP4
CLX_BASE_URL=http://192.168.50.219:8015/v1 ./target/debug/clx models
```

Or persist defaults in `~/.clx/clx.toml`:

```toml
base_url = "http://192.168.50.219:8015/v1"
model = "DeepSeek-V4.1-Flash-TP4"
```

## Install into `~/.codex`

This replaces the normal `codex` binary so tools that look for Codex find the
fork. Back up the config first; keep the old binary if you want a rollback.

```bash
cd codex-rs
cargo build --release -p clx --bin clx
cargo build --release -p codex-cli --bin codex

install -m 755 target/release/clx ~/.local/bin/clx
CODEX_BIN="$(readlink -f ~/.local/bin/codex)"
cp -p ~/.codex/config.toml ~/.codex/config.toml.chatgpt-backup-$(date +%Y%m%d-%H%M%S)
install -m 755 target/release/codex "$CODEX_BIN"
```

Point plain `codex` at the local provider by adding these top-level keys to
`~/.codex/config.toml` (the `clx`-generated catalog supplies model metadata):

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
```

`clx` is the generic entry point: it reads `~/.clx/clx.toml`, queries
`/v1/models`, rewrites the catalog, and launches the fork. A minimal config:

```toml
base_url = "http://127.0.0.1:8000/v1"
model = "DeepSeek-V4.1-Flash-TP4"
```

If the fork binary is not next to `clx` (or in `~/.local/lib/clx/codex`), set
`CLX_CODEX_BIN` to its path.

## Roadmap

- Multi-endpoint config (vllm, flare, etc.) and an interactive model picker.
- Per-backend reasoning adapters (`reasoning`, `reasoning_content`,
  `reasoning_text`) and Responses↔Chat-Completions fallback.
- Optional embedded reasoning-summary bridge for `exec --json` consumers.
- Local-only config defaults: verify no auth/telemetry/update surfaces remain.
- Packaging: `clx` release binaries for macOS/Linux; install layout
  `~/.local/bin/clx` + the fork binary at the normal `codex` path.

## License

Apache-2.0. See `LICENSE` and `NOTICE`. The `codex-local`/`clx` names are this
fork's own; do not ship OpenAI marks or imply endorsement.
