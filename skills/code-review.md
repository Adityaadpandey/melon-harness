---
name: code-review
description: Review a change for correctness, callers, security and missing cases, with evidence
triggers: review, audit, critique, look over, check this, pull request, pr, diff
---
# Code review
Report only problems you can point to. For each: `file:line`, what is wrong, why it matters, and the evidence you read. Check: does the change do what was asked (and nothing else); callers and contracts it could break (read them); error paths and edge cases; auth, tenant scoping, input validation, secrets; concurrency, idempotency, transactions; tests that cover the change. No style nitpicks, no speculation, no praise. If nothing is wrong, say "No issues found" and nothing more.

## Scope first, once
Start with ONE call (`git op=scope`, or `git status -sb && git diff --stat` in a shell). It gives the branch, the uncommitted files and the branch commits over its base. Tell the user which of these you review, then do not re-run status, log or diff to rediscover it. "My uncommitted changes" means only the working tree. "The changes we made" means the working tree plus the branch commits over its base. A named commit, PR or file is the whole scope.
## Depth follows the change
Read the whole change with ONE call (`git op=patch`, or `git diff --no-color` in a shell; add `<base>...HEAD` for branch commits); never one diff call per file. Re-read a single file's diff only if the output cut it. For each changed function, state the claim to check, then read that function and only the callers or callees that confirm or refute it. Do not tour files that merely mention a symbol, and ignore content outside the diff. A focused diff (about 5 files, 200 lines) usually needs 12-15 tool calls; go past that only for a named, unresolved risk, and say which. Auth, privacy, payments, cache isolation and migrations justify more.
## Trace what flips it
Type checks and lint pass on most regressions, so they prove little. For every new or changed condition, flag or gate in the diff (disabled, loading, isFetching, a cache key, a permission check), find everything that changes it at runtime and say what the user sees then. Example: a button disabled while `isFetching` is true, and every like mutation invalidates that query's tag: the button dims after every tap. Grep the mutations, invalidations and refetches that touch it, read them, and judge the visible result. This is the check that finds bugs; do it before running tsc or lint.
## Trace what fails
For every new awaited call or write the diff puts on a request path (a DB insert, a cache write, an outbound fetch), ask what the user gets when it throws. If the data was optional (share links, analytics, badges) and the failure now 500s the whole response, that is a finding: catch it and fall back. A feature that moved onto more pages widens an old risk; say so.
## Read what the diff deleted
Before saying a change "has no trigger" or "never runs", read the lines it removed: the deleted effect, refetch or handler is what the author replaced, and the new code usually relies on a library doing that work. Example: a deleted `useEffect(() => refetch())` next to a query whose `skip` now includes `!isAuthenticated`: RTK Query starts a skipped query by itself when `skip` turns false, so nothing is missing. A claim about how a library behaves that you did not read in its docs or in `node_modules` is `unverified`.
## Severity needs evidence
HIGH is for defects you verified by reading the code that proves them. A guess about library or runtime behaviour is at most MEDIUM and carries `unverified` plus what would confirm it. Never repeat one guess as a second finding.
## Settle it or drop it
An unverified finding that one grep or file read could settle (an id's format in the schema, a prop's type, a caller) gets that lookup before you write it. If it stays unverified and is below MEDIUM, leave it out. Redundancy, "could drift later" and style are not findings.
## Decide before you write
Think first, then list. A suspicion you checked and cleared never appears in the answer, and a severity label is never put on an item you conclude has no defect: no "non-issue", "acceptable", "skip" or "verified fine" items, and no finding without a defect you can point to.
## When asked to fix
"Review and fix" means: the first verified finding gets the smallest fix right away (edit, then one check), and the findings you only state stay stated. Do not keep reading for a second finding before fixing the first, and do not end with "no issues" until you traced each changed gate above.
## Each finding
Severity, confidence, `file:line`, how it happens, impact, the fix or test, and a status: verified (you read the code that proves it), likely, or unverified. Never call a guess a confirmed bug. If a check was not run, say so.

## Answer format (this is what the user reads)
Findings only, most serious first, at most 6. One finding is at most two lines: `severity file:line  what is wrong -> the fix`, with `(unverified)` after the severity when you did not read the code that proves it. The first characters of the answer are the first finding: no intro sentence, no remarks about rounds, budgets or output files, no restating the scope, no "what I checked" list, no praise. Add ONE closing line only when something matters: a check you could not run, or "no issues found". Never explain how the harness or your budget behaved. Aim for under 150 words.
