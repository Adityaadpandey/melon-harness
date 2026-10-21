# melon: everything we built and decided, written for the blog

Raw material for a blog post (or three). Everything here comes from the git history (357 commits), the code, and the saved
benchmark files. Where a number is an estimate it says so, and where something was never measured it says that too.

One naming note: the project was called **mew** until Oct 20, when the code, CLI text, env vars and folders became melon too
(`melon --resume`, `MELON_PLAIN`, `~/.melon`, `.melon/`, `refs/melon/backup`). The history below uses "mew" where the commits do.

---

## 1. The one-paragraph story

In nine days (2026-10-13 to 2026-10-21, 357 commits) one person built a terminal coding agent from an empty folder. It started as
a way to get a strong harness for a real product repo (Tunyt), and it ended up being about one idea: **an agent should prove its
work instead of announcing it.** Checks run on the changed files, a second model reviews the real diff, a third agent tries to
disprove every finding, and every file path in the answer is looked up on disk. The rest (cheap context, a sandbox, routing,
learning from your corrections) exists to make that proof affordable and safe.

**Possible titles**
- "I built a coding agent in a week. The best part is the one that argues with itself."
- "Agents say 'done'. Mine has to show its work."
- "What 357 commits taught me about agent harnesses"
- "I benchmarked my homemade agent against Claude Code. It tied, and the misses taught me more than the wins."
- "Every token should earn its place: building a coding agent that never sends the same thing twice"

## 2. The timeline

Commits per day: Oct 13 (29), Oct 14 (96), Oct 15 (72), Oct 16 (36), Oct 17 (26), Oct 18 (19), Oct 19 (56), Oct 20 (20), Oct 21 (3).

| Day | What happened |
|---|---|
| **Oct 13** | The skeleton. Agent loop, provider adapters (Anthropic Messages + OpenAI), model registry with role-based routing and retry. A **task state machine** and the **verification gate** with repair cycles. Repo profiles, an enforced path/command guard. A parallel orchestrator that gives each worker its own git worktree. A benchmark runner. First bug found the hard way: shell commands that needed confirmation were being approved silently. |
| **Oct 14** | The big day (96 commits). Search and edit rewrite (gitignore-aware grep, multi-edit with fuzzy match). An approval engine. Project trust. **Workspace guardrails**: confined file tools, shell path checks, a macOS sandbox. **Anti-hallucination**: read-before-edit and a `file:line` citation check. The **graph** tool, routing tiers, parallel sub-agents, the first TUI, BM25 `search`, Codex login reuse, incremental reads, prompt-cache fixes, pruning, the token cost breakdown, `/undo`, background jobs, learning between sessions, resume. |
| **Oct 15** | "Pi mode". The harness had grown a lot of rails (call caps, a forced scout, reference retries). Copying how the pi agent works (seven plain tools, tiny prompt) and measuring it showed which rails were worth keeping. The first **review pipeline** (reviewers in parallel, then a verifier per finding). Plus JSON and RPC modes, a step checklist like Claude Code's todo list, and a long tail of review-quality fixes from real runs. |
| **Oct 16** | Hooks that can block. Plan mode. Slash commands borrowed from Claude Code. The graph tool moved from a snapshot to **live import-graph answers**. A broad-request "sweep". Retheme of the UI. Sub-agents visible while they work. A routing bug: a model the user picked was being swapped for the strong chain on escalation. |
| **Oct 17** | Playbooks (PR review, commit review, bug hunt, security, scalability, planning). Permission rules in Claude Code notation. User-defined sub-agents. JS/TS auto-verify via each package's own typecheck. `/rewind`. Task notes that survive compaction. Nested `AGENTS.md`. Custom slash commands. `gh` and `curl` allowed outside the sandbox, but only one plain command, only with approval. |
| **Oct 18** | The review system grew up: **review targets** (working tree, last N commits, one commit, branch, PR, including a PR not checked out), lenses, a **contracts** reviewer, a verifier that batches six claims at a time, **sandboxed repro of HIGH findings**, the **advisor** tool, **fix runs** and a **fix verifier**. The motive was a head-to-head with Claude Code's review on one PR. |
| **Oct 19** | Context quality (compaction keeps every user message, a `FILES AND CODE` section, restores working files). Then **learning**: lessons from corrections, written into `AGENTS.md`, and `learn_import --deep` that mines old Claude Code sessions. A context-replay and review benchmark saved to disk. |
| **Oct 20** | README rewritten with diagrams and decisions. Project renamed melon, and the app data moved from `ai-agent` to `melon` on first launch so sessions and lessons carried over. Then a day of **measuring and trimming**: transcript redraws, idle redraws, grep and search index speed. `resume` brings back the whole session. A head-to-head against Claude Code on 11 tasks, and the two things it taught: the **signature guard** and **fewer todo-only turns**. |
| **Oct 21** | `pyproject.toml` and a `melon` command (`uv tool install -e .`), the API key is also read from melon's data folder so it works from any directory. README rewritten to be read, not just kept. |

