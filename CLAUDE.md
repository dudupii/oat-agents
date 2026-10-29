# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`oat-agents` is a Rust CLI that runs a supervised team of coding agents on one machine: one
meta-agent (`oat-meta`) delegates to member agents, each a plain Claude Code or Codex session
on its own git worktree and branch, driven through tmux. A terminal live view (`tui`) shows
the whole team. Rust 1.85+, edition 2024.

## Read before changing anything

- `.dev_docs/CONTEXT.md` — the vocabulary. Use its terms in code, tests, issues and PRs; the
  *Avoid* lines matter more than the definitions (each names a word that caused a real
  mistake). Key terms: Goal, Run, Dispatch, work, settle, Run inbox, workflow log,
  meta-agent, member agent, backend, worktree, finding, gate, rework loop.
- `.dev_docs/adr/` — every decision record touching the area you change. If your change
  contradicts one, stop and say so. Cite records by number (`ADR-0001`), never filename; CI
  checks every citation resolves.
- `AGENTS.md` and `CONTRIBUTING.md` — the rules below are summarized from them.

## Commands

CI and local verification run the same set — all five before handing work over:

```sh
cargo build --locked --all-targets
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
.github/scripts/check-dev-docs.sh
.github/scripts/check-license-headers.sh
```

One test file while iterating: `cargo test --test rework` (integration) or
`cargo test --lib findings` (unit tests inside one module). One named test:
`cargo test name_fragment`.

Note: the tree is **not** rustfmt-clean; `cargo fmt --check` fails on pre-existing files and
is not a gate. Clippy with `-D warnings` is.

## Architecture

**Core vs plugins (ADR-0001).** The core owns the mechanism — launching, worktrees, sessions,
the Run inbox, the workflow log, execution environments, the live view, and the two core
roles. Anything expressing *how a team works* — role instructions, conventions, skills,
model choices — belongs in a plugin (`plugins/example/` is the shipped one); if a core change
would encode one team's practice, it needs a plugin hook instead. Prefixes mark ownership:
`oat-` (core roles, tmux, branches), `OAT_` (env vars), `.oat/` (repo config). Plugin roles
never take the `oat-` prefix.

**Flow of one Run.** `meta fire` reads the goal, resolves and snapshots plugins, settles the
repository's role limits/settings and rework settings into the Run record, creates the
meta-agent's worktree and tmux session. The meta-agent `role fire`s member agents; each
launch writes `prompt.md`, `task.md` (the task verbatim, for rework briefs to quote),
`launch.sh` into the Dispatch's state dir and starts a tmux session on a private server.
Members end their own Dispatch with `dispatch done`; messages land in the Run inbox, which
has exactly one consumer: that Run's `oat-meta`. Observers read the append-only workflow log
(`log show`), never the inbox.

**Layout under `src/`:**
- `lib.rs` — `execute(cli, env, catalog)` is the one testable entry point every command goes
  through; `commands/` holds one file per top-level subcommand (thin: parse, then mechanism).
- `store.rs` — on-disk state, `<state root>/runs/<run_id>/` with `run.json`, `dispatches/<id>/`,
  `inbox/<seq>.json`, `plugins/`, `findings/` (the state root is `$OAT_AGENTS_STATE_DIR/runs`,
  falling back to XDG state). Run/dispatch records are serde with `deny_unknown_fields`,
  defaults via `skip_serializing_if` — old records must keep loading, so new fields default.
- `event_log.rs` — the workflow log (JSONL, event-name constants in `events`), the only
  surface observers read a Run from.
- `worktree.rs` — git worktrees and `oat/<run>/<name>` branches; `checklist.rs` and
  `findings.rs` track per-work state (check items, review findings) keyed by canonical work
  name (`kiln`, `kiln-fix1`, … are one work; see `link_matches`).
- `launch/` — prompt assembly, per-backend launch scripts, skill materialization.
- `role/`, `plugins/`, `plugin/` — role catalog (core + plugin roles), plugin resolution,
  trust, and per-Run snapshots.
- `session/tmux`, `transcript.rs`, `liveness.rs` — sessions and how the CLI observes them.
- `env/` — execution environments (containers) a Dispatch's build/test commands run in.

**One contract to not break:** a review that returns a negative verdict is a *completed*
Dispatch and settles `succeeded`; `failed` means the work itself did not happen
(`assets/core/role-protocol.md`). Gate verdicts (PASS/FAIL from findings) are separate from
settlement outcomes.

## Testing

Integration tests live in `tests/*.rs` and drive the public seam — `execute(cli, env,
catalog)` — with `TestEnvironment`, a tmux stub, `TempRepo` and an in-memory role catalog
(`tests/common/mod.rs`, `install_pottery_plugin`). No test launches a real agent, tmux or
model; fixtures under `tests/fixtures/`. Pure logic (parsing, gate evaluation) is unit-tested
in `#[cfg(test)]` inside the module. New user-visible behavior needs a test and a
`CHANGELOG.md` entry under `[Unreleased]`.

## Repo rules

- **Public repository.** Never commit credentials, machine names, cluster contexts or private
  repository paths; everything pushed stays public.
- Every `.rs`/`.sh`/`.py` file starts with the SPDX MIT header (two lines, `Copyright (c)
  2026 oat-agents contributors`) — `tests/fixtures/` exempt.
- Branch off `main`; spec work lands on `epic/S<n>-<slug>` via one PR per ticket, the epic
  reaches `main` in one person-merged PR. Conventional Commits (`feat:`, `fix:`, `docs:`, …).
- Every commit needs a DCO `Signed-off-by` line — commit with `git commit -s`.
- Never delete or rewrite an ADR that has reached `main`.
