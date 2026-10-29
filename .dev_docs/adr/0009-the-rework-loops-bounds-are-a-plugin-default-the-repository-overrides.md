# ADR-0009 — The rework loop's bounds are a plugin default the repository overrides

*Status: in force.*

A failing review starts a rework loop: the meta-agent fires a fix against the findings, a
reviewer reviews the fix, and rounds repeat until a review passes. Without a bound the loop
can run forever, burning a worktree, an agent session and often an execution environment
each round. The bound — how many rounds a work gets, and whether P2 findings must close — is
a team's policy, which ADR-0004 gives to plugins.

But the CLI cannot leave the bound in prose alone. It counts the rounds, evaluates the gate
against the findings, and when a round fails at the cap it is `dispatch done` — a command,
not an instruction — that must tell the meta-agent to stop and surface the work to the
operator. A number a machine enforces has to reach the machine, or the gate and the
meta-agent's instructions drift apart. ADR-0006 already resolved this exact shape for
concurrency limits: a plugin default, a repository override, settled once at `meta fire`.

**Decision.** A plugin sets the rework loop's bounds in `[rework]` of its
`core/oat-meta.toml` (`max_rounds`, `p2_required`) — the defaults for every work the teams
it serves run. Only `core/oat-meta.toml` may set it; the rework loop is the meta-agent's to
run, so it has one owner like any core-role setting, and two plugins setting it is refused
at load naming both. A repository overrides either field in `.oat/rework.toml`. `meta fire`
settles the effective settings into the Run record once, and the round cap, the gate and the
escalation read the settled record, never the files. A work gets at least one round
(`max_rounds` is never below 1).

**Rejected.**
- Bounds in the meta-agent's instruction text only, ADR-0004 read strictly. The policy is
  indeed the plugin's, but the CLI enforces the cap mechanically and escalates through the
  Run inbox; a bound that lives only in prose cannot be what the gate counts without the two
  drifting apart.
- Bounds as `meta fire` flags. ADR-0004 rejects coordination policy carried by that command,
  and an operator firing a Run should not re-state a team's review policy every time.
- Repository only. A plugin could not ship a sensible default for teams it knows need
  tighter bounds; every repository would repeat the same numbers.
- Plugin only. The plugin author cannot know how expensive a review round is in a given
  repository.

**Consequences.**
- Changing `.oat/rework.toml` or the plugin's default affects the next Run, never one
  already running.
- A repository can tighten or loosen the plugin's numbers field by field, but cannot give a
  work zero review rounds.
- The meta-agent's instructions still own how the loop is run — when to fire a fix, what to
  quote from a rework brief. The CLI owns only the count and the verdict rule, and what
  those mean is documented beside the code, not here.
- ADR-0004's consequence that `meta fire` carries no coordination policy stands; this record
  is the one place a bound crosses into the CLI, and it crosses as data the CLI reads, not
  as policy the command carries.
