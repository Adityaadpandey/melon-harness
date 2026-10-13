---
name: code-review
description: Review a change for correctness, callers, security and missing cases, with evidence
triggers: review, audit, critique, look over, check this, pull request, pr, diff
---
# Code review
Report only problems you can point to. For each: `file:line`, what is wrong, why it matters, and the evidence you read. Check: does the change do what was asked (and nothing else); callers and contracts it could break (read them); error paths and edge cases; auth, tenant scoping, input validation, secrets; concurrency, idempotency, transactions; tests that cover the change. No style nitpicks, no speculation, no praise. If nothing is wrong, say "no issues found" and list what you checked.
