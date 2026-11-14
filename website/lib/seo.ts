import type { Metadata } from "next";
import { NAME, SITE } from "./site";

// Hand-tuned search copy for each docs page, keyed by slug.
// `title` is what shows in the browser tab and on Google (the layout adds " · zerd"), so it leads with
// the words people actually type, like "AI code review in the terminal", not our internal page name.
// `description` is the snippet under the link: keep it around 120 to 158 characters.
type Seo = { title: string; description: string; keywords?: string[] };

export const DOC_SEO: Record<string, Seo> = {
  quickstart: {
    title: "Install zerd: terminal AI coding agent quickstart",
    description: "Install zerd with one command, connect Claude, GPT, DeepSeek or a local model, and make your first checked code change in about two minutes.",
    keywords: ["install AI coding agent", "terminal coding agent setup", "zerd install"],
  },
  "how-a-run-works": {
    title: "How a coding agent run works, step by step",
    description: "What zerd does between your request and the final status: routing, edits, checks, review, and the five ways a run can end.",
    keywords: ["how coding agents work", "agentic coding loop", "AI agent run lifecycle"],
  },
  modes: {
    title: "Pi mode vs full mode: lean or complete setup",
    description: "zerd starts in pi mode with fewer tools and a short prompt to keep requests cheap. Switch to full mode for cheaper and open models.",
    keywords: ["coding agent modes", "lean agent prompt", "reduce AI coding cost"],
  },
  "everyday-use": {
    title: "Using zerd daily: shortcuts, approvals, resume",
    description: "The full-screen terminal UI, what you can type, keyboard shortcuts, approving edits, steering a run mid-flight, and resuming a session later.",
    keywords: ["terminal coding agent shortcuts", "resume AI coding session"],
  },
  models: {
    title: "Use Claude, GPT, DeepSeek or local models",
    description: "Connect a provider, pick a model, and see how zerd routes each request, fails over when quota runs out, and keeps cheaper models on track.",
    keywords: ["multi-model coding agent", "Claude GPT DeepSeek CLI", "local model coding agent", "model failover"],
  },
  safety: {
    title: "AI coding agent safety: sandbox and shell scanning",
    description: "How zerd keeps an AI agent inside your project: path confinement, scanned shell commands, a macOS OS sandbox, and trusted-only project config.",
    keywords: ["AI coding agent sandbox", "safe coding agent", "agent shell command safety"],
  },
  permissions: {
    title: "Approval modes and permission rules for AI agents",
    description: "Control what an AI coding agent may do without asking: approval modes, allow, deny and ask rules, and how each tool call is decided.",
    keywords: ["AI agent permissions", "coding agent approval mode"],
  },
  reviews: {
    title: "AI code review in the terminal: diffs and PRs",
    description: "Ask zerd to review your changes, a PR or recent commits. A second model reads the real diff and a verifier tries to disprove each finding.",
    keywords: ["AI code review CLI", "automated PR review", "AI diff review", "code review agent"],
  },
  "memory-and-undo": {
    title: "Agent memory and /undo: revert AI code changes",
    description: "zerd learns your preferences from corrections without any setup, and /undo puts every file back the way it was before the run.",
    keywords: ["undo AI code changes", "coding agent memory", "revert agent edits"],
  },
  "code-graph": {
    title: "Code graph: callers and imports without grepping",
    description: "The symbol and import graph zerd builds from your working tree, so the model can ask who calls a function instead of searching the whole repo.",
    keywords: ["code graph", "find callers", "symbol graph for AI agents", "codebase navigation"],
  },
  verification: {
    title: "How zerd verifies AI code changes",
    description: "How zerd picks which tests and checks to run, tells whose failure it is, and catches breakage that your tests can't see.",
    keywords: ["verify AI generated code", "AI code verification", "agent runs tests", "trust AI code changes"],
  },
  "context-and-cost": {
    title: "Cut AI coding cost: context and token caching",
    description: "How zerd finds code without reading everything, avoids paying twice for the same tokens, and estimates how many rounds a task needs.",
    keywords: ["reduce LLM token cost", "AI coding agent cost", "prompt caching coding agent", "context window management"],
  },
  cli: {
    title: "zerd CLI reference: flags, subcommands, exit codes",
    description: "Complete reference for the zerd command: options, subcommands, exit codes and examples for interactive and one-shot runs.",
    keywords: ["zerd CLI", "coding agent command line flags"],
  },
  commands: {
    title: "Slash commands reference for zerd sessions",
    description: "Every /command you can type inside an interactive zerd session, grouped by what it does, with an example for each.",
    keywords: ["slash commands", "coding agent commands"],
  },
  configuration: {
    title: "zerd configuration: config.toml and API keys",
    description: "Where zerd keeps config, API keys and data, which layer wins when settings conflict, and the flags you will actually use.",
    keywords: ["config.toml", "coding agent configuration", "API key setup"],
  },
  "files-and-env": {
    title: "zerd files and environment variables reference",
    description: "Every folder and file zerd reads or writes, and every environment variable it looks at, per operating system.",
    keywords: ["environment variables", "zerd data folder"],
  },
  tools: {
    title: "Agent tools reference: read_file, search, graph",
    description: "The tools zerd's model can call, and which are available in pi mode and in full mode, with what each one does.",
    keywords: ["AI agent tools", "coding agent tool list"],
  },
  "skills-and-agents": {
    title: "Skills and sub-agents for a terminal coding agent",
    description: "Write markdown skills the model can load, reusable slash commands, and sub-agents zerd can hand work to.",
    keywords: ["agent skills", "sub-agents", "custom slash commands", "Claude Code skills alternative"],
  },
  extending: {
    title: "Extend zerd: MCP servers, hooks, custom tools",
    description: "Add tools from MCP servers, run your own commands at points in a run, expose a shell command as a tool, and change zerd's instructions.",
    keywords: ["MCP servers", "agent hooks", "custom agent tools", "extend coding agent"],
  },
  troubleshooting: {
    title: "zerd troubleshooting: doctor and common errors",
    description: "What zerd doctor checks, plus the cause and the fix for the errors and odd run endings you are most likely to hit.",
    keywords: ["zerd doctor", "coding agent errors", "troubleshoot AI CLI"],
  },
  windows: {
    title: "Install and run zerd on Windows 10 and 11",
    description: "How zerd runs on Windows 10 and 11 with PowerShell and Git for Windows, and what differs from macOS and Linux.",
    keywords: ["AI coding agent Windows", "PowerShell coding agent", "install on Windows"],
  },
  benchmarks: {
    title: "zerd vs Claude Code benchmarks and caveats",
    description: "Two head-to-head rounds against Claude Code: a payment-webhook service attacked with 15 scenarios, and 11 mixed tasks.",
    keywords: ["zerd vs Claude Code", "coding agent benchmark", "Claude Code alternative"],
  },
  limits: {
    title: "Limits of zerd: known gaps, said plainly",
    description: "Where zerd is not there yet: sandbox coverage by OS, language support, token estimates, and how small the benchmarks are.",
    keywords: ["coding agent limitations"],
  },
  license: {
    title: "zerd license: source-available, free to run",
    description: "zerd is source-available: free to run for any purpose, commercial use included, but you cannot copy, modify or redistribute the source.",
    keywords: ["source-available license", "free coding agent"],
  },
};

// The full set of tags for one page. Next replaces `openGraph` and `twitter` wholesale when a page sets
// them, so a page that only passed title and description would silently lose the card type and site name.
// Building the whole object here keeps every page complete.
export function pageMeta(o: { path: string; title: string; description: string; keywords?: string[]; type?: "website" | "article"; image?: string }): Metadata {
  const { path, title, description, keywords, type = "website", image } = o;
  const images = image ? [{ url: image, width: 1200, height: 630, alt: title }] : undefined;
  return {
    title,
    description,
    keywords,
    alternates: { canonical: path },
    openGraph: { type, siteName: NAME, locale: "en_US", url: path, title: `${title} · ${NAME}`, description, images },
    twitter: { card: "summary_large_image", title: `${title} · ${NAME}`, description, images },
  };
}

export const absolute = (path: string) => `${SITE}${path}`;
