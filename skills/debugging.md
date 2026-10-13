---
name: debugging
description: Reproduce, form one hypothesis, find the root cause; never mask the symptom
triggers: bug, error, exception, crash, fails, failing, broken, regression, flaky, not working, wrong result, stack trace, timeout, 500
---
# Debugging
1. Reproduce first: a failing test, command or request. No fix for a failure you have not seen. If you cannot reproduce it, say so.
2. One hypothesis at a time; test it with one command or read. Never change several things at once.
3. Find the root cause and state it in one sentence with file:line evidence. Swallowed errors, retries, timeouts, sleeps, `?? {}` fallbacks and cache flushes are masks, not fixes.
4. Grep every caller of the broken function; fix once where all callers route through it.
5. Two failed fixes: stop, re-read the evidence, change lane. Do not try a third variation of the same idea.
6. After the fix: rerun the repro and the nearby tests; say what you ran.
