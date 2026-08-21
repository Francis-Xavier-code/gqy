# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

GQY（顾清影）is a single-binary macOS CLI AI assistant in Rust — a persistent chat
persona plus a terminal tool-bearer. One binary (`src/main.rs` → `gqy`) serves
three surfaces: terminal REPL, built-in web UI, and chat platforms (QQ/OneBot).
Requires Rust 1.89+. Comments and commit messages are predominantly Chinese;
match that when editing existing code.

## Commands

```sh
cargo build                      # debug binary at target/debug/gqy
cargo build --release            # target/release/gqy
cargo fmt --all --check          # CI-enforced
cargo clippy --all-targets -- -D warnings   # CI-enforced
cargo test --all                 # ~1400 tests, mostly in-crate #[cfg(test)] modules
cargo test agent::tests4::       # one module
cargo test <test_fn_name>        # one test by substring
cargo test --test daemon_reload  # one integration test file (tests/)
```

`build.rs` obfuscates the persona prompts into `OUT_DIR`, builds the o200k and
jieba indexes from `assets/`, and stamps `GQY_BUILD_ID` from the current time —
so **every source change forces a rebuild of build.rs's outputs**, and a client
whose `BUILD_ID` differs from the running daemon's restarts that daemon.

Manual/e2e harnesses live in `testkit/` (Python, PTY-driven, real providers):
`testkit/dev-smoke/run.py` (daemon-form dev REPL smoke, isolated home + port
18390), `testkit/persona-ab/run.py` (persona A/B, `GQY_DIRECT=1`, one home per
conversation). Both deliberately avoid the live 8300 daemon.

Release: pushing a `v*` tag runs the CNB pipeline `.cnb.yml` — fmt/clippy/test
→ dual-arch macOS builds + dual-arch Linux static builds → CNB Release with
binaries + regenerated Homebrew formula (which is committed back to `main`).

This repo mirrors to three platforms: `origin`=CNB, `github`=`Francis-Xavier-code/gqy`,
`gitee`=`Xynrin/GQY`. Use `bash scripts/sync-remotes.sh` to fetch/pull/push all
three at once (run with no args to see divergence first). The three histories have
diverged — confirm the canonical `main` before merging across remotes.

## Process model

The CLI is mostly a **thin IPC client**. `src/cli/daemon.rs::run` dispatches the
clap `Command` enum (defined in `src/cli/defs.rs:1521`); state-mutating commands
prefer the daemon when one is running and fall back to in-process execution when
it isn't (see `Command::Pop`, `Command::Reset`).

- **Daemon** — one background host per `GQY_HOME`, spawned as `gqy __daemon`
  (hidden subcommand) via `current_exe`. `src/daemon.rs` delegates lifecycle to
  `src/web.rs`, which owns the axum server, the Unix-socket IPC listener
  (`src/ipc.rs`, `PROTOCOL_VERSION`), and configured platform transports.
- **Direct mode** — `GQY_DIRECT=1` runs the REPL in-process, bypassing the
  daemon (`src/cli/direct.rs`). It takes an exclusive "core lease" on the home,
  so it is mutually exclusive with a running daemon. Not all REPL commands work
  here (e.g. `/new`); `src/cli/remote.rs` is the daemon-attached counterpart.
- **Renderer worker** — `main.rs` short-circuits to
  `platforms::plugins::run_renderer_worker()` when `GQY_INTERNAL_RENDERER_WORKER`
  is set, before any config is loaded.

Everything the app writes lives under one root: `~/.gqy` (or `$GQY_HOME`), with
`config/config.jsonc`, `data/`, `cache/`, `state/` beneath it — see
`src/paths/layout.rs`, which also handles migration from the legacy `miyu` XDG
layout. **Note:** the README's `~/.config/gqy/config.jsonc` is stale.
`src/transfer/registry.rs` requires every path under `GQY_HOME` to be classified
(a test fails on unclassified paths) — add new persistent files there.

Useful env vars: `GQY_HOME`, `GQY_DIRECT`, `GQY_LANG` (`auto`/`zh`/`en`, drives
`src/i18n.rs`), `GQY_LOG_REQUESTS=1` (record outbound LLM requests),
`GQY_SESSION`, `GQY_TURN_ORIGIN`.

## The cache contract (read before touching context assembly)

`docs/理念.md` is the design rationale and it is load-bearing, not aspirational.
Prompt-prefix cache hits dominate cost for a long-lived agent, and the cache
matches **bytes**, not meaning. The rules that constrain real code:

1. **Prefix is a contract** — identical conversation state must produce a
   byte-identical request prefix. No nondeterminism (timestamps, probes, async
   model metadata) in the prefix unless frozen at entry; exactly one
   serialization path per message.
2. **Append-only** — never insert into the middle, never delete what was sent.
   Transient blocks (runtime `<runtime now=…/>`, associative memory, image
   hints, sender metadata) are **fossilized** into `turns.context_messages` and
   replayed byte-for-byte forever. `ChatMessage::transient_context` marks the
   tail that gets fossilized.
