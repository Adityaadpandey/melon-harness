---
name: review-commits
description: Reviewing one commit or a range of commits, each judged on its own and as a series
triggers: review commit, review the commit, review commits, review the commits, last commit, last commits, latest commit, commit range, since main, since yesterday, review sha
---
# Reviewing commits
Pin the range first, once: `git log --oneline <base>..HEAD` (or the named sha / "last 3" = `HEAD~3..HEAD`). Say which commits you are reviewing.
- One commit: `git show --stat --patch <sha>`.
- A series: `git log -p --reverse <range>` reads in the order the author wrote it, so a later commit that fixes an earlier one is obvious. Judge the end state with `git diff <base>...HEAD`, not each step in isolation.

Per commit ask: does the message match the diff (a "fix typo" that edits a query is a finding), does it build and pass on its own (a broken middle commit breaks `git bisect`), and is unrelated work bundled in? A revert plus a re-apply, or a fixup that belongs squashed into its parent, is worth one line. Example: commit 3 renames `getUser` and commit 5 updates the last caller; commits 3 and 4 do not compile, so say that.

Merge commits: review what the merge resolved (`git show --cc <sha>`), not the whole merged branch. Never amend, rebase or rewrite history while reviewing. Combined with another playbook: one list, each item tagged with its lens and the sha it belongs to.
