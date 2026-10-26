---
name: debugging
description: Reproduce, form one hypothesis, find the root cause; never mask the symptom
triggers: bug, bugs, error, exception, crash, fails, failing, broken, regression, flaky, not working, wrong result, stack trace, timeout, 500
---
# Debugging and fixing
1. Reproduce first: a failing test, command or request. No fix for a failure you have not seen. If you cannot reproduce it, say so and say what you tried.
2. Read the evidence before theorising: the full error, the stack frame inside this repo (not the framework), the log line around it, `git log -p -- <file>` if it used to work. A regression has a first bad commit; `git bisect run <test>` finds it in a few steps.
3. One hypothesis at a time; test it with one command or read. Never change several things at once. Example: "500 on /orders" -> hypothesis "`customer` is null for guest checkout" -> one read of the handler and one request with a guest cart, not a try/catch around the whole handler.
4. Find the root cause and state it in one sentence with file:line evidence. Swallowed errors, retries, timeouts, sleeps, `?? {}` fallbacks and cache flushes are masks, not fixes. Ask why the bad value got there, not only where it blew up.
5. Grep every caller of the broken function and every sibling with the same pattern; fix once where all callers route through it.
6. Keep the fix small and add a regression test that fails before and passes after (run it both ways). Do not refactor while fixing.
7. Two failed fixes: stop, re-read the evidence, change lane. Do not try a third variation of the same idea.
8. A page, screen or endpoint that "won't load" is reproduced at runtime, not by the typechecker. Request it (`curl -sIL --max-redirs 5 <url>` shows a redirect loop or a 500 at once), read the dev-server log and the browser console, and grep the framework config and middleware for redirects or rewrites of that path (a rename often leaves an old alias pointing back). A green `tsc` proves types, never that it renders.
9. A claim about how a framework behaves ("redirect() deadlocks in v16", "skip leaves isLoading true") is a hypothesis until you have read the installed source or docs in node_modules, or watched it happen. Never switch to a new theory because the last one sounded wrong; go back to the evidence.
10. Report: cause, fix, what you ran to prove it, and what you did not check. If the fix is a guess, call it a guess. Never write "reload and it should work" for something you did not load: say "not verified in a browser" and what to check.
