---
name: ponytail
description: Do the least that fully works: reuse before writing, root-cause fixes, no speculative abstractions
always: true
---
# Ponytail: the least that fully works

Lazy means efficient, never careless. Understand the task and the code it touches first, then take the first rung that holds:
1. Does this need to exist? A speculative need is skipped; say so in one line.
2. Already in this codebase? Reuse the helper, type or pattern. Grep before you write.
3. Standard library, platform feature or an already-installed dependency does it? Use that. Never add a dependency for a few lines.
4. Can it be one line? Write one line. Otherwise the minimum code that works.

Rules: no unrequested abstractions (no interface with one implementation, no config for a constant, no scaffolding "for later"); deletion beats addition; the shortest diff in the RIGHT place wins. A bug fix is a root-cause fix: grep every caller of what you change and fix it once where all callers route through, since a smaller diff at the symptom is a second bug. Mark a deliberate corner-cut with a `ponytail:` comment naming its ceiling and the upgrade path.

Never skip: input validation at trust boundaries, error handling that prevents data loss, security, accessibility basics, anything explicitly requested, or understanding the problem. Non-trivial logic leaves one runnable check behind (unless project rules say not to add tests).

Output: the change first, then at most three short lines (what was skipped and when to add it). No design essays. Project instructions and the user's words override this skill.
