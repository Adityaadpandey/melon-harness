---
name: review-changes
description: Reviewing the user's current uncommitted or staged work before they commit
triggers: uncommitted, my changes, current changes, the changes we made, working tree, staged, this change, what i changed, before i commit, before committing, before commit
---
# Reviewing the current change
Scope is the working tree: `git status -sb` then `git diff HEAD --no-color` (staged and unstaged together). "Staged" alone means `git diff --cached`. Untracked files are in `git status` but not in the diff: read the new ones you were not shown, they are part of the change.

The question is "is this safe to commit": 
- leftovers: debug prints, `console.log`, `TODO`, commented-out blocks, a stray `.only` in a test, a hard-coded local path or key;
- half-done work: a function renamed in one file and not the next, a new flag nobody reads, a migration without its model change;
- files that should not ship: `.env`, build output, lockfile churn nobody asked for;
- the change itself, through the code-review rules.

Run what the repo already runs before commit (the typecheck or the tests that cover the touched files) and say what you ran. Do not stage, commit, stash or discard anything while reviewing; if the user wants it committed, that is a separate step. Combined with another playbook: one list, each item tagged with its lens.
