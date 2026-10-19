<p align="center">
  <img src="docs/banner.svg" alt="melon: the coding agent that proves its work" width="100%">
</p>

<p align="center">
  <img alt="python" src="https://img.shields.io/badge/python-3-c3a6ff?style=flat-square">
  <img alt="tests" src="https://img.shields.io/badge/tests-449%20passing-8ff0c4?style=flat-square">
  <img alt="models" src="https://img.shields.io/badge/models-Claude%20%C2%B7%20GPT%20%C2%B7%20Codex%20%C2%B7%20DeepSeek-ff8fc7?style=flat-square">
  <img alt="sandbox" src="https://img.shields.io/badge/sandbox-macOS-ffd28f?style=flat-square">
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#melon-in-numbers">Numbers</a> ·
  <a href="#architecture">Architecture</a> ·
  <a href="#design-decisions">Design decisions</a> ·
  <a href="#reviews-and-fixing-what-they-find">Reviews</a> ·
  <a href="#safety">Safety</a> ·
  <a href="#not-there-yet">Not there yet</a>
</p>

# melon

**The coding agent that proves its work.**

Most agents tell you they are done. melon shows you: the checks that passed, a second model's review of the real diff, and every
file reference in the answer checked against your tree. Underneath sits one rule: **every token and every tool call should earn its place.**

melon is a terminal harness for Claude, GPT/Codex, DeepSeek and anything else behind an OpenAI-style API. It is fast on big repos
because it finds code before it reads it, cheap because it never sends the same thing twice, and safe because its guardrails live
in code, not in a prompt asking the model to behave.

<p align="center">
  <img src="docs/screenshot.png" alt="melon working on a real repo: side panel, token breakdown, verification and review" width="100%">
</p>

A real session, not a mock-up: the request was judged hard and routed to the strong model, that provider hit a rate limit and melon
said so and failed over, the cost report shows 85% of input cached, and the result is `verified` with a plain note that the passing
checks do not exercise the changed behaviour.

## melon in numbers

Measured from this repo, not rounded up. Where a figure is an estimate, it says so.

<table>
<tr>
<td align="center"><h3>~0.7k</h3>tokens in the system prompt<br><sub>was ~3k, <b>4x smaller</b></sub></td>
<td align="center"><h3>75%</h3>of input served from cache<br><sub>32-call replay, estimated</sub></td>
<td align="center"><h3>1.2s</h3>warm search index<br><sub>cold: 13.7s, <b>11x faster</b></sub></td>
<td align="center"><h3>449</h3>offline tests<br><sub>all passing in ~27s</sub></td>
</tr>
<tr>
<td align="center"><h3>~22%</h3>of tool output still in context<br><sub>last request of the replay vs all ingested</sub></td>
<td align="center"><h3>85%</h3>input cached in a real session<br><sub>21 calls, see screenshot</sub></td>
<td align="center"><h3>2</h3>agents must agree<br><sub>a reviewer finds it, a verifier fails to refute it</sub></td>
<td align="center"><h3>3</h3>independent safety layers<br><sub>tools, shell scan, OS sandbox</sub></td>
</tr>
</table>

| | |
|---|---|
| Codebase | about 18k lines of Python, plus 5.7k lines of tests in 52 files |
| Surface | 21 built-in tool files, 19 skills, 7 model roles, 10 hook points, 7 approval modes |
| Context replay | 32 model calls, 64 tool runs, 453k characters of tool output; the last request carries about 25k tokens |
| Benchmarks | context replay (free, deterministic), planted-bug review bench, and real-repo tasks in a scratch clone |

How the replay numbers are made: `python -m bench.context_replay` scripts a "model" that makes the same calls in the same order
whatever the harness does, so any difference between two runs comes from the harness. It needs no API key and gives identical
numbers every run. Cache reads are estimated from the longest prefix a request shares with the previous one. Only the 75% figure
and the 22% figure come from it; the 85% figure is from the live provider usage in the screenshot.

The planted-bug review bench is tiny (a handful of cases, two saved runs), so no recall number is claimed. It is there so you can
measure your own reviewer on your own bugs.

## Contents