## 3. Architecture in plain words

(The README has the diagrams. This is the version to paste into prose.)

- One **front end** (a terminal UI or a one-shot CLI) hands a request to a **routing policy** that picks fast, default or strong
  without a model call.
- The **agent loop** streams the model, runs tools, and moves a **task** through a validated state machine (running → verifying →
  repairing → verified, and so on). A task can't jump from failed to verified.
- Every tool call, from the main agent, a sub-agent or a parallel worker, passes through **one function**,
  `ToolRegistry.invoke`. Guard rules, workspace confinement, approvals and the OS sandbox all hang off that single door.
- A **context manager** keeps the conversation small and cache-friendly: caps, an archive with ids, pruning, compaction.
- After edits, a **verification gate** runs checks on the changed files, an **independent review** reads the real diff, and
  **grounding** checks every cited path.
- Memory lives **outside the repo** and holds only what can't be invented.
- Every decision is written to a **JSONL trace**, so a bad run can be replayed.

## 4. The decisions, and the story behind each

Format: *the decision*, *why*, *what went wrong first*. The "what went wrong" is usually the best paragraph for a blog.

### 4.1 Enforce in code, not in the prompt
A prompt that says "never read `.env`" is a request. The guard sits at the single tool door and applies to everything. The first
version of this was a real bug: shell commands that needed confirmation were approved silently (Oct 13). Fail-closed approval
came from that.

### 4.2 The author never grades its own work
A different model reviews the real diff in a fresh context with read-only tools. The same principle gave us the challenger
(argues against a risky plan) and the advisor (a stronger model reads the whole transcript).

### 4.3 Findings are claims, so refute them
Early reviews read like confident fiction. The fix is structural: a verifier agent tries to **disprove** each claim, six at a
time so each gets real turns. A confirmed HIGH is then **run in a throwaway copy** to see the defect happen. If it can't be
reproduced, it is capped at MEDIUM with the attempt shown. Findings that cite a file that doesn't exist are dropped.
Also: claims the verifier refuted are remembered per repo for two weeks, so the same wrong claim isn't raised every review.

