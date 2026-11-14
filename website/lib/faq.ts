// The home page FAQ, shared by the page, its FAQPage schema and llms.txt so the three never drift apart.
export const FAQ = [
  ["What is zerd?", "A terminal coding agent that runs checks on the files it changed, has a second model review the real diff, and verifies every file path and line it mentions against your tree."],
  ["How do I install zerd?", "On macOS or Linux run: curl -fsSL https://zerd.si/install.sh | sh. On Windows PowerShell run: irm https://zerd.si/install.ps1 | iex. It is a single binary with no runtime to set up."],
  ["Which models does zerd work with?", "Claude, GPT, Codex, DeepSeek and local models. If a provider's quota runs out, zerd fails over to the next one and tells you."],
  ["How is zerd different from other coding agents?", "Instead of saying “done”, it reports the checks that ran, a review of the actual diff, and which claims were verified. Safety rules are enforced in code (confined tools and an OS sandbox), not just in the prompt."],
  ["Is zerd free and open source?", "It is free to run, including for commercial use. The source is on GitHub but it is source-available, not open source: you can read it, but not copy, modify or redistribute it."],
  ["Is zerd an alternative to Claude Code, Aider or Codex CLI?", "It does the same job, an AI coding agent in your terminal, but it ends every run with a receipt: which checks ran, what a second model found in the real diff, and which claims were verified. It also works with Claude, GPT, DeepSeek and local models, so you are not tied to one provider. In our webhook test it ran against real PostgreSQL and Redis where Claude Code used mocks; that is one project and one run each, so read it as depth, not a ranking."],
  ["Does zerd run on Windows?", "Yes, on Windows 10 and 11 in PowerShell, with Git for Windows installed. The OS sandbox is macOS only for now; Linux and Windows still get confined file tools and shell path checks."],
  ["Can I undo what zerd changed?", "Yes. Type /undo and every file goes back to how it was before the run."],
] as const;
