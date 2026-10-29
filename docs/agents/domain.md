# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- `.dev_docs/CONTEXT.md` — the vocabulary. Use its terms in code, tests, issues and PRs; the *Avoid* lines matter more than the definitions.
- `.dev_docs/adr/` — every decision record touching the area you change. Cite records by number (`ADR-0001`), never filename; `.github/scripts/check-dev-docs.sh` fails when a citation does not resolve.

This repo's rule is stronger than "read if present": CLAUDE.md makes both mandatory before changing anything, and a change that contradicts an ADR stops and says so rather than overriding silently.

## Use the glossary's vocabulary

When your output names a domain concept (an issue title, a refactor proposal, a test name), use the term as defined in `.dev_docs/CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007, but worth reopening because…_
