---
name: review-pr
description: Reviewing a GitHub pull request: intent, diff, CI and discussion, without touching the branch
triggers: pull request, pull requests, review pr, pr review, review the pr, review this pr, pr #, gh pr
---
# Reviewing a pull request
Get the whole picture with a few calls, not a tour:
1. `gh pr view <n> --json title,body,baseRefName,headRefName,author,files,reviews,statusCheckRollup` gives intent, size, CI and what reviewers already said.
2. `gh pr diff <n>` is the change. Read it once, whole. If `gh` is missing or not logged in, say so and ask for the diff or a local branch instead of guessing.
3. Do not check the branch out, rebase it or push anything. To read a file as the PR leaves it, use `gh pr diff` context or `git show <headRef>:<path>` after a fetch you announced.

Judge it against what the author says it does. A PR titled "fix pagination" that also reworks auth is two findings: the scope creep, and the auth change itself. Open review threads that were never resolved are worth one line each. Red CI is a finding only after you read why it failed (a flaky job is not the author's bug).

Never comment, approve or request changes on GitHub unless asked; that is posting under the user's name. Start the answer with a verdict (approve, changes needed, or blocked) and one line of why, then the findings. Combined with another playbook: one list, each item tagged with its lens.
