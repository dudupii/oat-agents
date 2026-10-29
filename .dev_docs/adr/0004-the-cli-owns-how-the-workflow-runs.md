# ADR-0004 — The CLI owns the agents and how the workflow runs; plugins own how a team works

*Status: in force.*

Every agent in a Run is told two different kinds of thing. One kind is how to operate this
workflow: which Run the session is bound to, how to delegate, how the Run inbox is consumed
and acknowledged, what each liveness state means and what it calls for, how a Dispatch is
settled and released, how a Run ends, and how to read the state of the whole system. Get any
of it wrong and a Run stalls or loses a message, and it changes whenever the CLI changes. The
other kind is how a team does its work: which roles to use and in what order, how to name
things, what a finished piece of work must include, when to give up.

**Decision.** The CLI owns every agent — whether it exists, how it is launched, which Run it is
bound to, how it ends — and it supplies everything needed to operate the workflow: a preamble
generated for each launch, baseline instructions for `oat-meta` and `oat-console`, the
protocol every role follows to take part in a Run, and the skills for reading the system and
for running commands in an execution environment. It ships with the binary, always matches
it, and no plugin can remove or replace it. Everything about how a team works comes from
plugins: every plugin role's instructions, and the core roles' team instructions in
`core/oat-meta-instruction.md` and `core/oat-console-instruction.md`, which a Run's plugins
must supply (ADR-0002) and which follow the baseline in the prompt.

**Rejected.**
- Shipping no instruction text in the core and taking even the workflow's basics from
  plugins. It makes every plugin restate how the CLI works, and every CLI change a silent
  break in plugins that do not know about it.
- Shipping only a fact-only reference of the CLI and leaving what to do about its output to
  plugins. The CLI has to provide the basic instructions and skills that control its own
  workflow; without them no plugin can be written against a stable footing.
- Letting plugins append to or override one shared core instruction for coordination policy,
  such as a limit on review rounds passed by `meta fire`. Policy of that kind is a team's, not
  the workflow's.

**Consequences.**
- Every agent's prompt is, in order: the preamble, the core baseline for its kind (the
  core role's baseline, or the role protocol), the plugin's instructions, the task.
- The line between the baseline and a team's practice has to be drawn for every sentence the
  core ships. The test: would the workflow stall, lose a message or misread the system
  without it? Then it is baseline. Otherwise it belongs in a plugin.
- `meta fire` carries no coordination policy. A limit on retries or review rounds is a
  plugin's instruction or a goal's text. The rework loop's bounds are the one crossing: a
  plugin default the repository overrides, read by the CLI as data (ADR-0009).
