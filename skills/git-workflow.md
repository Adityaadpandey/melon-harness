---
name: git-workflow
description: Commits, branches and merges, only when the user asks
triggers: commit, branch, merge, rebase, conflict, push, stash, cherry-pick
---
# Git
Never commit, push, force-push, reset --hard or delete branches unless the user asked for that exact action. Check `git status` and `git diff` first so unrelated user changes are never swept in; stage files by name. Small commits, one concern each; message says what and why in plain words. Never use --no-verify, and never rewrite published history.
