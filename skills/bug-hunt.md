---
name: bug-hunt
description: Finding bugs nobody reported yet: hostile inputs, state, races, boundaries
triggers: find bugs, find bug, look for bugs, hunt for bugs, bug hunt, any bugs, potential bug, potential bugs, what could break, what can break, edge cases, race condition, off by one, hidden bug
---
# Finding bugs before they are reported
Pick the code path first (the module the user named, or the diff), then attack it with a fixed set of questions instead of reading top to bottom:
- Inputs: empty, null, one item, huge, duplicate, wrong type, unicode, a negative number, a date at midnight or a timezone edge.
- Boundaries: `<` vs `<=`, first and last element, pagination page 0 and the last page, integer overflow, a limit of zero.
- State: what runs twice (retries, double clicks, webhooks delivered twice), what runs out of order, what is read after it was changed.
- Concurrency: two requests touching the same row, check-then-act without a lock or constraint, a cache filled while being invalidated.
- Failure: what each awaited call does when it throws or times out, and whether half-finished work is left behind.
- Contract drift: a caller assuming a field that the callee stopped returning.

Example: `if (balance - amount >= 0) { balance -= amount }` reads fine, but two parallel requests both pass the check. That is a bug you can state without running anything.

Report only what you can trace to lines, ranked by blast radius, each marked verified (you read the proof), likely, or unverified. When you can, write the failing test or command that shows it and run it; a reproduced bug beats a suspected one. If you found nothing after checking these, say which questions you checked. Combined with another playbook: one list, each item tagged with its lens.
