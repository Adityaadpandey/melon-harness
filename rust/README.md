# melon

A coding agent for real repositories, in one native binary. It reads and edits code, runs commands and tests,
reviews its own changes, and keeps a memory of what it learned in each project.

Default model: `MiniMaxAI/MiniMax-M3`. Switch any time with `-m <model>` or `/model`.

## Build

    cd rust
    cargo build --release                     # binary: target/release/melon
    ln -sf "$PWD/target/release/melon" ~/.local/bin/melon-rs   # optional: run it from any repo

The release profile uses fat LTO, one codegen unit and a stripped binary (about 10 MB, starts in a few milliseconds).

## Use

    cd ~/your/repo
    melon-rs                                  # full-screen session
    melon-rs "fix the failing test"           # one request, then exit
    melon-rs --json "..."                     # events as JSON lines
    melon-rs --rpc                            # JSON commands on stdin, events on stdout (editors, scripts)
    melon-rs --resume <id>   |   --continue   # pick a saved session back up
    melon-rs --parallel "..."                 # a planner splits the work across parallel workers
    MELON_PLAIN=1 melon-rs                    # plain scrolling terminal instead of full-screen

Useful flags: `-m <model>`, `--approval on-request|auto-edit|auto|plan|yolo`, `--effort low|medium|high`,
`--trust` (trust this project's `.melon/config.toml`), `--add-dir <path>`, `--challenge`.

In the full-screen session: Shift+Tab cycles the approval mode, Ctrl+C stops the turn, anything typed while it works
steers the next step, and the mouse selects and copies from the transcript. `/` opens the command menu.

Other commands:

    melon-rs learn-import --repo .            # mine past Claude Code sessions into edit history and lessons
    melon-rs probe-models                     # check which of your configured models answer

## Configuration

- Keys: environment variables or melon's own `.env` (`/connect <provider>` saves one). `API_KEY` and `BASE_URL` set the
  default gateway; direct providers use `-m groq:<model>`, `-m openrouter:<model>` and so on.
- Config: `~/Library/Application Support/melon/config.toml` (Linux: `~/.local/share/melon`), then the project's
  `.melon/config.toml`, then flags. Set `MELON_DATA_DIR` to move all of melon's data elsewhere.
- Project instructions: `AGENTS.md` / `CLAUDE.md` in the repo are read automatically.
- Project tools: each `.melon/tools/*.toml` becomes a tool that runs a shell command. Arguments arrive as JSON on stdin
  and as `MELON_ARG_<NAME>` environment variables. See `src/tools/discovery.rs` for the format.

Example `.melon/tools/lint.toml`:

    name = "lint"
    description = "Run the linter on one file"
    command = "ruff check \"$MELON_ARG_PATH\""
    kind = "read"                             # or "shell" to ask for approval first
    [parameters.path]
    type = "string"
    description = "File to check"

## Tests

    cargo test                                # about 500 tests, no network, no real model, around 20 seconds

Every test uses a scripted model (`src/tests/fakes.rs`) and a throwaway data folder, so nothing touches your real
sessions or keys and no API credits are spent. Tests are grouped by area under `src/tests/`:
`core` (agent loop, approvals, compaction, cost), `tools` (search, edit, workspace, tasks), `client` (providers, routing,
skills, trust) and `review` (review pipeline, orchestrator, modes, full-screen helpers).

## Benchmarks

    python -m bench.run --bin rust/target/release/melon --label rust --model <id>

Benchmarks call real models and cost money; run them deliberately.

## Limits

- Token counts are estimated (bytes / 4) rather than counted exactly.
- MCP servers: stdio and streamable HTTP.
- Outlines and signature checks for Python files use an indentation-based reader.
