# The review pipeline's golden eval

The worker pipeline has a golden eval; the review pipeline deserves the same. Heterogeneous
review's premise is that a reviewer on another backend catches reasoning-level defects a
green test suite cannot — the field evidence was a production auth bypass (an id token
accepted without verification, P1) in a change whose suite was fully green. This eval keeps
that premise honest: it replays an auth-bypass-shaped diff through a real reviewer Dispatch
and requires the P1 to be caught.

This is a **human-run eval, not CI**: firing the reviewer Dispatch consumes a real model
session. Nothing here runs in `cargo test` or CI, on purpose.

## The fixture

Written from scratch for this eval; every name in it is fictional.

- `fixture/base/` — a small TypeScript repository. `src/oidc.ts` is the acceptance path:
  `signIn()` verifies the id token through an injected `TokenVerifier`, then checks issuer,
  audience and expiry before trusting its claims. The suite runs on Node 22.18+/23+
  (type stripping): `npm test`.
- `fixture/defect.patch` — the change under review. It stops calling the verifier — the
  parameter stays, the call goes — and reads the claims straight off the token, behind a
  comment that rationalizes the shortcut. Nothing is verified: not the signature, not the
  issuer, not the audience, not the expiry.

The suite is green on both states, by construction: it stubs the verifier and exercises
only the happy path. That is the point of the eval — the defect is invisible to tests, so
a reviewer that only re-runs them finds nothing, and only reading the change does.

## Procedure

Run from your `oat-agents` checkout, with `oat-agents` built, `codex` installed and
authenticated, and tmux available. `REPO` below is that checkout.

1. Build the fixture repository: base on `main`, the defect as one commit on a branch, and
   a worktree of that branch for the reviewer to sit in.

   ```sh
   git init ~/signin-eval && cd ~/signin-eval
   cp -r "$REPO"/evals/review/fixture/base/. .
   git add -A && git commit -m "the sign-in path before the change"
   git checkout -b worker/oidc-signin
   git apply "$REPO"/evals/review/fixture/defect.patch
   git add -A && git commit -m "feat: sign in from the OIDC callback's id token"
   git checkout main   # the branch is checked out here; free it for the worktree
   git worktree add ~/signin-worker worker/oidc-signin
   npm test   # green, defect and all — that is the point
   ```

2. Wire the repository to oat-agents: pin the embedded example plugin (it supplies the
   `reviewer` role) and put the reviewer on codex.

   ```sh
   oat-agents init
   mkdir -p .oat
   cat > .oat/roles.toml <<'EOF'
   [reviewer]
   backend = "codex"

   [reviewer.model.codex]
   model = "<a model your codex backend offers>"
   EOF
   ```

   The model name is whatever your `codex` actually serves (see its own config) — a name the
   backend rejects kills the session with a stream error partway through.

3. Start the Run and fire the reviewer Dispatch at the fixture worktree. `role fire` runs
   inside a Run, hence `OAT_RUN_ID`; `--no-exec` because this review is pure reasoning by
   design — the defect is not one a mechanical re-run would surface.

   ```sh
   oat-agents meta fire --repo "$(pwd)" --name review-eval --no-exec \
     --prompt "An operator is driving this Run by hand for an eval; wait, and fire no member agents."
   export OAT_RUN_ID=<run_id from that output>
   oat-agents role fire reviewer --from ~/signin-worker --name oidc-signin \
     --input-file "$REPO"/evals/review/brief.md
   ```

   Attach to the tmux session the `role fire` output prints to watch the reviewer work. It
   settles its own Dispatch with `dispatch done`, passing its findings as
   `--findings-file`.

4. Check the round. The work's findings record lives under the Run's state
   (`$OAT_AGENTS_STATE_DIR/runs`, else `$XDG_STATE_HOME/oat-agents/runs`, else
   `~/.local/state/oat-agents/runs`):

   ```sh
   python3 "$REPO"/evals/review/check.py \
     ~/.local/state/oat-agents/runs/$OAT_RUN_ID/findings/oidc-signin.json
   oat-agents meta finish --run "$OAT_RUN_ID"
   ```

   `meta finish` removes the Run's Dispatch worktrees, `~/signin-worker` among them — rebuild
   it (step 1's last two commands) before re-running the eval.

## Pass condition

At least one `P1` finding citing `src/oidc.ts` with a line (`file:line`). `check.py` exits
0 only then; it also accepts the raw JSON array the reviewer filed, if you kept it. A round
that files the bypass as a P2 or P3, cites the wrong file, omits the line, or files
nothing, fails.

## Reading a failure

A miss is a fact about the pipeline — the reviewer's contract, the backend pairing, or the
model — not a prompt to tune the fixture until a specific configuration passes. Record the
date, backend and model, and file the gap; the fixture changes only to fix an admitted
flaw (a defect a test could catch, a suite not green on both states), never to make a
failing run pass.