3. **Compaction is the only legal prefix reset**, and it must be monotonic.
   `src/agent/tests4.rs` asserts this: every request must be a pure append-only
   extension of the previous one, and any divergence must be a compaction
   carrying a `<conversation-checkpoint>`. That test is the regression gate —
   byte-purity degrades silently otherwise.
4. **Three-way content split** — `raw_content` (what memory/diary/retrieval
   read), `display_content` (UI), and engineering sidecars (`context_messages`).
   Cache work touches bytes; it must never pollute what the memory system sees.
5. **Authorization lives in code, never in prompt text.** Any tag claiming
   privilege is decoration; the execution layer checks the real principal
   (`src/platforms/access_control.rs`). This is what makes context re-layout safe.
6. **Stub tools** — ~80 tools would blow up the tools array, and any change to
   that array invalidates the whole prefix. Default `tools.mode = "stub"`
   (`src/config/schema.rs:197`, `src/tools/registry.rs::stub_definitions`) keeps
   every tool permanently present as name + one-line summary + loose parameter
   shell; full contracts arrive on demand via the `load_tools` **tool result**,
   which lands in the tail and never touches the prefix.
7. Provider cache behavior differs per vendor (contract / best-effort /
   per-request billing). Universal wins (byte purity, append-only, fossils,
   stubs) are on by default; vendor-specific gambles (`cache.keepalive_seconds`,
   `cache.write_grace_ms` in `src/config/core.rs`) default to **off**.
8. Observe with absolute numbers, not percentages —
   `prompt cache accounting prompt_tokens=… cache_read=… fresh=…`
   (`src/llm/cache_log.rs`).

`docs/archive/compact-plan.md` holds the compaction design (summary + fixed-token
verbatim tail, turn-boundary cuts, mechanical fallback on summarizer failure,
anti-thrash gates) with per-competitor evidence; `src/agent/compact.rs` and
`src/agent/overflow.rs` are the implementation.

## Module layout and the splitting convention

Modules are kept around ~1500 lines. Oversized files are split into a directory
with a `mod.rs` that declares submodules and re-exports (`pub(crate) use x::*;`),
where `impl X {}` blocks are spread across several files. Hence the numeric
suffixes everywhere: `agent_impl.rs`…`agent_impl4.rs`, `sessions_a.rs`/`sessions2.rs`,
`tests.rs`…`tests4.rs`, `providers.rs`/`providers2.rs`. Submodules typically start
with `pub(crate) use super::*;` and inherit the parent's imports — so adding an
import to the parent `mod.rs` can be what fixes a submodule. Test modules are
flat (`mod tests` … `mod tests3`), never nested.

Clippy is `-D warnings`; project practice is to converge unavoidable lints as a
**file-level** `#![allow(clippy::…)]` at the top of the split file (must sit above
the `pub(crate) use super::*;` line), not as scattered per-item allows.

Subsystems worth knowing before editing:

- `src/agent/` — the turn loop, tool loop, vision, overflow/compaction,
  subagent research (`research.rs`), background tasks. Two modes:
  `AgentMode::Normal` (full persona) and `AgentMode::Dev` (minimal coding form,
  no persona; its own persona scope `state::DEV_PERSONA`).
- `src/llm/openai_compatible/` — the only LLM transport. Provider quirks are
  concentrated in `providers*.rs` (e.g. DeepSeek needs a `reasoning_content`
  key present on replayed tool-call turns; Anthropic thinking signatures are
  captured but never serialized as OpenAI JSON).
- `src/state/` — SQLite (`rusqlite`, bundled) conversation DB, sessions,
  schema migrations (`migrations.rs`). Compaction is a soft delete (`hidden=1`)
  with full undo; `replace_visible_with_summary` has optimistic concurrency.
- `src/tools/` — registry + ~77 tool implementations; JSON parameter schemas
  live as separate files in `src/tools/descriptions/`.
- `src/platforms/` — transport adapters (QQ via OneBot), access control, and
  the plugin family under `plugins/` (renderer subprocess, reply processor,
  meme collector, group management).
- `src/render/` — terminal rendering (syntect, termimad, cosmic-text, ratex for
  math), plus long-reply-to-image.
- `src/prompts/` — persona and workflow prompts as Markdown. `GQY.md`,
  `GQY.hint.md`, `GQY-dialogs.md` are XOR-masked at build time; the rest are
  plain `include_str!`.
- `web/` — the web UI (plain `index.html`/`styles.css`/`app.js` + vendored
  KaTeX), embedded via `include_str!`/`include_bytes!` in `src/web/defs.rs`.
  No JS build step; editing these files is enough.

## Conventions

- Persona/tool-usage rules belong in `src/prompts/`; tool behavior and review
  flows belong in `src/tools/`. Don't blur the two.
- Do not touch `kb/` — the default knowledge base is maintained by hand.
- User-facing strings are bilingual through `crate::i18n::{text as t, is_zh}`;
  clap help is localized in `src/cli/defs.rs` (`localize_subcommands`,
  `apply_chinese_help_template`).
- `docs/architecture.md` is a short orientation doc; keep it in sync when
  module responsibilities move.
