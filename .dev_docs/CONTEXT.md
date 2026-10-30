# oat-agents

A command-line tool that runs a supervised team of coding agents on one machine: a
meta-agent takes one goal and delegates bounded work to member agents, each in its own
git worktree and tmux session, settling the results through a file-based inbox. The core
supplies the mechanism and two roles; plugins supply every other role and the way a team
works.

## How to read this glossary

A word earns a place here only if getting it wrong causes a concrete mistake. Every *Avoid*
entry carries the reason it is dangerous. A word is defined once; a second definition is a
defect.

## Language

### Work

**Goal**
The one input a Run is started from: what is to be done, written for the meta-agent.
`meta fire` takes it; `goal show` reads it back.
*Avoid*: **task list** — the meta-agent decides the steps; a goal states the outcome.
*Avoid*: **Big Plan**, **objective** — earlier names for the goal. Two names read as two
inputs, and Runs recorded before the rename still carry it as `big_plan`, so a reader that
looks only for `goal` misses them.

**Run**
One execution of one goal, from `meta fire` until the meta-agent finishes it. Owns its
Dispatches, its Run inbox and its workflow log.
*Avoid*: **job**, **session** — a session is one agent's terminal; a Run spans many of them.

**Dispatch**
One launch of one role inside a Run: one agent session, one worktree, one settlement.
*Avoid*: **task** — several Dispatches can serve the same piece of work (a correction round
is a new Dispatch), and conflating them loses which attempt said what.

**Work**
One piece of the goal, identified by what its Dispatches fire under: `kiln`, `kiln-fix1` and
`kiln-fix2` are one work. A work's checklist links and review findings are recorded under its
canonical name, the shortest of those.

**hash_id**
The stable identifier of one agent execution, `<role>-<4 hex>-<name>`, quoted in every log
line about it. `<name>` is what the Dispatch is for — its `role fire --name`, or the branch of
the worktree it starts in — and the Run's name only for the meta-agent and for a launch that
named nothing.
*Avoid*: deriving a hash_id anywhere but from the Dispatch's record — a copy that uses a
different name looks for a session that does not exist.

**Queued launch**
A `role fire` for a role already at its concurrency limit in the Run, waiting for a place. It
already has the id its Dispatch will carry, but it is not a Dispatch yet: it has no worktree and
no session, and the CLI launches it when a place comes free.
*Avoid*: firing it again — it will start on its own, and a second fire queues a second copy of
the same work.

**Settle**
What an agent does to end its own Dispatch with an outcome and a report. A Dispatch that is
not settled is not finished, whatever its terminal shows.

**Finding**
One problem a review round filed against a work: its id, its severity (P1 must fix, P2 should,
P3 cosmetic), its file and line, and the fix it requires. Findings carry across a work's
rounds, merged by id — a re-review files the same finding again with its new status.
*Avoid*: **issue** — an issue is the repository's, not a round's.

**Review round**
One review of one work: the findings its reviewer filed, the verdict the gate derived from
them, and the commit the review saw.

**Gate**
The rule that turns a round's findings into its verdict: PASS only when no finding is open —
every P1 fixed, never waived; every P2 fixed while the Run requires it; a P3 fixed or waived
with a note.
*Avoid*: **the reviewer's verdict** — a reviewer files findings; the gate, not the reviewer,
says PASS or FAIL.

**Rework loop**
The rounds a failed gate sets off: `rework brief` hands the worker the original task and the
open findings, `rework delta` hands the reviewer the findings and only the fix diff, until the
gate passes or the work reaches its round cap and is escalated to the human (`needs_human`).
*Avoid*: **correction** — the earlier name for a fix round. The cap is the Run's
(`max_rounds`), not a count the meta-agent keeps.

### Communication

**Run inbox**
The Run's queue of messages from its member agents to the meta-agent — questions, escalations and
completion reports. It has exactly one consumer: that Run's `oat-meta`.
*Avoid*: **reading the inbox** for anything that merely observes — a delivery is handed over
once, and an observer that takes one leaves the meta-agent waiting for a message that is
already gone. Observers read the workflow log.

**Delivery**
One batch handed to the meta-agent by `run wait`, acknowledged as a whole.

**Workflow log**
The append-only record of everything that happened in a Run, written by the CLI and by the
meta-agent's decisions. The only place an observer reads a Run from.

### Roles

**Role**
A named kind of agent: its instructions, the skills it may load, and its model settings.

**Core role**
A role the core ships and owns: `oat-meta` and `oat-console`. Only core roles carry the
`oat-` prefix.

**Plugin role**
Any role supplied by a plugin, such as `planner`, `worker` or `reviewer` in the example
plugin. The core knows nothing about what a plugin role does.
*Avoid*: assuming a fixed Planner → Worker → Reviewer set in the core — which roles exist is
a plugin's decision.

**oat-meta**
The core role that runs each Run's meta-agent.

**Meta-agent**
The agent of a Run's `oat-meta` Dispatch. It delegates the goal's work to member agents, is
the only consumer of the Run inbox, and finishes the Run; it does not do the goal's work
itself.
*Avoid*: **coordinator** — it names a job, not an agent. A plugin role can coordinate too,
and the word does not say which agent alone may consume the Run inbox.

**Member agent**
The agent of a Dispatch of a plugin role, launched with `role fire`. It works in its own
worktree and settles its own Dispatch. Every agent in a Run is either its meta-agent or a
member agent, never both; `oat-console` belongs to no Run and is neither.
*Avoid*: **subagent** — a member agent runs as its own session in its own worktree, not
inside the meta-agent's turn; treating it as a subagent leads to relaying work that was
meant to be delegated.
*Avoid*: **worker** — a role of the example plugin. The settlement message was once named
`worker_done` though every member agent sends it, which read as only that role having
finished.

**oat-console**
The operator's system console. It belongs to no Run and observes all of them; it never
consumes a Run inbox and never settles or releases a Dispatch.

### Extension

**Plugin**
A unit that supplies roles, skills and conventions to the core without changing it. Its
format is decided by the first spec.
*Avoid*: **customization** for changes made to the core itself — a customization that
needs a core edit is a missing plugin hook, and naming it so is how the hook gets built.

**Skill**
A versioned instruction file an agent loads on demand, synchronized to the selected
backend's skill directory before a launch.

**Backend**
The agent program a session runs: currently Claude Code or Codex.
*Avoid*: **runtime** — it reads as where a Dispatch's commands run, which is the execution
environment, not the program that runs the agent.

### Execution

**Worktree**
A git worktree the core creates for one Dispatch, branched from its meta-agent's worktree,
or, for a role that starts in an existing worktree and is fired with `--at`, from that commit.

**Execution environment**
A container, described by an execution profile, in which a Dispatch's build and test
commands run instead of the host.
On Kubernetes it is one pod, and the operator-facing surfaces (the live view,
`docs/exec-environments.md`) call it that. A role that asked for one and ran on the
host is reported as such, never left to be inferred.

**Execution profile**
A named description of how a machine provides execution environments. The repository names
the profile it expects; the machine says how it provides it.

## Words not to use

| Word | Why |
|---|---|
| **agent** alone, where a role or a Dispatch is meant | "The agent failed" does not say whether a role is wrong or one attempt failed. |
