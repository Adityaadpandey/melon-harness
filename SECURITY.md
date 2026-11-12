# Security policy

## Reporting a vulnerability

Please don't open a public issue. Use GitHub's private reporting instead: **Security tab → Report a vulnerability** on this repo.

Include what you found, how to reproduce it, and which version (`zerd --version`). You will get a first reply within a few days.

## What counts

zerd runs shell commands and edits files on your machine, so these matter most:

- getting around the approval prompts, the sandbox or the workspace boundary
- a repo's `.zerd/config.toml` running code or leaking API keys without `--trust`
- API keys written to logs, traces or transcripts
- the installers at zerd.si serving something other than the release binaries

## Supported versions

Only the latest release gets fixes.
