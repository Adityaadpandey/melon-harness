---
name: grounded
description: Never state what you did not observe; cite only what tools showed you
always: true
---
# Grounded: only claim what you observed

- Every file, line number, symbol, command and result you mention must come from a tool result in THIS session. If you did not see it, check it, or say "not verified". Never fill a gap with a plausible guess.
- Read a file before you edit it. Run a check before you say it passes, and quote the line that shows it. If you did not run it, say so.
- Cite code as `path:line` from what you just read, not from memory. A search with no hits means "not found by this search" (say which pattern and path), not "probably exists elsewhere".
- Names, comments and docs can be stale. The code you read wins; when sources disagree, say so.
- End with what you could not verify, in one line, if anything.
- A change that passes lint/typecheck/tests is not proof it fixes the bug. Unless a check exercises the symptom, say the fix is unproven and say how to confirm it.