### 4.4 A hallucination check that is not a model
Look up every `path` and `path:line` in the answer. A fabricated file or a line past the end of the file is sent back for one
correction. It is a few dozen lines of code. (A related fix on Oct 19: a sub-agent's report has its paths checked on disk, and a
missing one is flagged with the real file of that name, so the main agent doesn't read a guessed path.)

### 4.5 Cheapest evidence first
Search by ranked words, then outline, then read one symbol, then grep. Persisting the search index took a large repo from 13.7s
cold to 1.2s warm.

### 4.6 Never send the same thing twice
Input tokens dominate cost because each call re-sends the conversation. So: remember which lines of a file the model has seen
and return only new ones (or a diff if it changed), cap and archive big results with an id (`recall_result r7`), and make a
whole-file re-read cost one line if unchanged.

### 4.7 Failure always has a next step
Checks fail → repair. Repair fails → escalate to the strong model. A loop → escalate. One provider's plan quota is hit → fail over
to another provider and block the first for the session. Budget runs low → one last tool-free call that turns the work into an
answer. "Nothing falls over silently" became a design rule.

### 4.8 Remember only what can't be invented
Lessons are what the user said. Edit history is what verified runs changed. No model opinions about the repo. Lessons are written
into the repo's own `AGENTS.md`, so a human can read, edit and commit them.
*Why this changed:* the first version learned by matching phrases ("always", "never"). The user's real rules are casual
("commit in parts pls"), and a regex found none of them. So a model now reads each reply **next to what it replied to**, and
"no, not this" or "yes, like that" becomes a lesson. Then `learn_import --deep` weighs candidates against what `AGENTS.md`
already says, so only new rules get added.

### 4.9 Pi mode: delete rails that don't pay rent
mew began with a lot of guardrails: call caps, a scout before every task, reference retries, aggressive clearing. Copying the
simple design of the pi agent (seven tools, a tiny prompt, files read whole) and measuring it showed which rails were worth
keeping. It became the default (`--no-pi` brings the old setup back).
*The honest part:* it wasn't one-way. Within the same day the replay showed **3.4x the input tokens** when old results were never
cleared, so clearing came back. The lesson is "measure each rail", not "fewer rails are better".

### 4.10 Different models for different jobs
Role chains (default, fast, strong, summarize, review, advisor, challenge), each a fallback list. Adding a model needs no code.
The advisor and challenger default to a different model family than the author on purpose, so they don't share blind spots.

### 4.11 Plain files, outside the repo, written atomically
Sessions, traces, archives, lessons and review memory live in the user data dir. Temp file plus rename means a kill mid-write
never leaves a truncated session. Compaction appends the messages it replaces to `transcripts/<session>.jsonl`, because earlier
a compaction lost items 6 to 10 of a pasted bug list and they were simply gone.

### 4.12 Say what you did not do
The answer states its scope (working tree, last 3 commits, PR 42), lists every fallback, says when passing checks don't
exercise the change, and labels guesses `[guess]`.

### 4.13 Trust is per exact file
A repo's `.ai-agent/config.toml` can't turn on hooks, MCP servers, verify commands or approval changes until you trust that exact
file, and editing it revokes the trust. A cloned repo can't quietly turn on code execution.

### 4.14 Let a benchmark miss become a rule
The head-to-head (section 6) had one melon miss: asked to dedupe, `unique_by` lost its `seen=` parameter, the checks passed, and
the run said `verified`. Passing tests did not cover the old call. So a **signature guard** now compares the diff's public
signatures with the old ones (Python through `ast`, TS/JS exports through a light reader) and sends the agent back once if a
parameter was dropped, made required, reordered, or an export vanished. Tests, new files, moved functions and anything the
request names are skipped, so a deliberate change isn't blocked. The prompt had also said "no compatibility leftovers", which
read as permission to drop the parameter; it now asks to keep existing callers working.

### 4.15 Count the model calls, not just the tokens
Looking at the 113 model calls in the benchmark, 31 were turns that held nothing but a todo update, because the todo tool said
"mark each step done the moment it finishes, never batch". Todos are now for long work (5+ steps) and their updates ride along
with the next real call. Cheap per call, but every call re-sends the conversation.

### 4.16 Measure the harness itself, not only the model
Much of Oct 20 was the app, not the agent: the transcript pane re-split and hashed the whole session on each redraw (93ms for
20k lines, now under 5ms), the screen redrew ten times a second even idle (now only when something changed, 0.4/s), grep read
every file line by line (now once per file, and a whole-file miss skips the line pass: 5.1s to 1.1s on a 24k-file repo), and
the search index was built line by line (11.1s to 5.6s). None of this is model quality, and all of it is what makes a long
session feel fast.

## 5. Mistakes, reversals and things we got wrong

Good blog posts need these. All are visible in the commit history.

- **The full-screen UI flip-flopped.** Opt-in on one commit, default again on the next, then plain terminal as default, then that
  was reverted the same day. The tension: a full-screen app looks great but breaks native selection and scrollback copy.
- **Review pipeline: heavy, then lean, then rich again.** First version fanned out to many reviewers. A real run showed
  sub-agents each spawning their own challenger (80k tokens, 4 minutes) and it was cut to one reviewer plus one verifier on a
  cheap model. Over the next days it grew again, but with structure: lenses, a contracts reviewer, batching, repro. Pattern:
  cut to the bone, then add back only what a real run proved was missing.
- **Call caps.** A read-only budget of 20 calls, then 12, then counted in *rounds* instead of calls (so batching parallel reads
  is rewarded), then none in pi mode.
- **A partial `git add` shipped a syntax error** (Oct 16). The follow-up commit fixed it and the lesson was: stage only your own
  hunks when another session is editing the same checkout.
- **A switch that wasn't wired.** `core_tools_only` was added in one commit and "now really narrows the defaults" in the next.
- **A hook that was never called.** The `on_error` hook existed but nothing fired it until Oct 16.
- **Routing bug:** a model the user picked was silently swapped for the strong chain on escalation, even with `-m`.
- **A 39-minute wait** on an approval prompt failed a run the moment it was approved, because the wait counted against the time
  budget. Time spent waiting for the user is now excluded.
- **Pruning too eagerly.** A 53k prompt had its sub-agent report and `AGENTS.md` reads wiped, the model re-read them, and the
  cache broke. Clearing now waits until the prompt is large, and folder rules are re-attached after a prune.
- **Reviews that argued with themselves.** The model listed findings, then dismissed them in the same answer. Fixed by a
  one-time clean rewrite, then by structure (verifier) instead of wording tricks.
- **Learning by regex found nothing** (see 4.8).
- **The rename left sessions behind.** Moving app data from `ai-agent` to `melon` skipped the move when a `melon` folder
  already existed, and the test suite had created one, so 51 sessions and every lesson stayed behind. The move now merges.
  Tests also got their own temporary data folder, so they stop writing into the real one.
- **A test suite that quietly doubled.** Creating an agent downloaded the real model list, taking the suite to 60s and failing
  a timing test. Tests now start with an empty cached catalog.
- **"Verified" was true and still wrong.** `unique_by` dropped a parameter, every check passed, status said `verified` (4.14).
- **The benchmark's other miss was Claude Code's:** asked to delete the repository's history and data, it did.

## 6. Real numbers (and where each comes from)

| Number | Source |
|---|---|
| 357 commits in 9 days; ~17k lines of Python and ~5.9k lines of tests in 56 test files | `git rev-list --count HEAD`, `wc -l` |
| 454 offline tests passing in ~32s | `pytest -q`, run on 2026-10-21 |
| System prompt ~3k → ~0.7k tokens (earlier: ~1,990 → ~1,140 in one pass) | README / commit messages |
| Search index of a large repo: 13.7s cold → 1.2s warm | commit message |
| Context replay: 32 model calls, 64 tool runs, 453k chars of tool output; last request ~25k tokens (~22% of what was ingested) | `bench/results/context-replay-baseline.json` (estimate) |
| ~75% of input served from cache in that replay | same file, estimated from shared prefix |
| 85% of input cached in a real 21-call session | screenshot in the README |
| Replay without clearing old results: 3.4x the input tokens | commit message, Oct 15 |
| Per-task token budget 1.5M → 300k | commit message, Oct 14 |
| 3,000-line transcript redraw: ~109ms → ~3ms per streamed write | commit message, Oct 14 |
| Review sub-agent spawning its own challenger: 80k tokens, 4 minutes | commit message, Oct 15 |
| Skills list was half of a 26k-char system prompt | commit message, Oct 16 |
| **melon vs Claude Code, 11 tasks, one run each: 10/11 and 10/11** | `bench/results/vs-melon-codex_gpt-5.6-luna.json`, `bench/results/vs-cc-claude-code.json` |
| Prompt tokens 1.01M (melon) vs 1.31M (Claude Code); cached share 87% vs 76% | same files, summed |
| Model calls 113 vs 42; wall time 355s vs 273s (melon is slower and chattier here) | same files, summed |
| Claude Code cost $1.49 for the 11 tasks; melon's cost is not recorded (Codex login) | same files |
| 31 of melon's 113 calls were todo-only turns | commit message, Oct 20 |
| Transcript redraw with 20k lines: 93ms to under 5ms; idle redraws 10/s to 0.4/s | commit messages, Oct 20 |
| Broad grep on a 24k-file repo 5.1s to 1.1s; cold search index 11.1s to 5.6s (index file 25MB to 19MB) | commit messages, Oct 20 |
| Deep learn on a real repo: 563 sessions since 2026-05-25 mined; the run produced 17 lessons and dropped the older noise | working notes from the Oct 19 run |

**Not claimed anywhere, on purpose**
- The head-to-head is **one run per task on 11 small tasks, with different models** (Codex luna vs Sonnet 5.5), so it supports
  "in the same range" and nothing stronger. Do not write "melon beats Claude Code". Do write that melon sent fewer tokens and
  more calls, and that each missed a different task.
- The cross-model task benchmark (`bench/results/final-*`) shows 22/22 for one model and 4/22 for most others. That looks like
  quota blocking, not ability, so it is not quoted.
- The planted-bug review bench has two saved runs on two tiny cases, so no recall figure is quoted. One run missed the bug, the
  next found it with no noise on the control case. That is a fair thing to say in a blog, and an unfair thing to turn into a percentage.
- Several defaults were tuned on one real repo. Treat them as measured there, not universal.

If you want stronger numbers for the post, the honest route is to run the head-to-head with the same model on both sides and
several runs per task, rerun the model comparison on a day with free quota, and add more planted-bug cases. Both are real work, and both would make the story better.

## 7. Suggested blog outline

1. **Hook.** "Done" is the most overused word in agent output. Show the screenshot where melon says `verified`, and then says
   the checks don't exercise the changed behaviour.
2. **The rule.** Every token and every tool call should earn its place.
3. **The single door.** Why safety lives in code, with the silent-approval bug as the origin story.
4. **Cheap context.** Incremental reads, the archive with ids, caching that actually hits (the Codex backend only caches with
   `session_id` and `prompt_cache_key`). Use the 3.4x replay number.
5. **The review that argues with itself.** Reviewer → verifier → sandboxed repro. The 80k-token mistake. Why a failing script
   beats a plausible chain of reasoning.
6. **Pi mode: deleting your own rails.** The about-face within a day is the interesting part.
7. **Memory that can't hallucinate.** Regex learning failed; "read the reply next to what it replied to" worked; `AGENTS.md` as
   the store so humans stay in the loop.
8. **The head-to-head.** 10/11 each, with a different miss each. Show the table, then what the misses changed (signature guard,
   fewer todo turns). Be loud about the caveats: different models, one run, 11 tasks.
9. **What isn't there.** macOS-only sandbox, no image input, no head-to-head benchmark. Ending on honesty fits the product.

**Lines worth quoting**
- "A prompt that says never read `.env` is a request. A boundary is code."
- "The author never gets to grade its own work."
- "A reviewer that guesses is cheap to catch if a second agent reads the code with no stake in the claim."
- "Memory should hold what you said and what was verified, not what the model thinks."
- "Every rail has to pay rent. We measured, and we removed the ones that didn't."

## 8. Where to look in the code (for linking in the post)

| Topic | File |
|---|---|
| Task lifecycle | `agent/task.py` |
| Review pipeline, verifier, lenses | `agent/review_pipeline.py` |
| Sandboxed repro of HIGH findings | `agent/review_repro.py` |
| Fix runs and the fix verifier | `agent/review_fix.py`, `agent/review_fix_verify.py` |
| Review targets (PR, commits, branch) | `agent/review_target.py` |
| What earlier reviews ruled out | `agent/review_memory.py` |
| Challenger and advisor | `agent/challenge.py`, `tools/builtin/advisor.py` |
| Citation check | `agent/grounding.py` |
| Routing without a model call | `agent/routing_policy.py` |
| Incremental reads, archive | `tools/builtin/read_file.py`, `utils/archive.py` |
| Ranked search | `utils/rank.py` |
| Import graph, repo map | `utils/importgraph.py`, `utils/repomap.py` |
| Safety layers | `safety/guard.py`, `safety/workspace.py`, `safety/sandbox.py` |
| Learning | `agent/learn.py`, `agent/learn_import.py` |
| Parallel worktrees | `agent/orchestrator.py` |
| Signature guard | `agent/verify.py` (wired in `agent/agent.py`) |
| Benchmarks | `bench/context_replay.py`, `bench/review_bench.py`, `bench/run.py` (`--agent claude-code` for the head-to-head) |
