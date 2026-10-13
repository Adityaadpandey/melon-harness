# AI Agent

An AI agent that can execute tasks using tools and manage conversations.

## Features

### Core Functionality

- Interactive and single-run modes
- Streaming text responses
- Multi-turn conversations with tool calling
- Configurable model settings and temperature

### Built-in Tools

- File operations: read, write, edit files
- Directory operations: list directories, search with glob patterns
- Text search: grep for pattern matching
- Shell execution: run shell commands
- Web access: search and fetch web content
- Memory: store and retrieve information
- Todo: manage task lists

### Context Management

- Automatic context compression when approaching token limits
- Tool output pruning to manage context size
- Token usage tracking

### Safety and Approval

- Multiple approval policies: on-request, auto, never, yolo
- Dangerous command detection and blocking
- Path-based safety checks
- User confirmation prompts for mutating operations

### Session Management

- Save and resume sessions
- Create checkpoints
- Persistent session storage

### MCP Integration

- Connect to Model Context Protocol servers
- Use tools from MCP servers
- Support for stdio and HTTP/SSE transports

### Subagents

- Specialized subagents for specific tasks
- Built-in subagents: codebase investigator, code reviewer
- Configurable subagent definitions with custom tools and limits

### Loop Detection

- Detects repeating actions
- Prevents infinite loops in agent execution

### Hooks System

- Execute scripts before/after agent runs
- Execute scripts before/after tool calls
- Error handling hooks
- Custom commands and scripts

### Configuration

- Configurable working directory
- Tool allowlisting
- Developer and user instructions
- Shell environment policies
- MCP server configuration

### User Interface

- Terminal UI with formatted output
- Command interface: /help, /config, /tools, /mcp, /stats, /save, /resume, /checkpoint, /restore
- Real-time tool call visualization

## Setup

```bash
uv venv .venv && uv pip install --python .venv/bin/python -r requirements.txt
echo 'API_KEY=...' > .env            # BASE_URL defaults to https://api.commandcode.ai/provider/v1
.venv/bin/python main.py -m claude-haiku-5-5 --approval auto "your task"
```

## Models and routing

- Claude models are called via `/messages`, everything else via `/chat/completions` (picked from the live `/models` catalog, cached 24h).
- `python scripts/probe_models.py --json out.json` shows which models your key can really call.
- Per-role fallback chains in `.ai-agent/config.toml`:

```toml
[routing]
default   = ["claude-sonnet-5-5", "deepseek/deepseek-v4-pro"]  # tried after the main model
summarize = ["deepseek/deepseek-v4-flash"]                      # context compaction
review    = ["gpt-5.6-sol"]
```

Fallbacks are announced in the output, never silent. A model that returns not-in-plan/not-found is skipped for an hour.

## Verification

After a run that changes files, checks must pass before the result is `verified`. Set `verify_commands` or `[[verify_rules]]`
(path-prefix -> commands, `{files}` = changed files); otherwise pytest/go/cargo are auto-detected. Failures go back to the model
as a repair request (`max_repairs`, default 2). Final status: `verified | unverified | verification_failed | failed | done_no_changes`.

## Parallel mode

`--parallel` (model plans the split) or `--graph tasks.json`. Each task runs in its own git worktree, results merge in dependency
order into branch `mew/<run>/integration`; real conflicts are re-run on top of the merged state and flagged for review.
`--apply` (default) fast-forwards your checkout only when it is clean. Measure before using: it only pays off for work that is
genuinely independent and not tiny.

## Repo profiles

`profiles/<name>.toml` holds per-project rules (instructions, `forbidden_paths`, `forbidden_commands`, `verify_rules`, routing),
auto-selected by a path component in `match`, or via `profile = "name"` in project config. Forbidden paths/commands are enforced
in the tool layer, not just requested of the model. Project `.ai-agent/config.toml` overrides the profile.

## Tests and benchmarks

```bash
.venv/bin/python -m pytest -q                       # offline unit tests
.venv/bin/python -m bench.run --model claude-haiku-5-5 --runs 3 --label mine
.venv/bin/python -m bench.run --mode parallel --model deepseek/deepseek-v4-flash --jobs 1 --runs 3
.venv/bin/python -m bench.run --summarize bench/results/*.json
```

Benchmarks hit the real API. Runs that cannot execute (plan, quota, key) are recorded as `blocked`, not as failures. Traces: one
JSONL per session under the user data dir (`traces/`).

## Skills, subagents, review (what makes it "smart" on a known repo)

- **Skills** are short curated playbooks (`profiles/<name>/skills/*.md`, or `.ai-agent/skills/*.md` in a repo) with `triggers`
  and an optional `canonical:` long doc. Matching playbooks are attached to the request automatically (keyword match, no model
  call); the model can pull any other with `use_skill(name)` or a single section of the full doc with `use_skill(name, section=...)`.
- **Subagents** can be declared in a profile (`[[subagents]]`: name, description, goal_prompt, allowed_tools, role). `role` picks a
  `[routing]` chain, so exploration can run on a cheap model and debugging on a strong one.