1. [melon in numbers](#melon-in-numbers)
2. [The idea in one picture](#the-idea-in-one-picture)
3. [Quick start](#quick-start)
4. [Architecture](#architecture)
5. [Life of a request](#life-of-a-request)
6. [Design decisions](#design-decisions) (the part worth reading)
7. [Context: how melon stays cheap](#context-how-melon-stays-cheap)
8. [Finding code without reading everything](#finding-code-without-reading-everything)
9. [Correctness: verify, review, ground](#correctness-verify-review-ground)
10. [Reviews, and fixing what they find](#reviews-and-fixing-what-they-find)
11. [Safety](#safety)
12. [Models and routing](#models-and-routing)
13. [Memory and learning](#memory-and-learning)
14. [Parallel agents](#parallel-agents)
15. [Skills, subagents, profiles](#skills-subagents-profiles)
16. [Interactive UI](#interactive-ui)
17. [Extending: MCP, hooks, commands](#extending-mcp-hooks-commands)
18. [Tests and benchmarks](#tests-and-benchmarks)
19. [Not there yet](#not-there-yet)

## The idea in one picture

Most harnesses make the model do the hard parts and then hope. melon moves the hard parts into code the model cannot skip.

| Problem every agent has | What most do | What melon does |
|---|---|---|
| Finding the right file | grep around, read a lot | BM25 `search`, `outline`, symbol reads, a read-only **scout** |
| Re-reading files | send the whole file again | per-file map of lines already seen; a changed file comes back as a **diff** |
| Context filling up | summarize everything at 90% | newest 40k tokens verbatim, older turns folded into a summary, nothing important paraphrased |
| Model says "done" | trust it | **verification gate**, then an **independent reviewer** on a different model |
| Reviewer cries wolf | show every claim | a **verifier** tries to refute each finding; HIGHs are **reproduced** in a sandbox |
| Model invents a file or line | nothing | every `path:line` in the answer is checked against the tree |
| Safety | "please don't touch ~/.ssh" in the prompt | confined tools, a path-scanning shell, an OS sandbox |
| Provider quota hits | the session dies | fails over to another provider and says so |
| A bad edit | `git checkout` and hope | pre-run backup and `/undo` |
| Forgetting what you told it | start over each session | lessons learned from your corrections, written into `AGENTS.md` |

## Quick start

```bash
uv venv .venv && uv pip install --python .venv/bin/python -r requirements.txt
echo 'API_KEY=...' > .env            # BASE_URL defaults to https://api.commandcode.ai/provider/v1
.venv/bin/python main.py                                      # interactive
.venv/bin/python main.py -m claude-haiku-5-5 --approval auto "your task"   # one shot
```

Using a ChatGPT/Codex login instead of an API key? `-m codex/<model>` reads and refreshes the login the Codex CLI already stored.
No key needed.

## Architecture

melon is a handful of layers with one rule between them: **everything the model can do goes through one door, and everything the
harness decides is written to a trace.**

```mermaid
flowchart TB
    subgraph UI["Front end"]
        TUI["Terminal UI<br/>(ui/)"]
        CLI["One-shot CLI<br/>(main.py)"]
    end

    subgraph CORE["Agent core (agent/)"]
        ROUTE["Routing policy<br/>fast / default / strong"]
        LOOP["Agent loop<br/>stream, tool calls, repair"]
        TASK["Task state machine<br/>validated transitions"]
        VER["Verification gate"]
        REV["Review pipeline<br/>reviewers, verifier, repro"]
        GROUND["Grounding<br/>citation check"]
    end

    subgraph CTX["Context (context/, utils/)"]
        MGR["Context manager<br/>prune, compact, restore"]
        ARCH["Result archive<br/>recall_result rN"]
        LOOPD["Loop detector"]
    end

    subgraph TOOLS["Tools"]
        REG["ToolRegistry.invoke<br/>the single door"]
        BUILT["read, edit, search, outline,<br/>git, graph, shell, advisor, ..."]
        MCP["MCP servers"]
    end

    subgraph SAFE["Safety (safety/)"]
        GUARD["Guard: forbidden paths and commands"]
        WS["Workspace confinement"]
        APP["Approval + rules"]
        SBX["OS sandbox"]
    end

    subgraph LLM["Client (client/)"]
        RT["Router + failover"]
        AD["Adapters<br/>/messages, /chat, Responses SSE"]
    end

    MEM[("Per-repo memory<br/>lessons, edit history,<br/>review memory, notes")]
    TRACE[("Trace JSONL<br/>every decision")]

    UI --> ROUTE --> LOOP
    LOOP <--> TASK
    LOOP --> REG
    REG --> GUARD --> WS --> APP --> SBX
    REG --> BUILT
    REG --> MCP
    LOOP <--> MGR
    MGR <--> ARCH
    LOOP --> LOOPD
    LOOP --> RT --> AD
    LOOP --> VER --> REV --> GROUND
    MEM -.feeds.-> LOOP
    LOOP -.writes.-> MEM
    CORE -.-> TRACE
    LLM -.-> TRACE
```

| Layer | Folder | Job |
|---|---|---|
| Front end | `ui/`, `main.py` | Full-screen terminal UI or a one-shot run; approvals, side panel, slash commands |
| Agent core | `agent/` | The loop, the task lifecycle, verify, review, grounding, learning |
| Context | `context/`, `utils/` | Pruning, compaction, the result archive, loop detection, ranked search |
| Tools | `tools/` | Built-in tools, MCP, sub-agents, one registry every call passes through |
| Safety | `safety/` | Guard rules, workspace confinement, approvals, the macOS sandbox |
| Client | `client/` | Model catalog, routing, failover, one adapter per wire format |
| Config | `config/` | Layered config, project trust |

## Life of a request

<p align="center">
  <img src="docs/flow.svg" alt="Request flow: classify, scout, edit, verification gate, review, citation check" width="100%">
</p>

The same flow as a diagram you can follow branch by branch:

```mermaid
flowchart LR
    A([Your request]) --> B{Classify<br/>no model call}
    B -->|easy| F[fast chain]
    B -->|normal| D[default chain]
    B -->|hard| S[strong chain]
    F & D & S --> L[Agent loop]
    L -->|read-only questions| ANS
    L -->|edits files| V{Verification gate<br/>checks on changed files}
    V -->|pass| R{Independent review}
    V -->|fail| P[Repair round<br/>max 2]
    P --> V
    R -->|clean| G[Citation check]
    R -->|findings| P2[One fix, review again]
    P2 --> G
    G --> ANS([Answer + status])
    L -. loop detected .-> S
    P -. still failing .-> S
```

Every request is a **task** with an explicit lifecycle. Transitions are validated in code, so a run can't jump from `failed` to
`verified`, and each move is traced.

```mermaid
stateDiagram-v2
    [*] --> running
    running --> verifying: files changed
    running --> done_no_changes: nothing edited
    running --> unverified: no checks exist
    running --> failed: API error, loop, budget, deadline
    verifying --> verified: checks pass
    verifying --> repairing: checks or review failed
    verifying --> verification_failed: still failing after repairs
    repairing --> verifying: fix applied
    repairing --> failed
    verified --> [*]
    unverified --> [*]
    verification_failed --> [*]
    done_no_changes --> [*]
    failed --> [*]
```

The final status is always one of `verified | unverified | verification_failed | failed | done_no_changes`. It is never a bare "done".

## Design decisions

The choices that shaped melon, with the reason behind each. Several started as a mistake that showed up in a real session.

### 1. Enforce in code, ask in the prompt only for taste

A prompt that says "never read `.env`" is a request. melon puts rules where the model cannot talk its way past them:
`ToolRegistry.invoke` is the only way to run a tool, and the guard, workspace confinement and approval checks sit in front of
every call, for the main agent, sub-agents and parallel workers alike.
*Trade-off:* more code to maintain than a paragraph in a prompt, but a jailbroken or confused model still can't leave the folder.

### 2. The author never grades its own work

The model that wrote a change is the worst judge of it. Review runs in a separate context, with read-only tools, on a different
model when routing allows. Same idea for the **challenger** (below) and the **advisor**.
*Trade-off:* extra model calls. They are spent only where a mistake is expensive (a diff, a risky design question).

### 3. Reviewers are cheap to fool, so a second agent tries to refute them

Early reviews read like confident fiction: plausible chains of reasoning about code that did not behave that way. So a finding
is a claim, not a result. Each claim goes to a verifier whose job is to **disprove** it; confirmed HIGHs are run in a throwaway
copy of the change to see the defect happen, and one that does not reproduce is capped at MEDIUM with what was tried shown beside
it. Findings citing a file that doesn't exist are dropped before anyone reads them.
*Trade-off:* a review costs more and takes longer. The reader gets fewer, truer findings.

### 4. A hallucination check that is not a model

Every `path` and `path:line` in the final answer is looked up in the real tree. A made-up file or a line past the end of a file
goes back for one correction; whatever is still unverified is shown as a warning. It is a few dozen lines of code and catches a
whole class of confident wrongness for free.

### 5. Cheapest evidence first

Tools are ordered by cost: `search` (ranked files) → `outline` (symbols) → `read_file(symbol=...)` → `grep`. The scout, the
prompt and the tool descriptions all push the model down that ladder, because a 1,300-line file read to find one function is
tokens that every later call re-sends.

### 6. Never send the same thing twice

`read_file` remembers which lines the model has seen and returns only new ones, or a diff if the file changed. Cleared or capped
tool output isn't lost: it goes to an archive and the model gets an id (`r7`) to `recall_result`. Compaction deterministically
lists files read and modified. The goal is that the model never has to guess what it used to know.

### 7. Failure has a defined next step

Checks fail → repair. Repair fails → escalate to the strong model. A loop is detected → escalate. Quota hits one provider → fail
over to another provider and block the first for the session. Budget runs low → one last tool-free call that turns the work so
far into an answer. Nothing falls over silently, and nothing ends in "the model said it was fine".

### 8. Only remember what cannot be invented

Memory stores what *you* told melon and what *verified* runs changed. It does not store the model's opinions about your repo,
because those are exactly what drifts and compounds. Lessons are plain text in your `AGENTS.md`, so you can read, edit and commit
them.

### 9. Pi mode is the default: fewer rails, simpler agent

melon started with many guards: call caps, aggressive clearing of old results, a scout before every task. Measured against a simple
agent (seven plain tools, a tiny prompt, files read whole), the guards often cost more than they saved. So **pi mode** is on by
default, copied from how the [pi](https://github.com/badlogic/pi-mono) coding agent works: a small tool set, no call caps, results
cleared only near the window, compaction only close to the limit. `--no-pi` brings back the guarded setup (scout, reference check,
parallel tasks). Safety and correctness layers stay on in both.
*Lesson:* every rail needs to pay rent. We measured with `bench/context_replay.py` and removed what didn't.

### 10. Different brains for different jobs

One model for everything wastes money and one model for the review shares the author's blind spots. Routing is by **role**
(`default`, `fast`, `strong`, `summarize`, `review`, `advisor`, `challenge`), each a fallback chain. Adding a model needs no
code. The advisor and challenger default to a different model family than the author on purpose.

### 11. Plain files, outside the repo, atomic

Sessions, traces, archived results, learned lessons and review memory live in the user data dir, not in your project. Writes are
temp-file + rename, so a kill mid-write never leaves a truncated session. Traces are JSONL so a bad run can be replayed line by line.

### 12. Say what you did not do

The answer states the review scope (working tree, last 3 commits, PR #42), lists every fallback (PR base unknown, fetch failed),
says when passing checks don't exercise the change, and labels guesses as `[guess]`. A harness that proves its work has to be
equally clear about where it couldn't.

## Context: how melon stays cheap

The cheapest token is the one never sent. Input tokens dominate cost because every call re-sends the whole conversation, so most of
melon's savings come from keeping that prefix small and stable.

```mermaid
flowchart LR
    T[Tool result] --> CAP{Over the output cap?}
    CAP -->|yes| ARCH[(Archive r7)]
    CAP -->|no| CTX[In context]
    ARCH -->|note + id| CTX
    CTX --> PRUNE{Prompt > 180k tokens?}
    PRUNE -->|yes| CLEAR[Clear old results<br/>keep newest, leave a hint]
    CLEAR --> ARCH
    PRUNE -->|no| COMP{Past 200k tokens?}
    CLEAR --> COMP
    COMP -->|yes| FOLD[Fold older turns into summary<br/>keep newest 40k verbatim]
    FOLD --> RESTORE[Restore up to 5 working files<br/>as they are on disk]
    RESTORE --> CTX
    ARCH -. recall_result .-> CTX
```

- **Tiny system prompt.** About 0.7k tokens (it was ~3k). Always-on skills add about 450 more.
- **Prompt caching that actually hits.** Claude calls cache the system prompt and the conversation prefix. The Codex backend only
  caches when `session_id` and `prompt_cache_key` are sent, so melon sends both. A model that isn't caching triggers a warning, and
  `/cost` says why a miss happened (idle gap, model changed, prefix changed).
- **Incremental reads.** Only new lines, or a diff since the last copy the model saw.
- **Output caps and archive.** Over-broad `grep` collapses into a summary; every result is capped; the full text is recallable.
- **Pruning inside a task.** Old results are dropped once the prompt passes 180k tokens, with a hint of what was cleared.
- **Compaction that keeps facts.** A summary with a `FILES AND CODE` section of verbatim snippets, every user message kept
  (lists and `file:line` whole), an errors-and-fixes section, and the replaced messages saved to
  `transcripts/<session>.jsonl` so nothing is truly lost. Terminal noise is stripped for free before the summarizer sees it.
- **Folder rules follow the model.** Reading `apps/web/src/a.tsx` attaches `apps/web/AGENTS.md` once, and re-attaches it after a
  prune or compaction clears the result that carried it.
- **Task notes.** A `notes` tool keeps goal, decisions, done and next on disk, so a long job survives compaction and `/resume`.
- **Cost-weighted accounting.** Each result is weighted by how many later calls re-sent it, so `/cost` points at the thing worth
  fixing, and only shows the breakdown when it is worth acting on.
- **Persistent search index.** Cold index of a large repo: 13.7s. Warm: 1.2s.
- **Long commands go to the background.** A shell command still running after 15s becomes a job (`/jobs`); the model is told when
  it finishes.
- **No stalls.** A stuck stream can't outlive the request budget; a hung worker fails after 15 minutes; sub-agents have a hard timeout.

## Finding code without reading everything

In the order to reach for them, cheapest first:

1. **`search`**: BM25-ranked files for a plain-words query, camelCase-aware, with co-occurrence rerank and noise down-weighting.
   Files that fixed similar requests before get a boost.
2. **`outline`**: symbols and JSX section markers with line ranges (bracket-balanced, so huge components don't blow up).
3. **`read_file(path, symbol=...)`**: one function, not the file.
4. **`git`**: read-only `blame`, `log`, and `find_change` (`git log -S`) to answer "when did this break".
5. **`graph`**: structure questions (callers, callees, path, community) from a graphify graph. Call direction is read from source
   because graph edge order is unreliable, and results say how stale the graph is. An import graph (tsconfig `paths`, relative and
   Python imports) is read straight from the tree, so it never goes stale.
6. **`grep` / `glob`**: exact strings. They respect `.gitignore`, find dotfiles, never touch secrets, and use smart-case.

With `--no-pi`, a non-trivial request starts with a **scout**, a read-only investigator on a cheap model, so the main context begins
from findings instead of searches. In pi mode the same investigator is available as a sub-agent the model can call, so wide
searches run in their own context instead of filling this one.

## Correctness: verify, review, ground

A run that changes files is not `verified` until it clears three stages:

```mermaid
flowchart TB
    EDIT[Files changed] --> G1
    subgraph G1["1. Verification gate"]
        C1["verify_commands / verify_rules<br/>pytest, go, cargo auto-detected<br/>only the changed files"]
    end
    G1 -->|fail, up to max_repairs| REP[Back to the model as a repair request]
    REP --> G1
    G1 -->|pass| G2
    subgraph G2["2. Independent review"]
        C2["Separate agent, different model<br/>reads the real diff, read-only tools"]
    end
    G2 -->|findings| REP2[One fix, then review again]
    REP2 --> G3
    G2 -->|clean| G3
    subgraph G3["3. Grounding"]
        C3["Read-before-edit<br/>citation check<br/>'grounded' rule"]
    end
    G3 --> OUT([verified / unverified / ...])
```

1. **Verification gate.** `verify_commands` or per-path `[[verify_rules]]` (`{files}` = changed files, `{tests}` = related unit
   specs). Checks cover **only the changed files**, never a build or the whole suite. Failures go back as a repair request
   (`max_repairs`, default 2). A note typed during the checks stops them and re-checks the whole diff after.
2. **Independent review** (`auto_review = true`). A read-only reviewer on a different model reads the real diff with search,
   outline and git. It tests the author's stated root cause against the code and says so when passing checks don't exercise the
   change. `max_reviews` (default 2) bounds the loop: review, one fix, review that fix, and whatever the second finds is shown
   to you instead of looping, because each blind repair round re-sent a 70k context dozens of times.
3. **Grounding.**
   - *Read before edit:* editing a file the agent hasn't read this session is refused, and so is editing one that changed on
     disk since it was read.
   - *Citation check:* every `path` and `path:line` in the final answer is verified against the tree (guarded mode).
   - *Always-on `grounded` rule:* only claim what a tool showed you.

**Challenger.** Before acting on a risky request (a design choice, a migration, "should we...?"), a read-only agent on a different
model argues against the plan. Each objection is `[verified]` with a real `file:line` or labelled `[guess]`; objections with
wrong citations are removed, never shown.

**Advisor.** A parameterless tool: the agent calls `advisor()` and a stronger model reads the *whole* transcript and returns
numbered advice. Fix runs can't make their first edit until the advisor has been consulted, and consult it again before finishing.

**Loop detection** catches repeating actions and escalates to a stronger model instead of burning budget.

## Reviews and fixing what they find

Ask "review my changes", "review my last 3 commits", "review this branch" or "review PR #42" and melon runs a full pipeline modeled
on how a careful human team reviews.

```mermaid
flowchart TB
    Q["'review PR #42'"] --> TGT["Resolve the target<br/>working tree / commits / branch / PR"]
    TGT --> WT{PR checked out?}
    WT -->|no| FETCH["Fetch pull/42/head into a<br/>throwaway worktree, diff against<br/>the real base branch"]
    WT -->|yes| DIFF
    FETCH --> DIFF["Build the diff<br/>skip lockfiles, images, maps"]
    DIFF --> SPLIT{Big diff?}
    SPLIT -->|yes| AREAS["Up to 3 areas, one<br/>reviewer each"]
    SPLIT -->|no| ONE["One reviewer, every angle"]
    AREAS --> CON
    ONE --> CON["+ Contracts reviewer<br/>follows each changed value<br/>from producer to consumer"]
    CON --> SEC["+ Security lens if the diff<br/>touches auth, data, money"]
    SEC --> GAP["Gap pass: what did the<br/>others miss?"]
    GAP --> VERIFY["Verifier: tries to refute<br/>each claim, 6 at a time"]
    VERIFY -->|refuted| DROP[Dropped, remembered for 2 weeks]
    VERIFY -->|confirmed HIGH| REPRO["Repro agent: run it in a<br/>sandboxed copy"]
    REPRO -->|reproduced| SHOW
    REPRO -->|cannot reproduce| CAP["Capped at MEDIUM,<br/>attempt shown"]
    CAP --> SHOW
    VERIFY -->|confirmed| SHOW["Write-up: top findings grouped as<br/>likely bugs / worth fixing / minor"]
    SHOW --> OFFER["Offer to fix them"]
```

- **Scope is stated.** The answer says what was reviewed, how many commits, and every fallback taken. A PR is diffed against the
  branch it merges into (from `gh`, not assumed to be `main`), after fetching that base so a stale local copy doesn't drag in other
  people's commits.
- **Lenses.** Behaviour, approach, failures, project rules, history, security and a dedicated producer/consumer *contracts* reviewer.
  By default one reviewer works through every angle with a bigger budget; set `review_lenses = N` for N separate specialists.
- **Review memory.** A claim the verifier refuted, or you called wrong, is remembered per repo for two weeks and shown to the next
  reviewers as "already ruled out". Code that changed since then reopens it.
- **Alternate reviewer.** A review can run as a headless `claude -p` session on your own Claude login, with read-only tools.
- **A planted-bug benchmark** (`bench/review_bench.py`) scores a reviewer on seeded defects, and a replay of a real PR's review is
  scored against an external reviewer's findings.

### Fixing

Say "fix them", "fix 2 and 3" or "review and fix", and the fix run starts from the facts the review already proved (not a vague
"fix the issues"):

```mermaid
flowchart LR
    F["'fix 2 and 3'"] --> BR["Brief per finding:<br/>where, why it breaks"]
    BR --> ADV1["advisor(): required<br/>before the first edit"]
    ADV1 --> EDIT["Root-cause fix +<br/>regression test"]
    EDIT --> ADV2["advisor(): again<br/>before finishing"]
    ADV2 --> FV["Fresh verifier in a<br/>scratch worktree"]
    FV -->|PASS| DONE([Done])
    FV -->|FAIL| EDIT
```

The verifier never saw the author's reasoning, so it can't inherit its blind spots. It diffs the change against what the author
*claimed*, triggers each finding, checks that the added test fails without the fix, and runs checks and edge cases. A `FAIL` goes
back to the author.

## Safety

Three independent layers, so one bug doesn't open the door:

```mermaid
flowchart LR
    CALL[Tool call] --> L1
    subgraph L1["1. File tools"]
        W["Resolve path first<br/>.. and symlinks out are refused"]
    end
    L1 --> L2
    subgraph L2["2. Shell scan"]
        S["Paths in ~, /etc, other users,<br/>.. escapes: refused before running<br/>history-destroying git blocked"]
    end
    L2 --> L3
    subgraph L3["3. OS sandbox"]
        O["macOS sandbox-exec<br/>write only workspace + temp + caches<br/>no ~/.ssh ~/.aws ~/.gnupg keychains"]
    end
    L3 --> RUN([Runs])
```

1. **File tools** (read, edit, write, list, grep, glob, and the `cwd` of shell) are confined to the folder you started in. Paths are
   resolved first, so `..` and symlinks that point out are refused. Add roots on purpose with `--add-dir`.
2. **Shell commands** are scanned for paths in your home, `/etc`, other users' folders, or `..` escapes, and refused before they
   run. History-destroying git commands are blocked.
3. **OS sandbox** (macOS `sandbox-exec`, on by default). Even a dynamically built command cannot write outside the workspace, temp
   dirs and package caches, or read `~/.ssh`, `~/.aws`, `~/.gnupg` and keychains. `sandbox = "off"` disables it.

Plus:

- `.env*`, `*.pem`, `*.key` and SSH keys are protected by default.
- **Allow / deny / ask rules** in Claude Code's notation: `Bash(git diff:*)`, `Edit(src/**)`, `Read(.env*)`, `WebFetch(domain:...)`.
- `gh` and `curl` run outside the sandbox (they need the keychain login and the network) but only as one plain command, only
  after you approve, even in auto mode. Chained or piped ones stay sandboxed.
- `web_fetch` refuses localhost, private ranges and cloud metadata at *every* redirect hop (SSRF-safe).
- **Project trust:** a repo's `.ai-agent/config.toml` cannot enable `hooks`, `mcp_servers`, `verify_commands`, `verify_rules`,
  `approval` or `shell_environment` until you trust that exact file (`--trust`). Editing the file revokes the trust. A cloned repo
  can't quietly turn on code execution.
- **Approvals show a diff**, and what you approve is exactly what gets written: preview and write share one planner. Choices are
  yes / yes and don't ask again / no, and tell me what to do differently (your reply goes to the model).
- **Atomic writes** for files and sessions.
- **`/undo`:** before each request melon records the working tree as a commit object (`refs/mew/backup`, nothing touches your index or
  stash). `/undo` restores what the last request changed and removes files it created. Untracked files that existed before are never
  deleted.

Approval modes: `on-request` (default), `auto-edit`, `auto`, `plan` (read-only, ends in a plan), `never`, `on-failure`, `yolo`. Shift+Tab cycles them.

## Models and routing

```mermaid
flowchart LR
    REQ[Request] --> CL["Classifier<br/>(rules, returns its reason)"]
    CL -->|fast| FC["fast chain"]
    CL -->|default| DC["default chain"]
    CL -->|strong| SC["strong chain"]
    FC & DC & SC --> TRY{Call model}
    TRY -->|ok| OK([Stream])
    TRY -->|not in plan / not found| SKIP["Skip for 1 hour,<br/>try next in chain"]
    TRY -->|plan-wide quota| XPROV["Fail over to another provider,<br/>block this one for the session"]
    SKIP --> TRY
    XPROV --> TRY
    XPROV -.last resort.-> OWN[Your own model]
```

- Claude models go through `/messages`, everything else through `/chat/completions`, picked from the live `/models` catalog
  (cached 24h). Codex models use the Responses API over SSE with automatic token refresh.
- **Role chains** in `.ai-agent/config.toml`:

  ```toml
  [routing]
  default   = ["claude-sonnet-5-5", "deepseek/deepseek-v4-pro"]
  fast      = ["deepseek/deepseek-v4.1-flash"]       # easy requests
  strong    = ["claude-opus-5-5"]                    # hard ones, and escalation
  summarize = ["deepseek/deepseek-v4.1-flash"]       # context compaction
  review    = ["gpt-5.6-sol"]
  advisor   = ["deepseek/deepseek-v4-pro"]
  ```

- **Difficulty tiers.** Each request is classified without a model call: small, well-defined work goes `fast`; debugging,
  concurrency, security, architecture and multi-file work go `strong`. A failed repair or a loop escalates to `strong`. `-m` or
  `/model` pins a model. `/effort low|medium|high|auto` sets how hard it thinks.
- `python scripts/probe_models.py --json out.json` shows which models your key can really call.

## Memory and learning

Per repo, stored outside it. melon keeps only what cannot be invented: what *you* told it, and what *verified* runs changed.

```mermaid
flowchart TB
    subgraph IN["Signals"]
        S1["/learn rule"]
        S2["Rejecting an action with feedback"]
        S3["Standing rule: 'always use pnpm'"]
        S4["A correction: 'no, not this',<br/>'I meant the header'"]
        S5["Verified run: request words<br/>-> files changed"]
        S6["Past Claude Code sessions<br/>(learn_import)"]
    end
    S1 & S2 & S3 & S4 --> LES["Lessons"]
    S6 -->|"reward: commit/works +1<br/>interrupt/'no...' -1"| LES
    S6 --> EH
    S5 --> EH["Edit history"]
    LES --> AG["AGENTS.md<br/>'## Learned by mew'"]
    LES --> PROMPT["Next session's prompt<br/>newest first, capped"]
    EH --> SEARCH["search boosts files that<br/>fixed similar requests"]
```

- **Lessons.** `/learn <rule>`, a rejection with feedback, a standing rule at the start of a message, or a correction. A model reads
  each reply *next to what it replied to*, so "no, not this" or "yes, like that" becomes a lesson tied to its request, not a
  phrase-match. It runs as a background call. Lessons are written into the repo's `AGENTS.md` under `## Learned by mew`, so you can
  review and commit them. `/lessons` lists them, `/forget N` removes one (also from `AGENTS.md`).
- **Edit history.** Each verified run records its request words and the files it changed. A match is the share of the search
  query's words, so a 40-word brief can't drown a 5-word search.
- **Learn from the past.** `python -m agent.learn_import --repo <path>` mines your Claude Code sessions. Claude Code deletes
  transcripts after 30 days, but `~/.claude/history.jsonl` keeps every prompt forever. `--deep` reads each task's story in chunks
  (files, failed commands, what Claude said, your reply) and **weighs candidates against the repo's existing `AGENTS.md` rules,
  taste and skills**, so only new rules are added and noise is dropped.
  The deep run is review-then-apply: the dry run saves its verdict, `--apply` writes exactly that with no model call, and
  `--reweigh` redoes only the final step.
- **Instruction files** are honoured the way you wrote them: every `AGENTS.md` / `CLAUDE.md` in a directory plus
  `~/.claude/CLAUDE.md`, `@imports` followed, up to 80k chars whole, and commit/comment/spec/plan rules kept even when a file is
  too long to inline. Command Code `taste` files are read too.
- **Review memory** (see above) and **task notes** (see context) are the other two stores.

## Parallel agents

- **Read-only sub-agents** called in the same turn run concurrently under a shared cap (`max_parallel_agents`, default 4) with live
  progress. A sub-agent's report has its paths checked on disk; a missing one is flagged at the top with the real file of that name.
- **`parallel_tasks`** (guarded mode) fans write-capable workers out into separate **git worktrees**, merges them in dependency
  order into `mew/<run>/integration`, detects overlap, re-runs real conflicts on top of the merged state, and flags them for review
  instead of resolving them by guessing. `--parallel` lets the model plan the split; `--graph tasks.json` is explicit.
  It only pays off for genuinely independent work. Measure before relying on it.

```mermaid
flowchart LR
    PLAN[Task graph] --> W1[Worktree A]
    PLAN --> W2[Worktree B]
    PLAN --> W3[Worktree C<br/>depends on A]
    W1 --> MERGE[Merge in dependency order<br/>into integration branch]
    W2 --> MERGE
    W3 -.after A.-> MERGE
    MERGE -->|overlap or conflict| RERUN[Re-run the conflicted task<br/>on top of merged state]
    RERUN --> MERGE
    MERGE --> REVIEW[Flag for review]
```

## Skills, subagents, profiles

- **Skills** are short playbooks with `triggers`. Matching ones attach automatically (keyword match, no model call); the model can
  pull any other with `use_skill(name)` or one section with `use_skill(name, section=...)`. Every skill is listed with a one-line
  "use when" so the model can choose. A playbook already in the conversation is named, not pasted again.
  - Always on: `ponytail` (do the least that fully works), `grounded`, `house-rules`.
  - On match: `debugging`, `code-review`, `security`, `refactor`, `testing`, `performance`, `database`, `api-design`,
    `git-workflow`, `review-pr`, `review-changes`, `review-commits`, `bug-hunt`, `scalability`, `typescript`, `feature-planning`.
  - Skills are read from `~/.mew/skills` first, then Claude Code's, `agents` and pi's folders. Turn one off with `disabled_skills`.
- **Subagents** are declared in a profile (`[[subagents]]`: name, description, goal_prompt, allowed_tools, role). `role` picks a
  routing chain, so exploration runs cheap and debugging runs strong. Sub-agents start no MCP servers, can't spawn more, and cap
  answers at 6000 chars.
- **Repo profiles** (`profiles/<name>.toml`): per-project instructions, `forbidden_paths`, `forbidden_commands`, `verify_rules`,
  routing, enforced in the tool layer. Auto-selected by path, or `profile = "name"`.

## Interactive UI

A full-screen terminal UI with its own look, not a clone.

- Scrollable conversation, a side panel (only the sections that have something) for files read / written (lines added and removed) /
  commands run, a todo checklist, an agents panel, and a live status line with phase, elapsed time and tokens.
- Streamed markdown, syntax-highlighted tinted diffs, in-app approvals that disappear once answered. A turn ends with one line
  like `Worked for 11s · ↓ 0.3k tokens`; the details live in `/cost`.
- `ask_user`: the model asks 1 to 4 questions with 2 to 4 options each, and you answer with one key (or type your own), instead of
  prose with lettered options.
- The input box grows with wrapped text, then scrolls. History (Up / Ctrl+R), `/` completion, `@path` attachment, `!cmd` shows
  output to the model (`!!cmd` keeps it private), Shift+Enter for a newline, **Ctrl+C interrupts the agent, not the app**.
- Mouse selection (drag, double-click word, triple-click line, drag at the edge to scroll) copies to the system clipboard and via OSC 52.
- Queued messages say they are queued. `MEW_PLAIN=1` keeps the classic line layout; `MEW_FULLSCREEN=1` opts into the panel layout
  where the default is the plain terminal.
- **Resume anything.** Every turn autosaves. Exit prints `mew --resume <id>` and `mew --continue`. `/sessions` lists them.

Commands: `/help /clear /compact /context /cost /model /effort /approval /skills /expand /config /stats /tools /mcp /save /sessions
/resume /checkpoint /restore /undo /jobs /copy /learn /lessons /forget /exit`, plus your own (below).

## Extending: MCP, hooks, commands

- **MCP:** stdio and HTTP/SSE servers; their tools appear beside the built-ins. Gated by project trust.
- **Hooks:** `before_agent`, `after_agent`, `before_tool`, `after_tool`, `on_error`, `pre_compact`, `post_compact`, `session_start`,
  `stop` (exit 2 sends stderr back and keeps the agent working), `subagent_stop`. Gated by project trust.
- **Custom slash commands:** markdown files in `.claude/commands` and `.ai-agent/commands` (repo and home); folders become
  `name:sub`, descriptions come from front matter and show in tab completion. Prompt templates support `$1`, `$@`, `${1:-default}`.
- **Built-in tools:** `read_file`, `write_file`, `edit_file` (multi-edit, fuzzy match, CRLF-preserving), `list_dir`, `glob`, `grep`,
  `search`, `outline`, `git`, `graph`, `shell`, `web_search`, `web_fetch`, `memory`, `notes`, `todo`, `ask_user`, `advisor`,
  `recall_result`, `use_skill`, `parallel_tasks`.

## Tests and benchmarks

```bash
.venv/bin/python -m pytest -q                       # 449 offline unit tests
.venv/bin/python -m bench.run --model claude-haiku-5-5 --runs 3 --label mine
.venv/bin/python -m bench.run --mode parallel --model deepseek/deepseek-v4.1-flash --jobs 1 --runs 3
.venv/bin/python -m bench.run --summarize bench/results/*.json
.venv/bin/python -m bench.context_replay            # replay a recorded session through pruning/compaction, no API calls
.venv/bin/python -m bench.review_bench              # planted-bug review benchmark
.venv/bin/python -m bench.tunyt_tasks --clone <scratch clone> --out <dir> --model <model> --tasks <name>
```

Benchmarks hit the real API (except the context replay). Runs that can't execute (plan, quota, key) are recorded as `blocked`, not as
failures. Real-repo tasks run only in a scratch clone, never in your working copy. Traces: one JSONL per session under the user
data dir (`traces/`).

## Not there yet

Said plainly, so you know what you are getting:

- No image input (pasting a screenshot).
- No structured JSON output for CI.
- The OS sandbox is macOS only; on Linux you get the confined tools and shell path checks, not the kernel layer.
- No rewind to an earlier message (`/undo` reverts the last request), no LSP diagnostics, no IDE extension.
- Several results above are tuned on one real repo; treat the defaults as measured there, not universal.

### On "better than the others"

The tables compare *design choices*, not leaderboard scores. No head-to-head benchmark against other harnesses has been run, so none
is claimed here. The harness ships its own bench so you can measure it on your tasks, with your models.
