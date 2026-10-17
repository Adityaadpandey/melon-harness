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

Take extra care, a review is read by a teammate:
- Verify every finding against the code at the PR's head before you state it: quote the file:line, say what input or state breaks it, and drop what you cannot reproduce by reading. A wrong blocker costs the author more than a missed nit.
- Rank by impact: security and data loss (fail-open checks, dropped one-time secrets, missing authorization), then wrong behaviour, then fake or misleading UI/data, then style. Say which are blocking and which can follow.
- Check the neighbours of a change: other callers, the API type versus what the UI reads, the server select versus the client field.
- Keep each comment to one finding with a concrete fix, no praise padding, no restating the diff.

If `gh` cannot post (not logged in, no access), give the review as a script the user can paste: one fenced sh block, `PR=<n>` first, then one `gh pr review "$PR" --repo <owner/repo> --comment|--request-changes --body-file - <<'EOF'` heredoc per finding, bare commands only. The repo comes from `git remote get-url origin`, with its exact capitalisation.

Post to GitHub only when the user asks ("post it", "comment on the PR"); that is posting under their name. When they do, show the exact text once, then post it with `gh pr review` / `gh pr comment` (inline notes through `gh api repos/<owner>/<repo>/pulls/<n>/comments`), one command per finding, and report each URL. Start the answer with a verdict (approve, changes needed, or blocked) and one line of why, then the findings. Combined with another playbook: one list, each item tagged with its lens.
