---
name: code-review
description: Review a change for correctness, callers, security and missing cases, with evidence
triggers: review, audit, critique, look over, check this, pull request, pr, diff
---
# Code review
Report only problems you can point to. For each: `file:line`, what is wrong, why it matters, and the evidence you read. Check: does the change do what was asked (and nothing else); callers and contracts it could break (read them); error paths and edge cases; auth, tenant scoping, input validation, secrets; concurrency, idempotency, transactions; tests that cover the change. No style nitpicks, no speculation, no praise. If nothing is wrong, say "no issues found" and list what you checked.

## Scope first, once
Start with ONE call: `git op=scope`. It gives the branch, the uncommitted files and the branch commits over its base. Tell the user which of these you review, then do not re-run status, log or diff to rediscover it. "My uncommitted changes" means only the working tree. "The changes we made" means the working tree plus the branch commits over its base. A named commit, PR or file is the whole scope.
## Depth follows the change
Read the diff once (`git op=diff path=<file>` for a file you need in full). For each changed function, state the claim to check, then read that function and only the callers or callees that confirm or refute it. Do not tour files that merely mention a symbol, and ignore content outside the diff. A focused diff (about 5 files, 200 lines) usually needs 12-15 tool calls; go past that only for a named, unresolved risk, and say which. Auth, privacy, payments, cache isolation and migrations justify more.
## Each finding
Severity, confidence, `file:line`, how it happens, impact, the fix or test, and a status: verified (you read the code that proves it), likely, or unverified. Never call a guess a confirmed bug. If a check was not run, say so.
