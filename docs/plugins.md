# Writing a plugin

A plugin is how a team's own practice enters `oat-agents` without changing the core: its own
roles, the skills those roles may load, and the instructions the two core roles (`oat-meta` and
`oat-console`) follow for that team. The format below is fixed by ADR-0002 — once you publish a
plugin, changing its shape is a breaking change for everyone using it.

This guide's worked example lives in this repository at `tests/fixtures/sample-plugin/` (with a
second, minimal plugin at `tests/fixtures/sample-plugin-secondary/` used only to show how two
plugins combine). Every field below is used at least once there; when in doubt, read the
fixture alongside this guide. For a real, working plugin rather than a fixture built to exercise
every field, see `plugins/example/`: a planner, a worker, a reviewer and one skill, in the same
format.

## Layout

```
<plugin>/
├── oat-plugin.toml        format version, name, description
├── roles/<role>/
│   ├── role.toml          description, start location, execution environment,
│   │                      prior verification, concurrency limit, skills,
│   │                      per-backend model
│   └── instructions.md
├── skills/<skill>/
│   ├── SKILL.md           the standard skill file both backends read
│   └── …                  any supporting files; scripts keep their mode
└── core/                  instructions for the core roles
    ├── oat-meta-instruction.md      required from at least one plugin in a Run
    ├── oat-meta.toml                optional: skills and per-backend model
    ├── oat-console-instruction.md   required from at least one plugin in a Run
    └── oat-console.toml             optional
```

A role's name is its directory name under `roles/`, and is written nowhere else. A skill's name
is likewise its directory name under `skills/`. Every TOML file rejects unknown fields, so a
typo is refused at load time rather than silently ignored.

## `oat-plugin.toml`

```toml
format_version = 1
name = "sample-plugin"
description = "A worked example plugin covering every kind of customization the plugin format supports."
```

- `format_version` — this build understands version `1`. A plugin naming any other version is
  refused before any of its other files are read.
- `name` — the plugin's own name. It is not a role or skill name and never appears with an
  `oat-` prefix (that prefix is the core's).
- `description` — one line for a human reading a list of plugins.

## `roles/<role>/role.toml`

```toml
description = "Starts a fresh worktree, requires an execution environment, and produces its own prior verification."
start = "fresh"
exec_environment = true
prior_verification = true
max_concurrent = 2
skills = ["sample-skill"]

[model.claude]
model = "sample-claude-model"

[model.codex]
model = "sample-codex-model"
reasoning_effort = "high"
```

- `description` — one line for a human reading a list of roles; it is not part of a Dispatch's
  prompt.
- `start` — either `"fresh"`, a new child worktree is created for every Dispatch of this role,
  or `"existing"`, the Dispatch runs inside a worktree named at launch with `--from`, or, with
  `--at <commit>`, in a new worktree of its own at that commit. Naming the wrong one at launch
  is refused, not defaulted.
- `exec_environment` — whether a Dispatch of this role gets an execution environment (F2 says
  what that means at launch time; this plugin only declares whether the role wants one).
- `prior_verification` — whether a Dispatch of this role's prompt carries the execution
  ledger's prior verification of its worktree (again, F2's concern at launch time).
- `max_concurrent` — optional: how many Dispatches of this role one Run may run at once, at
  least `1`. Leaving it out means no limit. It is this plugin's default; a repository replaces
  it in `.oat/roles.toml` (see below). A Dispatch holds its place from launch until it is
  settled or released, and a `role fire` beyond the limit waits in the Run's queue until a
  place comes free.
- `skills` — the names of skills, from this same plugin's `skills/`, that a Dispatch of this
  role may load. A name with no matching `skills/<name>/` directory is refused.
- `[model.claude]` / `[model.codex]` — optional per-backend `model` and `reasoning_effort`.
  Leaving a backend out means that role uses the backend's own default when launched there.
  These are the plugin's defaults; a repository replaces them in `.oat/roles.toml` (see below).

`roles/<role>/instructions.md` is plain Markdown: the role's own working instructions, placed
in the prompt after the core role protocol and before the task. It may reference one of the
role's declared skills as `$skill-name` (see below).

## A repository's own role settings: `.oat/roles.toml`

A repository can replace, for any plugin role, the plugin's `max_concurrent`, and, for any role
including `oat-meta` and `oat-console`, the backend it runs on and its per-backend model:

```toml
[worker]
max_concurrent = 2
backend = "codex"

[worker.model.codex]
model = "gpt-5.5"
reasoning_effort = "high"

[reviewer]
max_concurrent = 1

[oat-meta.model.claude]
model = "opus"

[oat-console]
backend = "claude"
```

