# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A first-class rework loop around review. A review settles with structured findings
  (`dispatch done --findings-file`), each carrying an id, a severity (`P1`/`P2`/`P3`), its
  file and line, and the fix it requires; the gate derives the round's PASS or FAIL from them
  while the Dispatch still settles `succeeded` — a failing review is a completed review. A
  re-review merges its findings into the work's record by id, so a finding's status carries
  across rounds. `oat rework brief` hands the worker the original task (recorded verbatim per
  Dispatch) plus the open findings, `oat rework delta` hands the reviewer the findings and
  only the fix diff, and `oat rework show` reports where a work's gate stands. Rounds are
  capped — `max_rounds`, default 3, and `p2_required`, the meta-agent plugin's defaults
  (`[rework]` in its `core/oat-meta.toml`) overridden field by field by `.oat/rework.toml`,
  settled into the Run at `meta fire` — and a work still failing at its cap is escalated with
  a `needs_human` entry instead of looping. The example plugin's reviewer now files findings
  and its meta-agent works the loop.
- A golden eval for the review pipeline, `evals/review/`: a synthetic TypeScript fixture
  (a base repository plus `defect.patch`) whose change accepts an OIDC id token without
  verifying it while its test suite stays green on both states, a procedure for replaying
  that diff through a reviewer Dispatch on the codex backend (`[reviewer]` in
  `.oat/roles.toml`, settled with `dispatch done --findings-file`), and `check.py`, which
  passes only when a P1 finding cites the acceptance path's `file:line`. Human-run with a
  real model, deliberately not CI.
- Contributor documentation for the public release: `CONTRIBUTING.md`, `SECURITY.md`,
  `CODE_OF_CONDUCT.md`, issue and pull request templates, `CODEOWNERS`, and Dependabot
  configuration. CI now also runs on pull requests.
- An SPDX license header naming the oat-agents contributors on every source file, checked in CI.
- The example plugin's team works a change through to the meta-agent's branch: `oat-meta` skips
  the planner when the goal is already concrete, fires independent pieces together, reviews
  from the worker's worktree, merges a passed branch into its own worktree, hands a correction
  the failed branch and the reviewer's findings, and raises `needs-human` after two failed
  corrections. The planner marks each step's check and dependencies, the reviewer leaves the
  worker's worktree untouched and lists each problem with its file and line, and the console
  names why a stuck Run is stuck.
- A Traditional Chinese README, `README.zh-TW.md`, linked from the top of `README.md`.
- Every subcommand has a `--help` description.
- `meta fire` and a launched `role fire` report the agent's tmux `session` and the exact
  `attach` command for it. Agents run on a private tmux server, where a plain `tmux attach`
  does not find them.

### Changed

- The example plugin's `branch-naming` skill is replaced by `committing`. Its rule had a worker
  switch to a `<role>/<slug>` branch, leaving the Dispatch's own branch — the one the
  meta-agent merges and cleanup checks — without the work.
- The Big Plan is now the **goal**: `big-plan show|list` is `goal show|list` and prints the key
  `goal`. `big-plan` still works as a hidden alias, and Runs and logs recorded with `big_plan`
  still show their goal.
- The Run inbox's settlement message is `member_done` instead of `worker_done`, since every
  member agent sends it. An inbox holding `worker_done` still reads. A plugin whose
  instructions name `worker_done` should say `member_done`; finish open Runs before upgrading,
  since their meta-agent was told the old name.
- The words follow `.dev_docs/CONTEXT.md` throughout: `oat-meta` runs a Run's **meta-agent**,
  plugin roles run **member agents**, and "coordinator" is retired from prompts, skills, help
  text and docs.

### Fixed

- `cargo clippy` on the current stable toolchain passes again, so CI on `main` is green.
- The live view's log page no longer tells a Codex agent that Claude Code's session directory
  is not reachable; it names the agent's own backend.

## [0.1.0]

### Added

- Initial public version: `oat-agents` launches a Big Plan with `oat-meta`, which delegates
  Dispatches of plugin-defined roles, each in its own git worktree and tmux session, and reads
  their results from the Run inbox. Includes the live view (`oat-agents tui`), the `oat-console`
  core role, the embedded `example` plugin, git-sourced plugins with per-machine trust, Claude
  Code and Codex backends, optional Kubernetes execution environments, and `install.sh`.
