# Contributing to zerd

Thanks for helping. A few things make a change easy to merge.

## Before you start

- Small fix or typo: just open a pull request.
- New feature or behaviour change: open an issue first so we agree on the shape before you write code.
- Found a security problem? Don't open an issue, see [SECURITY.md](SECURITY.md).

## Build and test

```sh
cargo build
cargo test        # offline, no API keys needed
```

The tests run without network access, so they should pass on a fresh checkout. If you add behaviour, add a test next to the
ones in `src/tests/` that fails without your change.

## Pull requests

- Keep one change per PR. A bug fix and a refactor are two PRs.
- Commit in small steps, each one saying what it changed and why (`loop: wrap up with what is known when time runs out`).
- Comments should read like a person explaining to a teammate, with a short example when the logic is not obvious.
- Run `cargo test` before pushing. CI runs it on Linux, macOS and Windows.
- Don't commit secrets, `.env` files or your local `.zerd/` folder.

## Where things live

| Folder | What is in it |
|--------|---------------|
| `src/` | the Rust CLI (`agent/`, `client/`, `tools/`, `ui/`, `safety/`, `config/`) |
| `skills/`, `profiles/` | built-in skills and project profiles |
| `docs/` | long-form docs and images |
| `scripts/` | the install scripts served from zerd.si |
| `website/` | the Next.js site for zerd.si; its docs pages are built from `README.md` (`npm run sync`), so edit the README, not the generated pages |

## License

By contributing you agree your work is covered by the project's [LICENSE](LICENSE).