- `max_concurrent` — replaces the plugin's limit for that role. Core roles take none.
- `backend` — `"claude"` or `"codex"`. A plugin role's backend is, in order: `role fire
  --agent`, this, the Run's. The Run's is `meta fire --agent`, then this under `[oat-meta]`,
  then Claude Code. The console's is this under `[oat-console]`, then its plugins'
  `core/oat-console.toml`; `console open --agent` is used only when neither chooses one and is
  refused when it contradicts them.
- `[<role>.model.<backend>]` — `model` and `reasoning_effort` for launches of that role on that
  backend. Each one set replaces the plugin's; one left out keeps the plugin's.

`meta fire` settles all of it once — the repository's value where it names one, the plugin's
otherwise — and records it in the Run (`role_limits` and `role_settings` in its output), so a
change to either file affects the next Run, never one already running. The console reads the
file each time it is opened. A table naming a role no plugin of the Run supplies, an unknown
field, a limit of `0` or a limit on a core role refuses the Run with `role_limits_invalid`, and
an unknown backend with `role_settings_invalid`, before anything is created. A repository cannot
lift a plugin's limit to "none"; it can only set a number.

A `role fire` over the limit answers `"queued": true` with the Dispatch's id. The meta-agent's
own `run wait`, `dispatch release` and `role fire` start queued launches, oldest first, as
places come free, and the workflow log records `dispatch_queued`, `dispatch_dequeued`,
`queued_launch_failed` and `dispatch_dropped` (a launch still queued when the Run finishes).
A queued launch that fails when its turn comes is sent to the Run inbox as a `launch_failed`
message.

## The rework loop's bounds: `[rework]` and `.oat/rework.toml`

How hard a Run pushes a failed review before escalating to the human is a team's policy, so a
plugin supplies it — and how expensive a review round is differs per repository, so the
repository overrides it (ADR-0009, the same pattern as a role's limit above). The plugin's
defaults live in `[rework]` of its `core/oat-meta.toml`, because the rework loop is the
meta-agent's to run:

```toml
[rework]
max_rounds = 3
p2_required = true
```

A repository replaces either field in `.oat/rework.toml`:

```toml
max_rounds = 5
p2_required = false
```

- `max_rounds` — how many review rounds a piece of work gets (default `3`, never below `1`).
  When a round at the cap still fails its gate, the work is escalated: the workflow log records
  a `needs_human` entry and the settling review's Run inbox message tells the meta-agent to
  stop firing rounds and surface the work to the operator.
- `p2_required` — whether an open `P2` finding fails the gate (default `true`). With it off, a
  `P2` may be waived like a `P3`. A `P1` is never waivable, whatever these files say.

`meta fire` settles the plugin's defaults and the repository's overrides into the Run record
once, so a change to either file affects the next Run, never one already running. Two plugins
setting `[rework]` is a conflict naming both, like any core-role setting; `[rework]` in any
`core/` file but `core/oat-meta.toml` is refused. An unknown field, a `max_rounds` below `1`
or malformed TOML refuses the Run with `rework_settings_invalid`.

## `skills/<skill>/`

A skill is a directory. `SKILL.md` is required and is the same file either backend reads
directly if a person installs the skill by hand, outside `oat-agents`. Anything else in the
directory travels with it — a script, a data file — and keeps its file mode, so a script
skill's author marks it executable once and it stays that way in every Dispatch's worktree.

Skills refer to each other as `$skill-name`. Claude Code reads a plain name, so materializing a
skill for it rewrites every `$skill-name` in `SKILL.md` to `skill-name`; Codex reads the
reference as written, so nothing changes there. This rewrite is applied to `SKILL.md` only —
never to a script or another supporting file, since those are not prose and may contain a
literal `$` that is not a cross-reference at all (a shell variable, for instance).

## `core/`

The four files here carry the two core roles' team instructions, skills and model — the same
shape a plugin role's `role.toml` and `instructions.md` carry, minus `description`, `start`,
`exec_environment` and `prior_verification`, which are properties of a launched Dispatch that
the core roles do not have.

Across the plugins a Run loads, **at least one** must supply
`core/oat-meta-instruction.md`, and at least one must supply `core/oat-console-instruction.md`
— loading the whole set fails, naming the missing file, if none does. A single plugin need not
supply either; the worked example's secondary fixture plugin supplies only these two files and
nothing else. When several plugins supply the same core role's instructions, they are joined
with a blank line between them, in the order the plugins are listed. A skill or a per-backend
model declared for a core role by more than one plugin is a conflict, reported by name, not
resolved by picking one silently.

`core/oat-meta.toml` and `core/oat-console.toml` are optional and, when present, look like:

```toml
skills = ["sample-skill"]

[model.claude]
model = "sample-claude-meta-model"
```

`core/oat-console.toml` may also choose the backend the console runs on:

```toml
backend = "codex"   # or "claude"
```

A console is opened from no command that could choose one, so the repository's plugins do. When
none chooses, `oat-agents console open --agent <backend>` picks it, and with no `--agent`
either it is Claude Code; asking for a backend the plugins did not choose is refused rather than
silently obeyed or ignored. A repository's `.oat/roles.toml` chooses over its plugins (see
above). `core/oat-meta.toml` may not set `backend`: `oat-meta` runs on the Run's backend, chosen
by `meta fire --agent` or the repository. It is also the one file that may set `[rework]`, the
plugin's defaults for the rework loop's bounds (see above). Two plugins choosing the console's
backend is a conflict like any other.

## Validation

Loading a plugin never stops at the first problem: every malformed file, every missing file and
every declared skill with no matching directory is collected and reported together, so a plugin
author sees everything wrong with a plugin in one run rather than fixing one error at a time.
Combining several plugins into one Run works the same way — a role name two plugins both
define, and a core role's skill or model two plugins both supply, are each reported by the
plugin names involved.

## Where this fits

This guide covers the plugin format itself: what a directory has to contain to be a valid
plugin, and how several of them combine into the roles and core-role instructions a Run
launches. It does not cover where a Run's plugins come from, how they are pinned or trusted, or
`oat-agents init` — that is a repository's `.oat/plugins.toml` (a later ticket).
