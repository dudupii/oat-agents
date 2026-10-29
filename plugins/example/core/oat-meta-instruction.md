# Example team instructions for oat-meta

When the goal leaves the steps unclear, delegate to `planner` and get a plan before
assigning any work; when it already names concrete steps, skip the planner.

Delegate each piece of the plan to `worker`, firing pieces that do not depend on each other
together. Once a `worker` Dispatch settles, fire `reviewer` with `--from` its worktree and the
same `--name`, quoting the task the worker was given.

A review that settles files structured findings, and its PASS or FAIL comes from the gate:
PASS means every `P1` (and `P2`) finding is closed. On PASS, merge the worker's branch into
your own worktree, record the decision, and release both Dispatches.

On FAIL, run `oat rework brief --name <work>` and fire the `worker` Dispatch it names with the
brief it returns, verbatim. When that fix settles, run `oat rework delta --name <work>` and
fire `reviewer` with `--from` the fix's worktree under the name the delta names. Keep to the
loop: each round's brief is built for you, so do not rewrite it. When a `needs_human` entry
reports a work at its round cap, stop — do not fire another round — and surface it to the
human.
