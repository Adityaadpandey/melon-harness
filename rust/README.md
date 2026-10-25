# melon (Rust)

The same agent as the Python version, as one binary: no interpreter, instant start.

    cd rust && cargo build --release        # binary: rust/target/release/melon
    ln -sf "$PWD/target/release/melon" ~/.local/bin/melon-rs   # optional, so you can run it from any repo

## Try it in a repo

It reads the same config and keys as the Python build (`~/Library/Application Support/melon`, `.env`, `.melon/config.toml`),
so nothing to set up if the Python one already works for you.

    cd ~/your/repo
    melon-rs                                 # full-screen session (MELON_PLAIN=1 for the plain scrolling terminal)
    melon-rs "fix the failing test"          # one request, then exit
    melon-rs --json "..."  |  --rpc          # machine-readable modes
    melon-rs --resume <id>  |  --continue    # pick a session back up

Checks worth doing side by side with `python main.py` on the same small task: the edit it makes, the verify step after it,
`/cost`, `/undo`, `/diff`, an approval prompt (run with `--approval on-request`), and Ctrl+C mid-turn.

Other commands: `melon-rs learn-import --repo .` (mine Claude Code sessions), `melon-rs probe-models`.
Benchmarks: `python -m bench.run --bin rust/target/release/melon --label rust --model <id>`.

## Differences from Python

- Project tools are `.melon/tools/*.toml` running a shell command (see `src/tools/discovery.rs`), not Python classes.
- Token counts are estimated (bytes / 4), and Python outlines/signature checks use an indentation-based reader.
- MCP: stdio and streamable HTTP only.
