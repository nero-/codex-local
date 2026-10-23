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

## Install (local build)

```bash
cd codex-rs
cargo build --release -p clx --bin clx
cargo build --release -p codex-cli --bin codex

mkdir -p ~/.local/bin ~/.local/lib/clx
install -m 755 target/release/clx ~/.local/bin/clx
install -m 755 target/release/codex ~/.local/lib/clx/codex
```

`clx` looks for the fork binary at `~/.local/lib/clx/codex` (or
`$CLX_CODEX_BIN`). A minimal `~/.clx/clx.toml`:

```toml
base_url = "http://127.0.0.1:8000/v1"
model = "DeepSeek-V4.1-Flash-TP4"
```

## Roadmap

- Multi-endpoint config (vllm, flare, etc.) and an interactive model picker.
- Per-backend reasoning adapters (`reasoning`, `reasoning_content`,
  `reasoning_text`) and Responses↔Chat-Completions fallback.
- Optional embedded reasoning-summary bridge for `exec --json` consumers.
- Local-only config defaults: verify no auth/telemetry/update surfaces remain.
- Packaging: `clx` release binaries for macOS/Linux; install layout
  `~/.local/bin/clx` + `~/.local/lib/clx/codex`.

## License

Apache-2.0. See `LICENSE` and `NOTICE`. The `codex-local`/`clx` names are this
fork's own; do not ship OpenAI marks or imply endorsement.
