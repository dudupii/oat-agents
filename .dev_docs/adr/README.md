# Architecture Decision Records

A record here captures **one** decision that is hard to reverse, is the product of a real
trade-off, and would confuse a newcomer without its context. All three, or it does not belong
here. Every record states what it costs.

## What does not belong here

| | Where it goes |
| --- | --- |
| A fact that moves with the version — command set, file formats, flags | the documentation beside the code |
| Vocabulary | `.dev_docs/CONTEXT.md` |
| Working rules for agents | `AGENTS.md` |
| Filenames, function names, test names | Nowhere. The code is the record. |

## Amendments

A record that has reached `main` is never deleted. When something changes, there are three
endings:

- **The decision still stands, but its terms changed.** Rewrite the record in place so it
  reads as the decision holds today. If the rewrite follows from another record, cite it.
- **The decision was reversed.** Write a new record. The old one gets *one line* under its
  title: `Status: superseded by ADR-NNNN.` The reason belongs in the new record.
- **It was never a decision matter.** Move it where it belongs. The record is untouched.

A record written on an epic branch is a proposal until that branch merges, and may be
rewritten freely until then.

Cite a record by number, never by filename. `.github/scripts/check-dev-docs.sh` fails when a
citation does not resolve or a record is missing from the list below.

## Current list

| ADR | Decision |
| --- | --- |
| ADR-0001 | The core ships only `oat-meta` and `oat-console`; every other role comes from a plugin |
| ADR-0002 | A plugin is a directory with one directory per role |
| ADR-0003 | Plugin content lands only in the Dispatch's worktree |
| ADR-0004 | The CLI owns the agents and how the workflow runs; plugins own how a team works |
| ADR-0005 | A console belongs to one repository |
| ADR-0006 | A role's concurrency limit is a plugin default the repository overrides, and the CLI queues past it |
| ADR-0007 | A role's backend and model are plugin defaults the repository overrides |
| ADR-0008 | A git plugin may follow its latest commit |
| ADR-0009 | The rework loop's bounds are a plugin default the repository overrides |
