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