- **Independent review** (`auto_review = true`): after checks pass, a read-only reviewer on a different model reads the real diff;
  concrete findings go back for one fix, and the verdict is kept in the final report either way.

## Cost controls

System prompt ~0.7k tokens (was ~3k). Tool output is capped (`tool_output_max_chars`), unchanged re-reads of a file are
deduplicated, Claude calls cache both the system prompt and the conversation prefix, `max_total_tokens` stops runaway runs, and the
result line shows tokens, cache hit % and (if you fill `[pricing."model-id"]` with input/output/cached_input per 1M tokens) an
estimated cost. Plan-wide usage limits (HTTP 429 "usage limit") are not retried and not failed over.

## Interactive UI

`python main.py` opens a Claude Code style session: `⏺ Tool(args)` lines with a `⎿` result under each, compact numbered diffs
for edits, a spinner with the running tool, and a footer showing model, context %, tokens, cache hit rate, approval mode.

- **Input:** history (Up / Ctrl+R), `/` command completion, `@path` file completion and attachment, `!cmd` runs a command and
  shows the output to the model (`!!cmd` keeps it private), Alt+Enter for a newline, **Ctrl+C interrupts the agent** (not the app).
- **Commands:** `/help /clear /compact /cost /model /approval /skills /expand /config /stats /tools /mcp /save /sessions /resume /exit`.
- **Approvals:** edits ask by default with a diff preview and three choices: yes / yes and don't ask again this session /
  no, and tell me what to do differently (your reply goes to the model). Modes: `on-request` (default), `auto-edit`
  (edits pre-approved, commands still ask), `auto`, `never`, `yolo`. Non-interactive runs can't ask, so pass `--approval auto-edit`.

## Search and edits

`grep` and `glob` respect `.gitignore`, find dotfiles, never touch secrets (`.env*`, keys), and support `glob`, `context`, `limit`,
`literal` and `files_only`; smart-case by default. `edit` takes several replacements in one call (`edits`), tolerates trailing
whitespace and smart-quote differences, keeps CRLF files CRLF, and reports every location when a match is not unique. What you
approve is exactly what gets written (preview and write share one planner).

## Context and cost

Compaction keeps the most recent ~20k tokens verbatim and folds only older messages into a running summary (plus a deterministic
list of files read/modified). `/cost` and the result line report cache misses with a reason (idle gap, model changed, prefix changed).

## Project trust

A repo's `.ai-agent/config.toml` cannot enable `hooks`, `mcp_servers`, `verify_commands`, `verify_rules`, `approval` or
`shell_environment` until you trust that exact file (interactive prompt, or `--trust`). Editing the file revokes the trust.

## Guardrails: the session stays in its workspace

Three layers, each independent:
1. **File tools** (read, edit, write, list, grep, glob, and the `cwd` of shell) are confined to the folder you started in. Paths are
   resolved first, so `..` and symlinks that point out are refused. Add roots deliberately with `--add-dir`.
2. **Shell commands** are scanned for paths in your home, `/etc`, other users' folders or `..` escapes, and refused before they run.
3. **OS sandbox** (macOS `sandbox-exec`, on by default when available): even a command built dynamically cannot write outside the
   workspace, temp dirs and package caches, or read `~/.ssh`, `~/.aws`, `~/.gnupg`, keychains and similar. `sandbox = "off"` disables it.

Also: `.env*`, `*.pem`, `*.key` and SSH keys are protected by default; `web_fetch` refuses localhost, private ranges and cloud
metadata at every redirect hop; project config cannot loosen any of this until you trust it (`--trust`).

## Not hallucinating

- **Read before edit:** editing an existing file the agent has not read this session is refused, and so is editing a file that
  changed on disk since it was read.
- **Citation check:** every `path` / `path:line` in the final answer is checked against the real tree. Fabricated files or line
  numbers past the end of a file go back for one correction, and anything still unverified is shown as a warning.
- **Always-on `grounded` rule** (only claim what a tool showed you), an independent reviewer on the real diff, and the verification gate.
- **graph tool:** answers structure questions from the graphify graph; call direction is decided from the source text because the graph's
  edge order is unreliable. Results say how stale the graph is.

## Models and parallelism

- **Routing by difficulty:** with `fast` / `strong` chains in `[routing]`, each request is classified (no model call) and sent to the
  right chain; a failed repair or a detected loop escalates to `strong`. `-m` or `/model` pins a model.
- **Parallel sub-agents:** read-only sub-agents called in the same turn run concurrently (cap `max_parallel_agents`, default 4) and
  show live progress. `parallel_tasks` fans write-capable workers out into separate git worktrees, merges them in dependency order,
  and reports conflicts instead of resolving them silently.

## Built-in skills

Always on (about 450 tokens): `ponytail` (do the least that fully works), `grounded`, `house-rules`. Loaded when the request matches:
`debugging`, `code-review`, `security`, `refactor`, `testing`, `performance`, `database`, `api-design`, `git-workflow`. A profile's
own playbooks (e.g. `tunyt-debugging`) replace the generic one they specialise. Turn one off with `disabled_skills = ["ponytail"]`.
